# Icy Seas Courier

Icy Seas Courier is a resumable scientific-data transfer client for Icy Seas Co-Laboratory LLC. It is being built as a reusable Rust engine with CLI, desktop, headless, and instrument-facing clients layered over the same implementation.

This repository contains the client-first foundation of Phase 1: the monorepo, persistent transfer and multipart state, strict recursive inventory and configurable logical-file hashing, source-mutation checks, deterministic S3-compatible part planning, restart reconciliation, bounded-memory SeaweedFS uploads, retry policy, and CLI and desktop interfaces. The Registry accepts manifest v3 only, keeps logical files separate from transport objects, provides scoped renewable sessions and direct object upload authorization, and reserves `complete` for independent server-side verification.

## Quick start

For a prepared live demonstration:

```bash
./scripts/demo-start.sh
```

The command starts the Registry and verification worker, creates representative synthetic data and a single-use invitation, prints the presenter flow, and launches the desktop client.

For the development CLI:

```bash
cargo run -p courier-cli -- upload ./test-data --project P26014
cargo run -p courier-cli -- transfers
cargo run -p courier-cli -- inspect <transfer-id>
```

For the local multipart proof of concept:

```bash
docker-compose up -d seaweedfs
export AWS_ACCESS_KEY_ID=development
export AWS_SECRET_ACCESS_KEY=development
export AWS_REGION=us-east-1
cargo run -p courier-cli -- send <transfer-id>
```

The development CLI uploads directly to its configured S3-compatible data plane and therefore stops at `finalizing`. The Registry-connected desktop flow submits the transfer for independent verification and polls until it reaches `complete` or `failed`.

Use `--state-db ./courier.db` for an isolated development database and `--json` for automation.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p courier-transfer --test seaweedfs -- --ignored
```

See [docs/architecture.md](docs/architecture.md), [docs/adr](docs/adr), and the [closed-beta deployment runbook](docs/beta-deployment.md) for design and operating guidance.

## Desktop application

The initial Tauri 2/Svelte application lives in `apps/courier-desktop`. It provides validated Registry endpoint configuration, invitation context, native source-folder selection, Rust-backed inventory review, and locally persisted transfer discovery.

```bash
cd apps/courier-desktop
npm install
npm run check
npm run tauri dev
```

The desktop exchanges Registry invitations, presents only authorized projects, groups files up to 8 MiB into deterministic 64 MiB target zstd transport packs, registers immutable v3 manifests, and uploads through short-lived part-specific URLs. Pack files, multipart state, and rotating Registry sessions are cached for pause/resume recovery; pack files are removed after Registry verification completes. Courier displays live confirmed logical-byte progress, keeps credentials in the operating system vault, and never describes an object-store upload as a verified dataset.

## Status language

Courier deliberately distinguishes `uploaded` from `complete`. Local upload completion is not scientific verification. A transfer becomes `complete` only after the Data Registry ingest worker reconstructs every logical file and independently matches the digest recorded in its immutable manifest.

## Data Registry

The FastAPI/PostgreSQL control plane supports projects, hashed upload invitations, rotating renewable Courier sessions, scoped and idempotent transfer creation, immutable canonical v3 manifests, opaque transport object keys, narrowly scoped multipart authorization, audit events, schema migrations, request limits, security headers, and health/readiness endpoints.

```bash
./scripts/dev-up.sh
./scripts/dev-invite.sh
./scripts/dev-e2e.sh
```

OpenAPI documentation is available at `http://127.0.0.1:8010/docs` in development. Dataset bytes continue to travel directly to S3-compatible storage; the Registry does not proxy them.

The Registry Operations Console is available at `http://127.0.0.1:8010/admin/` and is rejected unless the client address is loopback or in `100.64.0.0/16`.
For the default development stack, unlock it with the local-only admin key
`development-only-change-me`.

See [docs/demo.md](docs/demo.md) for the complete desktop demonstration workflow.
