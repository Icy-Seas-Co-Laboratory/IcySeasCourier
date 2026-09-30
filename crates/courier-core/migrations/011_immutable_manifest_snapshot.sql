ALTER TABLE transfers ADD COLUMN manifest_json TEXT;
INSERT OR IGNORE INTO schema_migrations(version) VALUES (11);
