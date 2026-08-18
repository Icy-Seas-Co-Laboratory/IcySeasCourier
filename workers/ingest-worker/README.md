# Ingest Worker

The ingest worker is implemented by `data_registry.worker` and runs as a separate service from FastAPI. PostgreSQL provides durable claims with row locking and stale-lease recovery.

For each finalized transfer it:

1. streams each opaque object from S3-compatible storage;
2. decodes the declared transport compression;
3. independently calculates logical byte size and SHA-256;
4. compares both values to the immutable manifest;
5. records file and attempt evidence plus an audit event;
6. advances the transfer to `complete` only when every file matches.

Transient storage failures are retried up to the configured attempt limit. Integrity mismatches are permanent failures and remain visible for operator review.
