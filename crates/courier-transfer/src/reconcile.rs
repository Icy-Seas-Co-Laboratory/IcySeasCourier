use courier_core::{FileRecord, TransferStore};

use crate::{MultipartStore, RemotePart, StoreError, UploadSession};

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Local(#[from] courier_core::CourierError),
    #[error(transparent)]
    Remote(#[from] StoreError),
    #[error("remote upload contains unexpected part {0}")]
    UnexpectedRemotePart(u32),
    #[error("remote part {part_number} has {remote_size} bytes; local plan expects {planned_size}")]
    RemotePartSizeMismatch {
        part_number: u32,
        remote_size: u64,
        planned_size: u64,
    },
}

/// Starts or resumes one file upload, then makes local confirmed-part state
/// match the object store. Remote state is authoritative because a process can
/// die after the object store accepts bytes but before SQLite commits the ETag.
pub async fn reconcile_file(
    database: &TransferStore,
    store: &dyn MultipartStore,
    file: &FileRecord,
) -> Result<(UploadSession, Vec<RemotePart>), ReconcileError> {
    let session = match database.upload_session(file.id)? {
        Some((object_key, upload_id)) => UploadSession {
            object_key,
            upload_id,
        },
        None => {
            let object_key = database.object_key(file.id)?.unwrap_or_else(|| {
                format!(
                    "incoming/{}/{}/payload",
                    file.transfer_id.simple(),
                    file.id.simple()
                )
            });
            let session = store.begin(&object_key).await?;
            database.set_upload_session(file.id, &session.object_key, &session.upload_id)?;
            session
        }
    };

    let remote = store.list_parts(&session).await?;
    let planned = database
        .parts_for_file(file.id)?
        .into_iter()
        .map(|part| (part.part_number, part.source_length))
        .collect::<std::collections::HashMap<_, _>>();
    if let Some(part) = remote
        .iter()
        .find(|part| !planned.contains_key(&part.part_number))
    {
        return Err(ReconcileError::UnexpectedRemotePart(part.part_number));
    }
    for part in &remote {
        let planned_size = planned[&part.part_number];
        if part.size != 0 && part.size != planned_size {
            return Err(ReconcileError::RemotePartSizeMismatch {
                part_number: part.part_number,
                remote_size: part.size,
                planned_size,
            });
        }
    }
    let confirmed: Vec<(u32, String)> = remote
        .iter()
        .map(|part| (part.part_number, part.etag.clone()))
        .collect();
    database.reconcile_remote_parts(file.id, &confirmed)?;
    Ok((session, remote))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf, sync::Mutex};

    use async_trait::async_trait;
    use courier_core::{FileStatus, Transfer};
    use uuid::Uuid;

    use super::*;
    use crate::{MultipartLimits, plan_parts};

    #[derive(Default)]
    struct FakeStore {
        parts: Mutex<HashMap<String, Vec<RemotePart>>>,
    }

    #[async_trait]
    impl MultipartStore for FakeStore {
        async fn begin(&self, object_key: &str) -> Result<UploadSession, StoreError> {
            Ok(UploadSession {
                object_key: object_key.into(),
                upload_id: "upload-1".into(),
            })
        }
        async fn list_parts(&self, session: &UploadSession) -> Result<Vec<RemotePart>, StoreError> {
            Ok(self
                .parts
                .lock()
                .unwrap()
                .get(&session.upload_id)
                .cloned()
                .unwrap_or_default())
        }
        async fn upload_part(
            &self,
            _session: &UploadSession,
            part_number: u32,
            bytes: Vec<u8>,
        ) -> Result<RemotePart, StoreError> {
            Ok(RemotePart {
                part_number,
                etag: format!("etag-{part_number}"),
                size: bytes.len() as u64,
            })
        }
        async fn complete(
            &self,
            _session: &UploadSession,
            _parts: &[RemotePart],
        ) -> Result<(), StoreError> {
            Ok(())
        }
        async fn object_exists(&self, _object_key: &str) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn abort(&self, _session: &UploadSession) -> Result<(), StoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn remote_confirmation_wins_after_ambiguous_crash() {
        let mut database = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(PathBuf::from("/tmp"), None);
        database.create_transfer(&transfer).unwrap();
        let file = FileRecord {
            id: Uuid::new_v4(),
            transfer_id: transfer.id,
            relative_path: "data.bin".into(),
            absolute_path: "/tmp/data.bin".into(),
            size: 10,
            mtime_ns: 1,
            hash_algorithm: courier_core::HashAlgorithm::Sha256,
            sha256: "0".repeat(64),
            status: FileStatus::Ready,
            bytes_completed: 0,
        };
        database
            .replace_inventory(transfer.id, std::slice::from_ref(&file))
            .unwrap();
        database
            .replace_part_plan(
                file.id,
                &plan_parts(
                    file.id,
                    file.size,
                    MultipartLimits {
                        target_part_size: 5,
                        minimum_part_size: 5,
                        maximum_part_size: 10,
                        maximum_parts: 10,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        database
            .set_upload_session(file.id, "key", "upload-1")
            .unwrap();

        let store = FakeStore::default();
        store.parts.lock().unwrap().insert(
            "upload-1".into(),
            vec![RemotePart {
                part_number: 1,
                etag: "accepted-before-crash".into(),
                size: 5,
            }],
        );
        reconcile_file(&database, &store, &file).await.unwrap();

        let parts = database.parts_for_file(file.id).unwrap();
        assert_eq!(parts[0].etag.as_deref(), Some("accepted-before-crash"));
        assert_eq!(parts[0].status, courier_core::PartStatus::Complete);
        assert_eq!(parts[1].status, courier_core::PartStatus::Pending);

        store.parts.lock().unwrap().insert(
            "upload-1".into(),
            vec![RemotePart {
                part_number: 1,
                etag: "unexpected-size".into(),
                size: 6,
            }],
        );
        assert!(matches!(
            reconcile_file(&database, &store, &file).await,
            Err(ReconcileError::RemotePartSizeMismatch {
                part_number: 1,
                remote_size: 6,
                planned_size: 5,
            })
        ));
    }
}
