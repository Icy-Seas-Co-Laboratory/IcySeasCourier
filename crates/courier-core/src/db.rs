use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::{
    CourierError, FileRecord, PartRecord, RegistrySessionRecord, Result, Transfer,
    TransportMemberRecord, TransportObjectKind, TransportObjectRecord, model::TransferStatus,
};

pub struct TransferStore {
    conn: Connection,
}

impl TransferStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.conn.pragma_update(None, "foreign_keys", "ON")?;
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch(include_str!("../migrations/001_initial.sql"))?;
        let version: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 2 {
            self.conn
                .execute_batch(include_str!("../migrations/002_upload_sessions.sql"))?;
        }
        if version < 3 {
            self.conn
                .execute_batch(include_str!("../migrations/003_registry_state.sql"))?;
        }
        if version < 4 {
            self.conn.execute_batch(include_str!(
                "../migrations/004_registry_secrets_external.sql"
            ))?;
        }
        if version < 5 {
            self.conn
                .execute_batch(include_str!("../migrations/005_hash_algorithm.sql"))?;
        }
        if version < 6 {
            self.conn
                .execute_batch(include_str!("../migrations/006_manifest_version.sql"))?;
        }
        if version < 7 {
            self.conn
                .execute_batch(include_str!("../migrations/007_transport_objects.sql"))?;
        }
        Ok(())
    }

    pub fn create_transfer(&self, transfer: &Transfer) -> Result<()> {
        self.conn.execute("INSERT INTO transfers (id, server_transfer_id, project_id, source_root, created_at, updated_at, status, file_count, original_bytes, manifest_version) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![transfer.id.to_string(), transfer.server_transfer_id, transfer.project_id, transfer.source_root.to_string_lossy(), transfer.created_at.to_rfc3339(), transfer.updated_at.to_rfc3339(), transfer.status.to_string(), transfer.file_count, transfer.original_bytes, transfer.manifest_version])?;
        Ok(())
    }

    pub fn transition(&self, id: Uuid, next: TransferStatus) -> Result<()> {
        let current = self
            .get_transfer(id)?
            .ok_or_else(|| CourierError::TransferNotFound(id.to_string()))?;
        if !current.status.can_transition_to(next) {
            return Err(CourierError::InvalidTransition {
                from: current.status.to_string(),
                to: next.to_string(),
            });
        }
        self.conn.execute(
            "UPDATE transfers SET status=?1, updated_at=?2 WHERE id=?3",
            params![next.to_string(), Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn replace_inventory(&mut self, transfer_id: Uuid, files: &[FileRecord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM files WHERE transfer_id=?1",
            [transfer_id.to_string()],
        )?;
        let mut bytes = 0_u64;
        for file in files {
            bytes = bytes.saturating_add(file.size);
            tx.execute("INSERT INTO files (id,transfer_id,relative_path,absolute_path,size,mtime_ns,sha256,status,bytes_completed,hash_algorithm) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![file.id.to_string(), transfer_id.to_string(), file.relative_path.to_string_lossy(), file.absolute_path.to_string_lossy(), file.size, file.mtime_ns, file.sha256, file.status.to_string(), file.bytes_completed, file.hash_algorithm.to_string()])?;
        }
        tx.execute(
            "UPDATE transfers SET file_count=?1, original_bytes=?2, updated_at=?3 WHERE id=?4",
            params![
                files.len() as u64,
                bytes,
                Utc::now().to_rfc3339(),
                transfer_id.to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_transfer(&self, id: Uuid) -> Result<Option<Transfer>> {
        self.conn.query_row("SELECT id,server_transfer_id,project_id,source_root,created_at,updated_at,status,file_count,original_bytes,manifest_version FROM transfers WHERE id=?1", [id.to_string()], row_to_transfer).optional().map_err(Into::into)
    }

    pub fn list_transfers(&self) -> Result<Vec<Transfer>> {
        let mut stmt = self.conn.prepare("SELECT id,server_transfer_id,project_id,source_root,created_at,updated_at,status,file_count,original_bytes,manifest_version FROM transfers ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], row_to_transfer)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn incomplete_transfers(&self) -> Result<Vec<Transfer>> {
        Ok(self
            .list_transfers()?
            .into_iter()
            .filter(|t| {
                !matches!(
                    t.status,
                    TransferStatus::Complete | TransferStatus::Cancelled
                )
            })
            .collect())
    }

    pub fn files_for_transfer(&self, transfer_id: Uuid) -> Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare("SELECT id,transfer_id,relative_path,absolute_path,size,mtime_ns,sha256,status,bytes_completed,hash_algorithm FROM files WHERE transfer_id=?1 ORDER BY relative_path")?;
        let rows = stmt.query_map([transfer_id.to_string()], row_to_file)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn replace_part_plan(&mut self, file_id: Uuid, parts: &[PartRecord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM parts WHERE file_id=?1", [file_id.to_string()])?;
        for part in parts {
            tx.execute("INSERT INTO parts (file_id,part_number,source_offset,source_length,transport_length,checksum,etag,attempt_count,status,last_error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![part.file_id.to_string(), part.part_number, part.source_offset, part.source_length, part.transport_length, part.checksum, part.etag, part.attempt_count, part.status.to_string(), part.last_error])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn replace_transport_plan(
        &mut self,
        transfer_id: Uuid,
        objects: &[TransportObjectRecord],
        members: &[TransportMemberRecord],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM transport_objects WHERE transfer_id=?1",
            [transfer_id.to_string()],
        )?;
        for object in objects {
            tx.execute(
                "INSERT INTO transport_objects (id,transfer_id,kind,compression,encoding_version,original_bytes,transport_bytes,cache_path) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    object.id.to_string(),
                    object.transfer_id.to_string(),
                    object.kind.to_string(),
                    object.compression,
                    object.encoding_version,
                    object.original_bytes,
                    object.transport_bytes,
                    object.cache_path.as_ref().map(|path| path.to_string_lossy()),
                ],
            )?;
        }
        for member in members {
            tx.execute(
                "INSERT INTO transport_members (object_id,file_id,member_index) VALUES (?1,?2,?3)",
                params![
                    member.object_id.to_string(),
                    member.file_id.to_string(),
                    member.member_index,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn transport_objects(&self, transfer_id: Uuid) -> Result<Vec<TransportObjectRecord>> {
        let mut statement = self.conn.prepare("SELECT id,transfer_id,kind,compression,encoding_version,original_bytes,transport_bytes,cache_path FROM transport_objects WHERE transfer_id=?1 ORDER BY id")?;
        let rows = statement.query_map([transfer_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let transfer_id: String = row.get(1)?;
            let kind: String = row.get(2)?;
            Ok(TransportObjectRecord {
                id: Uuid::parse_str(&id).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                transfer_id: Uuid::parse_str(&transfer_id).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                kind: match kind.as_str() {
                    "file" => TransportObjectKind::File,
                    "pack" => TransportObjectKind::Pack,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                },
                compression: row.get(3)?,
                encoding_version: row.get(4)?,
                original_bytes: row.get(5)?,
                transport_bytes: row.get(6)?,
                cache_path: row.get::<_, Option<String>>(7)?.map(Into::into),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn transport_members(&self, transfer_id: Uuid) -> Result<Vec<TransportMemberRecord>> {
        let mut statement = self.conn.prepare("SELECT m.object_id,m.file_id,m.member_index FROM transport_members m JOIN transport_objects o ON o.id=m.object_id WHERE o.transfer_id=?1 ORDER BY m.object_id,m.member_index")?;
        let rows = statement.query_map([transfer_id.to_string()], |row| {
            let object_id: String = row.get(0)?;
            let file_id: String = row.get(1)?;
            Ok(TransportMemberRecord {
                object_id: Uuid::parse_str(&object_id).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                file_id: Uuid::parse_str(&file_id).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                member_index: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn parts_for_file(&self, file_id: Uuid) -> Result<Vec<PartRecord>> {
        let mut stmt = self.conn.prepare("SELECT file_id,part_number,source_offset,source_length,transport_length,checksum,etag,attempt_count,status,last_error FROM parts WHERE file_id=?1 ORDER BY part_number")?;
        let rows = stmt.query_map([file_id.to_string()], row_to_part)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn set_upload_session(
        &self,
        file_id: Uuid,
        object_key: &str,
        upload_id: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET object_key=?1, upload_id=?2 WHERE id=?3",
            params![object_key, upload_id, file_id.to_string()],
        )?;
        Ok(())
    }

    pub fn bind_registry_transfer(&self, id: Uuid, server_transfer_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET server_transfer_id=?1, updated_at=?2 WHERE id=?3",
            params![server_transfer_id, Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn bind_registry_file(
        &self,
        file_id: Uuid,
        server_file_id: Uuid,
        object_key: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET server_file_id=?1, object_key=?2 WHERE id=?3",
            params![server_file_id.to_string(), object_key, file_id.to_string()],
        )?;
        Ok(())
    }

    pub fn registry_file_binding(&self, file_id: Uuid) -> Result<Option<(Uuid, String)>> {
        self.conn
            .query_row(
                "SELECT server_file_id,object_key FROM files WHERE id=?1 AND server_file_id IS NOT NULL AND object_key IS NOT NULL",
                [file_id.to_string()],
                |row| {
                    let id: String = row.get(0)?;
                    let parsed = Uuid::parse_str(&id).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok((parsed, row.get(1)?))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn save_registry_session(&self, session: &RegistrySessionRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO registry_sessions (base_url,expires_at,refresh_expires_at,projects_json,created_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(base_url) DO UPDATE SET expires_at=excluded.expires_at,refresh_expires_at=excluded.refresh_expires_at,projects_json=excluded.projects_json,created_at=excluded.created_at",
            params![
                session.base_url,
                session.expires_at.to_rfc3339(),
                session.refresh_expires_at.to_rfc3339(),
                session.projects_json,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn registry_session(&self, base_url: &str) -> Result<Option<RegistrySessionRecord>> {
        self.conn
            .query_row(
                "SELECT base_url,expires_at,refresh_expires_at,projects_json FROM registry_sessions WHERE base_url=?1",
                [base_url],
                |row| {
                    let expires: String = row.get(1)?;
                    let expires_at = DateTime::parse_from_rfc3339(&expires)
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?
                        .with_timezone(&Utc);
                    let refresh_expires: String = row.get(2)?;
                    let refresh_expires_at = DateTime::parse_from_rfc3339(&refresh_expires)
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?
                        .with_timezone(&Utc);
                    Ok(RegistrySessionRecord {
                        base_url: row.get(0)?,
                        expires_at,
                        refresh_expires_at,
                        projects_json: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upload_session(&self, file_id: Uuid) -> Result<Option<(String, String)>> {
        self.conn.query_row("SELECT object_key,upload_id FROM files WHERE id=?1 AND object_key IS NOT NULL AND upload_id IS NOT NULL", [file_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(Into::into)
    }

    pub fn object_key(&self, file_id: Uuid) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT object_key FROM files WHERE id=?1 AND object_key IS NOT NULL",
                [file_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn confirm_part(
        &self,
        file_id: Uuid,
        part_number: u32,
        etag: &str,
        checksum: Option<&str>,
        transport_length: u64,
    ) -> Result<()> {
        self.conn.execute("UPDATE parts SET status='complete', etag=?1, checksum=?2, transport_length=?3, attempt_count=attempt_count+1, last_attempt=?4, last_error=NULL WHERE file_id=?5 AND part_number=?6", params![etag, checksum, transport_length, Utc::now().to_rfc3339(), file_id.to_string(), part_number])?;
        self.refresh_file_progress(file_id)
    }

    pub fn record_part_failure(&self, file_id: Uuid, part_number: u32, error: &str) -> Result<()> {
        self.conn.execute("UPDATE parts SET status='failed', attempt_count=attempt_count+1, last_attempt=?1, last_error=?2 WHERE file_id=?3 AND part_number=?4", params![Utc::now().to_rfc3339(), error, file_id.to_string(), part_number])?;
        Ok(())
    }

    pub fn mark_file_uploaded(&self, file_id: Uuid) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET status='uploaded', bytes_completed=size WHERE id=?1",
            [file_id.to_string()],
        )?;
        Ok(())
    }

    pub fn reconcile_remote_parts(&self, file_id: Uuid, remote: &[(u32, String)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE parts SET status='pending', etag=NULL WHERE file_id=?1 AND status='complete'",
            [file_id.to_string()],
        )?;
        for (number, etag) in remote {
            tx.execute("UPDATE parts SET status='complete', etag=?1, last_error=NULL WHERE file_id=?2 AND part_number=?3", params![etag, file_id.to_string(), number])?;
        }
        tx.commit()?;
        self.refresh_file_progress(file_id)
    }

    fn refresh_file_progress(&self, file_id: Uuid) -> Result<()> {
        self.conn.execute("UPDATE files SET bytes_completed=COALESCE((SELECT SUM(source_length) FROM parts WHERE file_id=?1 AND status='complete'),0) WHERE id=?1", [file_id.to_string()])?;
        Ok(())
    }
}

fn row_to_transfer(row: &rusqlite::Row<'_>) -> rusqlite::Result<Transfer> {
    let parse_error = |index, e: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, e)
    };
    let id_text: String = row.get(0)?;
    let created: String = row.get(4)?;
    let updated: String = row.get(5)?;
    let status: String = row.get(6)?;
    Ok(Transfer {
        id: Uuid::parse_str(&id_text).map_err(|e| parse_error(0, Box::new(e)))?,
        server_transfer_id: row.get(1)?,
        project_id: row.get(2)?,
        source_root: row.get::<_, String>(3)?.into(),
        created_at: DateTime::parse_from_rfc3339(&created)
            .map_err(|e| parse_error(4, Box::new(e)))?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated)
            .map_err(|e| parse_error(5, Box::new(e)))?
            .with_timezone(&Utc),
        status: status.parse().map_err(|e| parse_error(6, Box::new(e)))?,
        file_count: row.get(7)?,
        original_bytes: row.get(8)?,
        manifest_version: row.get(9)?,
    })
}

fn row_to_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    let parse_error = |index, e: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, e)
    };
    let id: String = row.get(0)?;
    let transfer_id: String = row.get(1)?;
    let status: String = row.get(7)?;
    Ok(FileRecord {
        id: Uuid::parse_str(&id).map_err(|e| parse_error(0, Box::new(e)))?,
        transfer_id: Uuid::parse_str(&transfer_id).map_err(|e| parse_error(1, Box::new(e)))?,
        relative_path: row.get::<_, String>(2)?.into(),
        absolute_path: row.get::<_, String>(3)?.into(),
        size: row.get(4)?,
        mtime_ns: row.get(5)?,
        hash_algorithm: row
            .get::<_, String>(9)?
            .parse()
            .map_err(|e| parse_error(9, Box::new(e)))?,
        sha256: row.get(6)?,
        status: serde_json::from_str(&format!("\"{status}\""))
            .map_err(|e| parse_error(7, Box::new(e)))?,
        bytes_completed: row.get(8)?,
    })
}

fn row_to_part(row: &rusqlite::Row<'_>) -> rusqlite::Result<PartRecord> {
    let parse_error = |index, e: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, e)
    };
    let file_id: String = row.get(0)?;
    let status: String = row.get(8)?;
    Ok(PartRecord {
        file_id: Uuid::parse_str(&file_id).map_err(|e| parse_error(0, Box::new(e)))?,
        part_number: row.get(1)?,
        source_offset: row.get(2)?,
        source_length: row.get(3)?,
        transport_length: row.get(4)?,
        checksum: row.get(5)?,
        etag: row.get(6)?,
        attempt_count: row.get(7)?,
        status: status.parse().map_err(|e| parse_error(8, Box::new(e)))?,
        last_error: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{FileStatus, PartStatus};

    fn fixture_file(transfer_id: Uuid) -> FileRecord {
        FileRecord {
            id: Uuid::new_v4(),
            transfer_id,
            relative_path: PathBuf::from("nested/data.bin"),
            absolute_path: PathBuf::from("/tmp/data.bin"),
            size: 20,
            mtime_ns: 1,
            hash_algorithm: crate::HashAlgorithm::Sha256,
            sha256: "0".repeat(64),
            status: FileStatus::Ready,
            bytes_completed: 0,
        }
    }

    #[test]
    fn persists_and_reconciles_remote_parts() {
        let mut store = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(PathBuf::from("/tmp"), None);
        store.create_transfer(&transfer).unwrap();
        let file = fixture_file(transfer.id);
        store
            .replace_inventory(transfer.id, std::slice::from_ref(&file))
            .unwrap();
        let parts = vec![
            PartRecord {
                file_id: file.id,
                part_number: 1,
                source_offset: 0,
                source_length: 10,
                transport_length: None,
                checksum: None,
                etag: None,
                attempt_count: 0,
                status: PartStatus::Pending,
                last_error: None,
            },
            PartRecord {
                file_id: file.id,
                part_number: 2,
                source_offset: 10,
                source_length: 10,
                transport_length: None,
                checksum: None,
                etag: None,
                attempt_count: 0,
                status: PartStatus::Pending,
                last_error: None,
            },
        ];
        store.replace_part_plan(file.id, &parts).unwrap();
        store.confirm_part(file.id, 1, "stale", None, 10).unwrap();
        store
            .reconcile_remote_parts(file.id, &[(2, "remote".into())])
            .unwrap();
        let actual = store.parts_for_file(file.id).unwrap();
        assert_eq!(actual[0].status, PartStatus::Pending);
        assert_eq!(actual[1].status, PartStatus::Complete);
        assert_eq!(actual[1].etag.as_deref(), Some("remote"));
        assert_eq!(
            store.files_for_transfer(transfer.id).unwrap()[0].bytes_completed,
            10
        );
    }

    #[test]
    fn persists_transport_objects_separately_from_logical_files() {
        let mut store = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(PathBuf::from("/tmp"), None);
        store.create_transfer(&transfer).unwrap();
        let first = fixture_file(transfer.id);
        let mut second = fixture_file(transfer.id);
        second.id = Uuid::new_v4();
        second.relative_path = "nested/second.bin".into();
        store
            .replace_inventory(transfer.id, &[first.clone(), second.clone()])
            .unwrap();
        let object = TransportObjectRecord {
            id: Uuid::new_v4(),
            transfer_id: transfer.id,
            kind: TransportObjectKind::Pack,
            compression: "zstd".into(),
            encoding_version: 2,
            original_bytes: first.size + second.size,
            transport_bytes: Some(21),
            cache_path: Some("/tmp/pack.zst".into()),
        };
        let members = [
            TransportMemberRecord {
                object_id: object.id,
                file_id: first.id,
                member_index: 0,
            },
            TransportMemberRecord {
                object_id: object.id,
                file_id: second.id,
                member_index: 1,
            },
        ];
        store
            .replace_transport_plan(transfer.id, std::slice::from_ref(&object), &members)
            .unwrap();

        assert_eq!(store.transport_objects(transfer.id).unwrap(), vec![object]);
        assert_eq!(store.transport_members(transfer.id).unwrap(), members);
    }

    #[test]
    fn migrations_are_safe_when_database_is_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("courier.db");
        drop(TransferStore::open(&path).unwrap());
        drop(TransferStore::open(&path).unwrap());
    }

    #[test]
    fn persists_scoped_registry_session_and_server_file_binding() {
        let mut store = TransferStore::open_in_memory().unwrap();
        let transfer = Transfer::draft(PathBuf::from("/tmp"), Some("P26014".into()));
        store.create_transfer(&transfer).unwrap();
        let file = fixture_file(transfer.id);
        store
            .replace_inventory(transfer.id, std::slice::from_ref(&file))
            .unwrap();
        let server_file_id = Uuid::new_v4();
        store
            .bind_registry_transfer(transfer.id, "ISC-TR-TEST")
            .unwrap();
        store
            .bind_registry_file(file.id, server_file_id, "incoming/opaque/payload")
            .unwrap();
        let session = RegistrySessionRecord {
            base_url: "http://registry.test".into(),
            expires_at: Utc::now(),
            refresh_expires_at: Utc::now(),
            projects_json: "[]".into(),
        };
        store.save_registry_session(&session).unwrap();

        assert_eq!(
            store
                .get_transfer(transfer.id)
                .unwrap()
                .unwrap()
                .server_transfer_id
                .as_deref(),
            Some("ISC-TR-TEST")
        );
        assert_eq!(
            store.registry_file_binding(file.id).unwrap(),
            Some((server_file_id, "incoming/opaque/payload".into()))
        );
        assert!(
            store
                .registry_session("http://registry.test")
                .unwrap()
                .is_some()
        );
        let columns = store
            .conn
            .prepare("PRAGMA table_info(registry_sessions)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "access_token"));
    }
}
