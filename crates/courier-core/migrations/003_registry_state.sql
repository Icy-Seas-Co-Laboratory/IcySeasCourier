ALTER TABLE files ADD COLUMN server_file_id TEXT;

CREATE TABLE registry_sessions (
  base_url TEXT PRIMARY KEY,
  access_token TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  projects_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

INSERT OR IGNORE INTO schema_migrations(version) VALUES (3);
