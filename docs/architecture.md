# Architecture

Courier is client-first and library-first. `courier-core` owns inventory, integrity, state, scheduling, retries, and transfer behavior. The CLI and future Tauri desktop application are adapters; transfer logic does not live in JavaScript.

SQLite is the durable local source of truth. WAL mode and transactional file/part updates allow an interrupted process to discover incomplete transfers and continue from confirmed part state. Absolute paths exist only in local state; manifests will contain preserved relative paths.

The transfer data plane will use standard S3 multipart semantics against SeaweedFS. The later Data Registry will remain a small control plane that authorizes narrowly scoped direct uploads and verifies reconstructed logical files. FastAPI will never proxy dataset bytes.

## Current increment

- versioned relational SQLite schema for transfers, files, and parts;
- explicit transfer-state transition validation;
- recursive, symlink-safe inventory;
- bounded-memory, setup-selected SHA-256, 128-bit XXH3, or BLAKE3 hashing;
- size and nanosecond modification-time mutation checks;
- transient HTTP retry classification with exponential full jitter;
- storage-independent multipart boundaries and upload-session contracts;
- reconciliation in which remote confirmed parts repair local state after an ambiguous crash;
- bounded-memory source-range upload with a mutation check before every part;
- transactional ETag persistence only after storage acceptance;
- an AWS S3 SDK adapter configured for custom path-style SeaweedFS endpoints;
- ambiguity-safe completion using a metadata-only object existence check;
- human-readable and JSON CLI output.

## Desktop boundary

The Tauri 2 application is a thin interface adapter. Native folder selection happens through a narrowly scoped dialog capability. Inventory, hashing, SQLite state, multipart planning, and state transitions run in Rust commands backed by `courier-core` and `courier-transfer`. The Svelte layer owns presentation and ordinary interaction state only.

Desktop progress is emitted only after a part ETag is committed locally. Pause is cooperative: an in-flight request is allowed to resolve, its result is persisted, and the engine stops before beginning another part. Resume uses the ordinary remote-authoritative reconciliation path, so application restarts and user pauses share one recovery mechanism.

## Next client increment

The Rust Courier client connects to Registry invitation exchange, renewable session rotation, transfer registration, immutable manifest submission, Registry-issued multipart authorization, and verification status. SQLite remains the recovery source of truth for non-secret metadata and Registry identities; rotating access and refresh credentials live in the operating system credential vault. The PostgreSQL-backed ingest worker claims finalized transfers, streams and reconstructs logical objects, records verification evidence, and alone advances a transfer to `complete`.
