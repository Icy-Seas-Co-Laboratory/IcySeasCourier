CREATE TABLE transport_objects (
  id TEXT PRIMARY KEY,
  transfer_id TEXT NOT NULL REFERENCES transfers(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('file', 'pack')),
  compression TEXT NOT NULL CHECK(compression IN ('none', 'zstd')),
  encoding_version INTEGER NOT NULL,
  original_bytes INTEGER NOT NULL,
  transport_bytes INTEGER,
  cache_path TEXT,
  server_object_id TEXT,
  object_key TEXT,
  upload_id TEXT
);

CREATE TABLE transport_members (
  object_id TEXT NOT NULL REFERENCES transport_objects(id) ON DELETE CASCADE,
  file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  member_index INTEGER NOT NULL,
  PRIMARY KEY(object_id, member_index),
  UNIQUE(file_id)
);

CREATE TABLE transport_parts (
  object_id TEXT NOT NULL REFERENCES transport_objects(id) ON DELETE CASCADE,
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
  PRIMARY KEY(object_id, part_number)
);

INSERT OR IGNORE INTO schema_migrations (version) VALUES (7);
