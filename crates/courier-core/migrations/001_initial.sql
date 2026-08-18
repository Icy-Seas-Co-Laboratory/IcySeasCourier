CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS transfers (
  id TEXT PRIMARY KEY,
  server_transfer_id TEXT,
  project_id TEXT,
  source_root TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  file_count INTEGER NOT NULL DEFAULT 0,
  original_bytes INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS files (
  id TEXT PRIMARY KEY,
  transfer_id TEXT NOT NULL REFERENCES transfers(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  absolute_path TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  status TEXT NOT NULL,
  bytes_completed INTEGER NOT NULL DEFAULT 0,
  UNIQUE(transfer_id, relative_path)
);

CREATE TABLE IF NOT EXISTS parts (
  file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  part_number INTEGER NOT NULL,
  source_offset INTEGER NOT NULL,
  source_length INTEGER NOT NULL,
  transport_length INTEGER,
  checksum TEXT,
  etag TEXT,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'pending',
  last_attempt TEXT,
  last_error TEXT,
  PRIMARY KEY(file_id, part_number)
);

INSERT OR IGNORE INTO schema_migrations(version) VALUES (1);

