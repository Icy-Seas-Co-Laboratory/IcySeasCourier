ALTER TABLE transfers ADD COLUMN registry_session_id TEXT;

CREATE TABLE registry_sessions_v10 (
  session_id TEXT PRIMARY KEY,
  base_url TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  refresh_expires_at TEXT NOT NULL,
  projects_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

INSERT INTO registry_sessions_v10 (session_id, base_url, expires_at, refresh_expires_at, projects_json, created_at)
SELECT 'legacy:' || base_url, base_url, expires_at, refresh_expires_at, projects_json, created_at
FROM registry_sessions;

DROP TABLE registry_sessions;
ALTER TABLE registry_sessions_v10 RENAME TO registry_sessions;
CREATE INDEX registry_sessions_base_url_idx ON registry_sessions(base_url);

UPDATE transfers
SET registry_session_id = 'legacy:' || registry_url
WHERE registry_url IS NOT NULL AND registry_session_id IS NULL;

INSERT OR IGNORE INTO app_settings (key, value, updated_at)
SELECT 'active_registry_session_id', 'legacy:' || value, CURRENT_TIMESTAMP
FROM app_settings
WHERE key = 'active_registry_url';

INSERT OR IGNORE INTO schema_migrations(version) VALUES (10);
