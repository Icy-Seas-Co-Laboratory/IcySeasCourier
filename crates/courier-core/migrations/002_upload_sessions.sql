ALTER TABLE files ADD COLUMN object_key TEXT;
ALTER TABLE files ADD COLUMN upload_id TEXT;

INSERT OR IGNORE INTO schema_migrations(version) VALUES (2);
