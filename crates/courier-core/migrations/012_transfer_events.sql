BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS transfer_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  transfer_id TEXT NOT NULL REFERENCES transfers(id) ON DELETE CASCADE,
  occurred_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  details_json TEXT NOT NULL,
  synced_at TEXT
);
CREATE INDEX IF NOT EXISTS transfer_events_pending ON transfer_events(transfer_id, sequence) WHERE synced_at IS NULL;
CREATE TRIGGER IF NOT EXISTS transfer_event_created AFTER INSERT ON transfers BEGIN
  INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
  VALUES((lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),NEW.id,NEW.created_at,'state.created',json_object('state',NEW.status));
END;
CREATE TRIGGER IF NOT EXISTS transfer_event_transition AFTER UPDATE OF status ON transfers
WHEN NEW.status != OLD.status BEGIN
  INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
  VALUES((lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),NEW.id,NEW.updated_at,'state.changed',json_object('state',NEW.status,'from_state',OLD.status));
END;
CREATE TRIGGER IF NOT EXISTS transfer_event_part_confirmed AFTER UPDATE OF status ON transport_parts
WHEN NEW.status = 'complete' AND OLD.status != 'complete' BEGIN
  INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
  SELECT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),transfer_id,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'part.confirmed',
  json_object('object_id',NEW.object_id,'part_number',NEW.part_number,'bytes',NEW.transport_length)
  FROM transport_objects WHERE id=NEW.object_id;
END;
CREATE TRIGGER IF NOT EXISTS transfer_event_part_failed AFTER UPDATE OF attempt_count ON transport_parts
WHEN NEW.status = 'failed' BEGIN
  INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
  SELECT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),transfer_id,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'part.failed',
  json_object('object_id',NEW.object_id,'part_number',NEW.part_number,'attempt',NEW.attempt_count)
  FROM transport_objects WHERE id=NEW.object_id;
END;
CREATE TRIGGER IF NOT EXISTS transfer_event_legacy_part_confirmed AFTER UPDATE OF status ON parts
WHEN NEW.status = 'complete' AND OLD.status != 'complete' BEGIN
  INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
  SELECT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),transfer_id,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'part.confirmed',
  json_object('object_id',NEW.file_id,'part_number',NEW.part_number,'bytes',NEW.transport_length)
  FROM files WHERE id=NEW.file_id;
END;
CREATE TRIGGER IF NOT EXISTS transfer_event_legacy_part_failed AFTER UPDATE OF attempt_count ON parts
WHEN NEW.status = 'failed' BEGIN
  INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
  SELECT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),transfer_id,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'part.failed',
  json_object('object_id',NEW.file_id,'part_number',NEW.part_number,'attempt',NEW.attempt_count)
  FROM files WHERE id=NEW.file_id;
END;
INSERT INTO transfer_events(id,transfer_id,occurred_at,kind,details_json)
SELECT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6)))),id,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'state.snapshot',json_object('state',status) FROM transfers WHERE NOT EXISTS (SELECT 1 FROM transfer_events WHERE transfer_id=transfers.id);
INSERT OR IGNORE INTO schema_migrations(version) VALUES (12);
COMMIT;
