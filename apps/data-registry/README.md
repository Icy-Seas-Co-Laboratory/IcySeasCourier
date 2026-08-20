# Icy Seas Data Registry

## Operations console

Start the development stack and open `http://127.0.0.1:8020/admin/`. Sign in with
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
algorithm in manifest v3. Changing the setting affects new transfers; existing
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

Courier registers a transfer, submits its v3 manifest once, and receives opaque server object keys for its declared transport-object IDs. It then initiates multipart uploads and requests short-lived presigned `PUT` URLs one part at a time. Completion records the accepted parts before the transfer moves to `finalizing`; that state still awaits independent scientific verification. Versions 1 and 2 are rejected.

From the repository root, the local vertical slice is:

```bash
./scripts/dev-up.sh
./scripts/dev-e2e.sh
```

The smoke test creates a project and single-use invitation, exchanges it for a scoped Courier session, submits an immutable manifest, uploads one real part directly to SeaweedFS, completes the multipart object, and finalizes the transfer. FastAPI handles only metadata and authorization. The host gateway defaults to `http://127.0.0.1:8020`; set `REGISTRY_PORT` to change it. Registry remains private on container port 8010.

The desktop uses these Registry APIs and stores rotating credentials in the operating system vault while preserving non-secret recovery state in SQLite.

Production configuration refuses the documented development admin key, token pepper, and SeaweedFS credentials, and it requires HTTPS. Supply unique secrets through the deployment environment. Authentication, administration, and general client API routes use a bounded, process-local sliding-window rate limiter; this intentionally matches Courier's single-process deployment model. Configure its request rates and maximum tracked clients with `REGISTRY_AUTHENTICATION_REQUESTS_PER_MINUTE`, `REGISTRY_ADMIN_REQUESTS_PER_MINUTE`, `REGISTRY_CLIENT_REQUESTS_PER_MINUTE`, and `REGISTRY_RATE_LIMIT_MAXIMUM_CLIENTS`.

## Network security

The admin console and `/api/v1/admin` routes accept direct clients only from loopback or Tailscale's `100.64.0.0/10` range by default. Forwarded client headers are ignored unless the immediate peer is explicitly listed in `REGISTRY_TRUSTED_PROXY_NETWORKS`. For a trusted Cloudflare peer, the Registry uses `CF-Connecting-IP`; other trusted proxies fall back to `X-Forwarded-For`. Keep the trusted-proxy list empty when Uvicorn is directly exposed. Production mode also requires HTTPS and non-development Registry and SeaweedFS credentials.

SeaweedFS is local and authenticated. The Compose stack passes its access and secret keys to `weed mini`; omitting credentials would put SeaweedFS into its development-only anonymous mode. Set `REGISTRY_S3_PUBLIC_ENDPOINT_URL` to the S3 URL reachable by Courier clients. A Cloudflare deployment uses a separate `https://s3.icyseascolab.io` tunnel route because dataset parts travel directly to SeaweedFS. PostgreSQL and the SeaweedFS master port are not published by Compose; Registry and S3 diagnostic ports bind to loopback by default.

See the repository's [closed-beta deployment runbook](../../docs/beta-deployment.md) for the TLS proxy, Compose environment, firewall, invitation, acceptance-test, backup, and rollback procedure.
