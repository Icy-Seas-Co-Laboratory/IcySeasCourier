# Icy Seas Data Registry

## Operations console

Start the development stack and open `http://127.0.0.1:8010/admin/`. Sign in with
the configured `REGISTRY_ADMIN_API_KEY` (`development-only-change-me` for the
default local stack). The console provides health metrics, project and invitation
administration, searchable transfer status, per-file verification evidence,
controlled verification retry, and the Registry audit log.

The console never receives stored invitation secrets. Newly issued codes are shown
once and the Registry retains only their keyed hashes.

## Digest policy

Set `REGISTRY_HASH_ALGORITHM` in the deployment `.env` file before starting the
stack. Supported values are `sha256` (default), `xxhash3` (128-bit XXH3), and
`blake3`. The Registry publishes the policy to Courier, which records the
algorithm in manifest v2. Changing the setting affects new transfers; existing
manifests retain their recorded algorithm.

The Data Registry is Courier's control plane. It manages projects, upload invitations, short-lived access sessions with rotating refresh credentials, transfers, provenance, and audit events. Dataset bytes never pass through FastAPI.

## Development

```bash
uv sync --dev
uv run alembic upgrade head
uv run uvicorn data_registry.main:app --reload
uv run pytest
uv run ruff check .
```

Administrative endpoints require `X-Admin-Key`. Courier invitation exchange returns a separate short-lived bearer token with access limited to the invitation's projects.

## Manifest and upload API

Courier registers a transfer, submits its versioned manifest once, and receives server-generated file IDs and opaque object keys. It then initiates multipart uploads and requests short-lived presigned `PUT` URLs one part at a time. Completion records the accepted parts before the transfer moves to `finalizing`; that state still awaits independent scientific verification.

From the repository root, the local vertical slice is:

```bash
./scripts/dev-up.sh
./scripts/dev-e2e.sh
```

The smoke test creates a project and single-use invitation, exchanges it for a scoped Courier session, submits an immutable manifest, uploads one real part directly to SeaweedFS, completes the multipart object, and finalizes the transfer. FastAPI handles only metadata and authorization. The host API defaults to `http://127.0.0.1:8010`; set `REGISTRY_PORT` to change it.

The next client increment replaces Courier's development S3 credentials with these Registry APIs while preserving its existing multipart engine and SQLite recovery state.

Production configuration refuses the documented development admin key and token pepper. Supply unique secrets through the deployment environment.
