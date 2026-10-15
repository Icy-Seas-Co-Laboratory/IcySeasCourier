# Icy Seas Data Registry

## Operations console

Start the development stack and open `http://127.0.0.1:8020/admin/`. On first login, enter the configured `REGISTRY_ADMIN_API_KEY` (`development-only-change-me` for the default local stack), enroll the generated TOTP secret in an authenticator, and confirm its current code. Later console logins require both factors and receive a short-lived, memory-only session. The console provides health metrics, project and invitation administration, searchable transfer status, per-file verification evidence, controlled verification retry, and the Registry audit log.

Administrators can reopen invitation codes and edit their expiration, use/size
limits, project scope, purpose, and issuer. New codes are stored encrypted in
addition to their keyed authentication hashes. Keep the token pepper stable and
back it up securely; it also derives the encryption key. Legacy hash-only codes
cannot be recovered: issue a replacement when needed. Changing scope or purpose
revokes existing sessions; users must exchange the invitation again, which counts
as another use. Expiration is the deadline for new code exchanges; existing
sessions retain their session expiry/refresh policy. Revoke to end existing access.
Revoked invitations remain viewable but cannot be edited.

Open a project to inspect metrics and live transfer states, then **Browse project
files** to navigate manifest folders and select files or multiple folders for
direct download from S3. A completed transfer can also be downloaded from its
details or from the project browser after selecting that transfer. Folder and
transfer downloads require a browser with the File System Access directory
picker (Chrome or Edge on a secure origin); choose a destination folder and
Courier restores the manifest paths there. Entire transfers are placed in a
separate folder named for the source and transfer ID. Selections spanning
transfers place each transfer under its ID to avoid path collisions. If any
selected file is still uploading, the collection is held until all files are
available. Completed objects are available during an upload, including
individual members of packs. Availability is separate from independent
verification. The browser streams packs and checks each selected file's size
and recorded digest before publishing it. Browser pack decoding is limited to
a 32 MiB Zstd frame window; unusual compression settings may require Courier
retrieval. Individual large files use the Save File API where available;
other browsers buffer individual files with a 512 MiB limit. Packed previews
read/decompress the containing pack; selecting several members currently reads
that pack for each selection.

The S3 bucket must permit cross-origin `GET` from the exact console origin (for
local development, typically `http://127.0.0.1:8020`; use the actual tailnet console origin for beta administration, or the HTTPS
origin for an HTTPS console). Merge this rule with any existing bucket CORS rules rather than replacing
them. Allow `GET`, origin-specific access, and any required request headers; no
wildcard origin or credentialed browser access is needed. The console's CSP allows
only the configured public S3 origin. Storage failures and CORS failures are
reported in the console; dataset bytes never pass through FastAPI.

Transfer details expose searchable, paginated logs and a full NDJSON export.
Registry lifecycle records are available for existing transfers. Updated desktop clients also
persist typed transfer diagnostics locally and synchronize them with idempotent
acknowledgements. Historical device logs and diagnostics not yet synchronized are
not available on the server. These are typed state, object, part, retry, and error
classification records; free-form device debug output, paths, tokens, and signed
URLs are excluded. The development direct-S3 CLI records local events but has no
Registry session and does not synchronize them. The admin view states its server
coverage explicitly.

See [storage operations review](../../docs/storage-operations-review.md) for
lakeFS publication preparation and the SSD temperature investigation.

## Digest policy

Set `REGISTRY_HASH_ALGORITHM` in the deployment `.env` file before starting the
stack. Supported values are `sha256` (default), `xxhash3` (128-bit XXH3), and
`blake3`. The Registry publishes the policy to Courier, which records the
algorithm in manifest v3. Changing the setting affects new transfers; existing
manifests retain their recorded algorithm.

The Data Registry is Courier's control plane. It manages projects, distinct upload and read-only download invitations, short-lived access sessions with rotating refresh credentials, transfers, provenance, and audit events. Download invitations dynamically list only completed, verified transfers in their authorized projects. Dataset bytes never pass through FastAPI: the API issues short-lived object URLs and Courier performs verified reconstruction locally.

The Registry has its own version in `pyproject.toml`. Increase it and refresh `uv.lock` whenever deployable Registry code changes; CI enforces this. Docker images automatically stamp a `src-...` fingerprint of the packaged source, and `REGISTRY_BUILD_ID` can override it with a commit ID. The landing page and `/api/v1/version` report both values.

## Development

```bash
uv sync --dev
uv run alembic upgrade head
uv run uvicorn data_registry.main:app --reload
uv run pytest
uv run ruff check .
```

Outside development, administrative endpoints require a short-lived bearer session obtained with the admin key and a fresh TOTP code. Development retains `X-Admin-Key` compatibility for local scripts and automated tests. Courier invitation exchange returns a separate short-lived bearer token with access limited to the invitation's projects.

## Manifest and upload API

Courier registers a transfer, submits its v3 manifest once, and receives opaque server object keys for its declared transport-object IDs. It then initiates multipart uploads and requests short-lived presigned `PUT` URLs one part at a time. Completion records the accepted parts before the transfer moves to `finalizing`; that state still awaits independent scientific verification. Versions 1 and 2 are rejected.

From the repository root, the local vertical slice is:

```bash
./scripts/dev-up.sh
./scripts/dev-e2e.sh
```

The smoke test creates a project and single-use invitation, exchanges it for a scoped Courier session, submits an immutable manifest, uploads one real part directly to SeaweedFS, completes the multipart object, and finalizes the transfer. FastAPI handles only metadata and authorization. The host gateway defaults to `http://127.0.0.1:8020`; set `REGISTRY_PORT` to change it. Registry remains private on container port 8010.

The desktop uses these Registry APIs and stores rotating credentials in the operating system vault while preserving non-secret recovery state in SQLite.

Production configuration refuses the documented development admin key, token pepper, and SeaweedFS credentials, and it requires HTTPS. Supply unique secrets through the deployment environment. Authentication, administration, and general client API routes use a bounded, process-local sliding-window rate limiter; this intentionally matches Courier's single-process deployment model. Configure its request rates and maximum tracked clients with `REGISTRY_AUTHENTICATION_REQUESTS_PER_MINUTE`, `REGISTRY_ADMIN_AUTHENTICATION_REQUESTS_PER_MINUTE`, `REGISTRY_ADMIN_REQUESTS_PER_MINUTE`, `REGISTRY_CLIENT_REQUESTS_PER_MINUTE`, and `REGISTRY_RATE_LIMIT_MAXIMUM_CLIENTS`.

## Network security

The admin console and `/api/v1/admin` routes accept direct clients only from loopback or the tailnet's `100.64.0.0/10` range by default. Forwarded client headers are ignored unless the immediate peer is explicitly listed in `REGISTRY_TRUSTED_PROXY_NETWORKS`. For a trusted Cloudflare peer, the Registry uses `CF-Connecting-IP`; other trusted proxies fall back to `X-Forwarded-For`. Keep the trusted-proxy list empty when Uvicorn is directly exposed. Production mode also requires HTTPS and non-development Registry and SeaweedFS credentials.

SeaweedFS is local and authenticated. The Compose stack passes its access and secret keys to `weed mini`; omitting credentials would put SeaweedFS into its development-only anonymous mode. Set `REGISTRY_S3_PUBLIC_ENDPOINT_URL` to the S3 URL reachable by Courier clients. A Cloudflare deployment uses a separate `https://s3.icyseascolab.io` tunnel route because dataset parts travel directly to SeaweedFS. PostgreSQL and the SeaweedFS master port are not published by Compose; Registry and S3 diagnostic ports bind to loopback by default.

See the repository's [closed-beta deployment runbook](../../docs/beta-deployment.md) for the TLS proxy, Compose environment, firewall, invitation, acceptance-test, backup, and rollback procedure.
