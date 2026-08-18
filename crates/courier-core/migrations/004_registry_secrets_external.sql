CREATE TABLE registry_sessions_v4 (
  base_url TEXT PRIMARY KEY,
  expires_at TEXT NOT NULL,
  refresh_expires_at TEXT NOT NULL,
  projects_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

INSERT INTO registry_sessions_v4 (
  base_url, expires_at, refresh_expires_at, projects_json, created_at
)
SELECT base_url, expires_at, expires_at, projects_json, created_at
FROM registry_sessions;

DROP TABLE registry_sessions;
ALTER TABLE registry_sessions_v4 RENAME TO registry_sessions;

INSERT OR IGNORE INTO schema_migrations(version) VALUES (4);
