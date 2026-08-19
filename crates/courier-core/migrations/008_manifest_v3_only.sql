UPDATE transfers
SET manifest_version = 3
WHERE server_transfer_id IS NULL
  AND manifest_version <> 3;

INSERT OR IGNORE INTO schema_migrations (version) VALUES (8);
