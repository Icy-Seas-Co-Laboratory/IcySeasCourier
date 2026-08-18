ALTER TABLE files ADD COLUMN hash_algorithm TEXT NOT NULL DEFAULT 'sha256';
INSERT OR IGNORE INTO schema_migrations (version) VALUES (5);
