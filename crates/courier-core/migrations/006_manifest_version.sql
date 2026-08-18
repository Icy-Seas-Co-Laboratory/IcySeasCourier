ALTER TABLE transfers ADD COLUMN manifest_version INTEGER NOT NULL DEFAULT 1;
INSERT OR IGNORE INTO schema_migrations (version) VALUES (6);
