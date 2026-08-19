ALTER TABLE transfers ADD COLUMN registry_url TEXT;

CREATE TABLE app_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

INSERT OR IGNORE INTO app_settings (key, value, updated_at)
SELECT 'active_registry_url', base_url, CURRENT_TIMESTAMP
FROM registry_sessions
ORDER BY created_at DESC
LIMIT 1;

UPDATE transfers
SET registry_url = (
  SELECT value FROM app_settings WHERE key = 'active_registry_url'
)
WHERE server_transfer_id IS NOT NULL
  AND registry_url IS NULL;

INSERT OR IGNORE INTO schema_migrations (version) VALUES (9);
