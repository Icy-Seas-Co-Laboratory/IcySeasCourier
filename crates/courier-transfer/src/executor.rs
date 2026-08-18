use std::io::ErrorKind;

use courier_core::{
    CourierError, FileRecord, PartStatus, RetryPolicy, TransferStore, verify_source_unchanged,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{
    MultipartStore, ReconcileError, RemotePart, StoreError, UploadSession, reconcile_file,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadProgress {
    pub parts_uploaded: u32,
    pub parts_already_present: u32,
    pub source_bytes_confirmed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartUploadEvent {
    pub part_number: u32,
    pub source_bytes: u64,
}

pub trait UploadObserver: Send + Sync {
    fn should_pause(&self) -> bool {
        false
    }

    fn part_confirmed(&self, _event: PartUploadEvent) {}

    fn reconciled(&self, _source_bytes_confirmed: u64) {}
}

struct NoopObserver;
impl UploadObserver for NoopObserver {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    Completed,
    AlreadyComplete,
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error(transparent)]
    Local(#[from] CourierError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    #[error(transparent)]
    Remote(#[from] StoreError),
    #[error("part {part_number} exhausted {attempts} attempts: {source}")]
    RetriesExhausted {
        part_number: u32,
        attempts: u32,
        #[source]
        source: StoreError,
    },
    #[error("part {part_number} is too large for this process: {length} bytes")]
    PartTooLarge { part_number: u32, length: u64 },
    #[error("retry policy must allow at least one attempt")]
    InvalidRetryPolicy,
    #[error("file has {0} locally incomplete parts")]
    IncompleteParts(usize),
    #[error("multipart upload session is missing from local state")]
    MissingUploadSession,
    #[error("confirmed part {0} is missing its ETag")]
    MissingEtag(u32),
    #[error("upload paused")]
    Paused,
}

/// Uploads only parts the object store has not already confirmed. Completion
/// is deliberately separate: resolving an ambiguous CompleteMultipartUpload
/// response requires an object-existence check supplied by the concrete store.
pub async fn upload_missing_parts(
    database: &TransferStore,
    store: &dyn MultipartStore,
    file: &FileRecord,
    retry: &RetryPolicy,
) -> Result<UploadProgress, UploadError> {
    upload_missing_parts_observed(database, store, file, retry, &NoopObserver).await
}

pub async fn upload_missing_parts_observed(
    database: &TransferStore,
    store: &dyn MultipartStore,
    file: &FileRecord,
    retry: &RetryPolicy,
    observer: &dyn UploadObserver,
) -> Result<UploadProgress, UploadError> {
    if retry.max_attempts == 0 {
        return Err(UploadError::InvalidRetryPolicy);
    }
    if observer.should_pause() {
        return Err(UploadError::Paused);
    }
    if let Some(object_key) = database.object_key(file.id)?
        && store.object_exists(&object_key).await?
    {
        database.mark_file_uploaded(file.id)?;
        observer.reconciled(file.size);
        return Ok(UploadProgress {
            parts_uploaded: 0,
            parts_already_present: database.parts_for_file(file.id)?.len() as u32,
            source_bytes_confirmed: file.size,
        });
    }
    let (session, existing) = reconcile_with_retry(database, store, file, retry).await?;
    let mut parts = database.parts_for_file(file.id)?;
    observer.reconciled(
        parts
            .iter()
            .filter(|part| part.status == PartStatus::Complete)
            .map(|part| part.source_length)
            .sum(),
    );
    let mut source = tokio::fs::File::open(&file.absolute_path)
        .await
        .map_err(|source| CourierError::Io {
            path: file.absolute_path.clone(),
            source,
        })?;
    let mut uploaded = 0_u32;

    for part in parts
        .iter_mut()
        .filter(|part| part.status != PartStatus::Complete)
    {
        if observer.should_pause() {
            return Err(UploadError::Paused);
        }
        verify_source_unchanged(file)?;
        let length =
            usize::try_from(part.source_length).map_err(|_| UploadError::PartTooLarge {
                part_number: part.part_number,
                length: part.source_length,
            })?;
        source
            .seek(std::io::SeekFrom::Start(part.source_offset))
            .await
            .map_err(|source| CourierError::Io {
                path: file.absolute_path.clone(),
                source,
            })?;
        let mut bytes = vec![0_u8; length];
        source
            .read_exact(&mut bytes)
            .await
            .map_err(|source| CourierError::Io {
                path: file.absolute_path.clone(),
                source: if source.kind() == ErrorKind::UnexpectedEof {
                    std::io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "source shortened during transfer",
                    )
                } else {
                    source
                },
            })?;

        let mut last_error = None;
        for attempt in 0..retry.max_attempts {
            if observer.should_pause() {
                return Err(UploadError::Paused);
            }
            match store
                .upload_part(&session, part.part_number, bytes.clone())
                .await
            {
                Ok(remote) => {
                    database.confirm_part(
                        file.id,
                        part.part_number,
                        &remote.etag,
                        None,
                        remote.size,
                    )?;
                    uploaded += 1;
                    observer.part_confirmed(PartUploadEvent {
                        part_number: part.part_number,
                        source_bytes: part.source_length,
                    });
                    last_error = None;
                    break;
                }
                Err(error) => {
                    database.record_part_failure(file.id, part.part_number, &error.to_string())?;
                    if !error.is_retryable() {
                        return Err(error.into());
                    }
                    last_error = Some(error);
                    if attempt + 1 < retry.max_attempts {
                        tokio::time::sleep(retry.delay(attempt)).await;
                    }
                }
            }
        }
        if let Some(source) = last_error {
            return Err(UploadError::RetriesExhausted {
                part_number: part.part_number,
                attempts: retry.max_attempts,
                source,
            });
        }
    }

    let confirmed = database.parts_for_file(file.id)?;
    Ok(UploadProgress {
        parts_uploaded: uploaded,
        parts_already_present: existing.len() as u32,
        source_bytes_confirmed: confirmed
            .iter()
            .filter(|part| part.status == PartStatus::Complete)
            .map(|part| part.source_length)
            .sum(),
    })
}

async fn reconcile_with_retry(
    database: &TransferStore,
    store: &dyn MultipartStore,
    file: &FileRecord,
    retry: &RetryPolicy,
) -> Result<(UploadSession, Vec<RemotePart>), ReconcileError> {
    for attempt in 0..retry.max_attempts {
        match reconcile_file(database, store, file).await {
            Ok(result) => return Ok(result),
            Err(ReconcileError::Remote(error)) if error.is_retryable() => {
                if attempt + 1 == retry.max_attempts {
                    return Err(ReconcileError::Remote(error));
                }
                tokio::time::sleep(retry.delay(attempt)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("retry policy is validated before reconciliation")
}

/// Completes a fully uploaded file. If the completion request fails after S3
/// accepted it, a HEAD check recognizes the finalized object and avoids
/// restarting or duplicating the upload.
pub async fn complete_uploaded_file(
    database: &TransferStore,
    store: &dyn MultipartStore,
    file: &FileRecord,
    retry: &RetryPolicy,
) -> Result<CompletionOutcome, UploadError> {
    if retry.max_attempts == 0 {
        return Err(UploadError::InvalidRetryPolicy);
    }
    let (object_key, upload_id) = database
        .upload_session(file.id)?
        .ok_or(UploadError::MissingUploadSession)?;
    let session = UploadSession {
        object_key,
        upload_id,
    };
    if store.object_exists(&session.object_key).await? {
        return Ok(CompletionOutcome::AlreadyComplete);
    }
    let local = database.parts_for_file(file.id)?;
    let incomplete = local
        .iter()
        .filter(|part| part.status != PartStatus::Complete)
        .count();
    if incomplete > 0 {
        return Err(UploadError::IncompleteParts(incomplete));
    }
    let parts: Vec<RemotePart> = local
        .iter()
        .map(|part| {
            Ok(RemotePart {
                part_number: part.part_number,
                etag: part
                    .etag
                    .clone()
                    .ok_or(UploadError::MissingEtag(part.part_number))?,
                size: part.transport_length.unwrap_or(part.source_length),
            })
        })
        .collect::<Result<_, UploadError>>()?;

    let mut last_error = None;
    for attempt in 0..retry.max_attempts {
        match store.complete(&session, &parts).await {
            Ok(()) => return Ok(CompletionOutcome::Completed),
            Err(error) => {
                if store.object_exists(&session.object_key).await? {
                    return Ok(CompletionOutcome::AlreadyComplete);
                }
                if !error.is_retryable() && !matches!(error, StoreError::UploadNotFound) {
                    return Err(error.into());
                }
                if matches!(error, StoreError::UploadNotFound) {
                    return Err(error.into());
                }
                last_error = Some(error);
                if attempt + 1 < retry.max_attempts {
                    tokio::time::sleep(retry.delay(attempt)).await;
                }
            }
        }
    }
    Err(UploadError::RetriesExhausted {
        part_number: 0,
        attempts: retry.max_attempts,
        source: last_error.expect("at least one completion attempt"),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicU32, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use courier_core::{FileStatus, Transfer};
    use uuid::Uuid;

    use super::*;
    use crate::{MultipartLimits, RemotePart, UploadSession, plan_parts};

    #[derive(Default)]
    struct FlakyStore {
        attempts: Mutex<HashMap<u32, u32>>,
        existing: Vec<RemotePart>,
    }

    #[derive(Default)]
    struct AmbiguousCompletionStore {
        object_exists: AtomicBool,
    }

    #[derive(Default)]
    struct PauseAfterOnePart {
        confirmed_parts: AtomicU32,
    }

    impl UploadObserver for PauseAfterOnePart {
        fn should_pause(&self) -> bool {
            self.confirmed_parts.load(Ordering::SeqCst) >= 1
        }

        fn part_confirmed(&self, _event: PartUploadEvent) {
            self.confirmed_parts.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl MultipartStore for FlakyStore {
        async fn begin(&self, object_key: &str) -> Result<UploadSession, StoreError> {
            Ok(UploadSession {
                object_key: object_key.into(),
                upload_id: "upload".into(),
            })
        }
        async fn list_parts(
            &self,
            _session: &UploadSession,
        ) -> Result<Vec<RemotePart>, StoreError> {
            Ok(self.existing.clone())
        }
        async fn upload_part(
            &self,
            _session: &UploadSession,
            number: u32,
            bytes: Vec<u8>,
        ) -> Result<RemotePart, StoreError> {
            let mut attempts = self.attempts.lock().unwrap();
            let count = attempts.entry(number).or_default();
            *count += 1;
            if number == 2 && *count == 1 {
                return Err(StoreError::Transient("injected disconnect".into()));
            }
            Ok(RemotePart {
                part_number: number,
                etag: format!("etag-{number}"),
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

    #[async_trait]
    impl MultipartStore for AmbiguousCompletionStore {
        async fn begin(&self, object_key: &str) -> Result<UploadSession, StoreError> {
            Ok(UploadSession {
                object_key: object_key.into(),
                upload_id: "upload".into(),
            })
        }
        async fn list_parts(
            &self,
            _session: &UploadSession,
        ) -> Result<Vec<RemotePart>, StoreError> {
            Ok(Vec::new())
        }
        async fn upload_part(
            &self,
            _session: &UploadSession,
            _number: u32,
            _bytes: Vec<u8>,
        ) -> Result<RemotePart, StoreError> {
            unreachable!()
        }
        async fn complete(
            &self,
            _session: &UploadSession,
            _parts: &[RemotePart],
        ) -> Result<(), StoreError> {
            self.object_exists.store(true, Ordering::SeqCst);
            Err(StoreError::Transient(
                "response lost after acceptance".into(),
            ))
        }
        async fn object_exists(&self, _object_key: &str) -> Result<bool, StoreError> {
            Ok(self.object_exists.load(Ordering::SeqCst))
        }
        async fn abort(&self, _session: &UploadSession) -> Result<(), StoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn skips_remote_parts_and_retries_transient_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        fs::write(&path, b"abcdefghij").unwrap();
        let mut database = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(dir.path().to_path_buf(), None);
        database.create_transfer(&transfer).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let mtime_ns = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let file = FileRecord {
            id: Uuid::new_v4(),
            transfer_id: transfer.id,
            relative_path: PathBuf::from("data.bin"),
            absolute_path: path,
            size: 10,
            mtime_ns,
            hash_algorithm: courier_core::HashAlgorithm::Sha256,
            sha256: "unused".into(),
            status: FileStatus::Ready,
            bytes_completed: 0,
        };
        database
            .replace_inventory(transfer.id, std::slice::from_ref(&file))
            .unwrap();
        let limits = MultipartLimits {
            target_part_size: 5,
            minimum_part_size: 5,
            maximum_part_size: 10,
            maximum_parts: 10,
        };
        database
            .replace_part_plan(file.id, &plan_parts(file.id, file.size, limits).unwrap())
            .unwrap();
        database
            .set_upload_session(file.id, "key", "upload")
            .unwrap();
        let store = FlakyStore {
            existing: vec![RemotePart {
                part_number: 1,
                etag: "existing".into(),
                size: 5,
            }],
            ..Default::default()
        };
        let retry = RetryPolicy {
            base: Duration::ZERO,
            maximum: Duration::ZERO,
            max_attempts: 3,
        };

        let progress = upload_missing_parts(&database, &store, &file, &retry)
            .await
            .unwrap();
        assert_eq!(progress.parts_already_present, 1);
        assert_eq!(progress.parts_uploaded, 1);
        assert_eq!(progress.source_bytes_confirmed, 10);
        assert_eq!(store.attempts.lock().unwrap().get(&1), None);
        assert_eq!(store.attempts.lock().unwrap().get(&2), Some(&2));
    }

    #[tokio::test]
    async fn recognizes_completion_accepted_before_response_loss() {
        let mut database = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(PathBuf::from("/tmp"), None);
        database.create_transfer(&transfer).unwrap();
        let file = FileRecord {
            id: Uuid::new_v4(),
            transfer_id: transfer.id,
            relative_path: "data.bin".into(),
            absolute_path: "/tmp/data.bin".into(),
            size: 5,
            mtime_ns: 1,
            hash_algorithm: courier_core::HashAlgorithm::Sha256,
            sha256: "unused".into(),
            status: FileStatus::Ready,
            bytes_completed: 0,
        };
        database
            .replace_inventory(transfer.id, std::slice::from_ref(&file))
            .unwrap();
        let limits = MultipartLimits {
            target_part_size: 5,
            minimum_part_size: 5,
            maximum_part_size: 10,
            maximum_parts: 10,
        };
        database
            .replace_part_plan(file.id, &plan_parts(file.id, file.size, limits).unwrap())
            .unwrap();
        database
            .set_upload_session(file.id, "key", "upload")
            .unwrap();
        database.confirm_part(file.id, 1, "etag", None, 5).unwrap();
        let retry = RetryPolicy {
            base: Duration::ZERO,
            maximum: Duration::ZERO,
            max_attempts: 2,
        };

        let remote = AmbiguousCompletionStore::default();
        let outcome = complete_uploaded_file(&database, &remote, &file, &retry)
            .await
            .unwrap();
        assert_eq!(outcome, CompletionOutcome::AlreadyComplete);
        let resumed = upload_missing_parts(&database, &remote, &file, &retry)
            .await
            .unwrap();
        assert_eq!(resumed.source_bytes_confirmed, file.size);
        assert_eq!(
            database.files_for_transfer(transfer.id).unwrap()[0].status,
            FileStatus::Uploaded
        );
    }

    #[tokio::test]
    async fn pauses_cooperatively_after_confirming_the_current_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        fs::write(&path, b"abcdefghij").unwrap();
        let mut database = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(dir.path().to_path_buf(), None);
        database.create_transfer(&transfer).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let mtime_ns = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let file = FileRecord {
            id: Uuid::new_v4(),
            transfer_id: transfer.id,
            relative_path: "data.bin".into(),
            absolute_path: path,
            size: 10,
            mtime_ns,
            hash_algorithm: courier_core::HashAlgorithm::Sha256,
            sha256: "unused".into(),
            status: FileStatus::Ready,
            bytes_completed: 0,
        };
        database
            .replace_inventory(transfer.id, std::slice::from_ref(&file))
            .unwrap();
        let limits = MultipartLimits {
            target_part_size: 5,
            minimum_part_size: 5,
            maximum_part_size: 10,
            maximum_parts: 10,
        };
        database
            .replace_part_plan(file.id, &plan_parts(file.id, file.size, limits).unwrap())
            .unwrap();
        let retry = RetryPolicy {
            base: Duration::ZERO,
            maximum: Duration::ZERO,
            max_attempts: 2,
        };
        let observer = PauseAfterOnePart::default();

        let result = upload_missing_parts_observed(
            &database,
            &FlakyStore::default(),
            &file,
            &retry,
            &observer,
        )
        .await;
        assert!(matches!(result, Err(UploadError::Paused)));
        let parts = database.parts_for_file(file.id).unwrap();
        assert_eq!(parts[0].status, PartStatus::Complete);
        assert_eq!(parts[1].status, PartStatus::Pending);
    }
}
