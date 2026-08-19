# Ingest Worker

The ingest worker is implemented by `data_registry.worker` and runs as a separate service from FastAPI. PostgreSQL provides durable claims with row locking and stale-lease recovery.

For each finalized transfer it:

1. streams each manifest-v3 transport object from local SeaweedFS;
2. reads a standalone file or strictly reconstructs each member of an `ISCPACK1` pack;
3. independently calculates logical byte size and the manifest-declared digest;
4. compares both values to the immutable manifest;
5. records file and attempt evidence plus an audit event;
6. advances the transfer to `complete` only when every file matches.

Transient storage failures are retried up to the configured attempt limit. Integrity mismatches are permanent failures and remain visible for operator review.
