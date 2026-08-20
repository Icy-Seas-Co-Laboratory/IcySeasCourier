# Courier closed-beta setup and deployment

This runbook describes a small, controlled Courier beta using one Registry process, one verification worker, PostgreSQL, local SeaweedFS, and a Cloudflare Tunnel. It is not a high-availability design.

## Deployment topology

Use one maintained Linux host with persistent local storage. It may run other Compose projects because Courier does not publish PostgreSQL or the SeaweedFS master port and uses project-scoped volumes and networks:

```text
Courier Desktop
  ├─ https://courier.icyseascolab.io ─┐
  └─ https://s3.icyseascolab.io ──────┤ Cloudflare edge
                                      │
                           outbound Cloudflare Tunnel
                                      │
                ┌─────────────────────┴─────────────────────┐
                │ dedicated courier-edge Docker network     │
                │ Registry                         SeaweedFS │
                └──────────────┬──────────────────────┬─────┘
                               │ courier-backend       │
                         PostgreSQL             verification worker
```

The tunnel makes outbound connections, so the host needs no public inbound port. Registry and S3 diagnostic ports bind to loopback by default and are configurable to avoid collisions. PostgreSQL and the SeaweedFS master port exist only on the internal `courier-backend` network. Courier's admin console and admin API independently enforce the configured client networks even when traffic arrived through Cloudflare.

## Prerequisites

- A maintained Linux host with Docker Engine and Compose.
- A remotely managed Cloudflare Tunnel with routes for `courier.icyseascolab.io` and `s3.icyseascolab.io`.
- Enough local space for incoming datasets, multipart staging, PostgreSQL, and operational headroom.
- A second storage location for backups. A backup on the Courier host is not sufficient.
- A signed and notarized Courier beta build for each supported desktop platform. See [Courier Desktop releases](desktop-releases.md) for the build and Apple-signing workflow.

## Configure the host

Copy `.env.beta.example` to `.env`, restrict it to the deployment account, and replace every `CHANGE_ME` value. Generate URL-safe secrets rather than passwords requiring database-URL escaping.

```bash
cp .env.beta.example .env
chmod 600 .env
# Edit .env and replace every CHANGE_ME value before continuing.
```

Keep the default single-process rate limits initially. They can be adjusted in `.env` without introducing a shared rate-limit service.

The default Compose project name is `icy-seas-courier`, so its networks, containers, and named volumes do not collide with unrelated Compose projects. PostgreSQL has no host port. If ports 8010 or 8333 are already occupied on loopback, change `REGISTRY_PORT` or `SEAWEEDFS_S3_PORT`; the container ports and Cloudflare routes do not change.

The edge network defaults to `172.30.50.0/24`, with `cloudflared` at `172.30.50.10`. If that subnet overlaps another Docker or host network, choose an unused private `/24`, update `CLOUDFLARED_IPV4_ADDRESS`, and update both proxy allowlists to the new cloudflared address.

## Configure the Cloudflare Tunnel

Create a remotely managed tunnel in Cloudflare and configure two published application routes:

```text
courier.icyseascolab.io  ->  http://data-registry:8000
s3.icyseascolab.io       ->  http://seaweedfs:8333
```

The S3 hostname is required. The Registry returns presigned URLs built from `REGISTRY_S3_PUBLIC_ENDPOINT_URL`, and Courier uploads dataset parts directly to those URLs; the Registry never proxies dataset bytes. Keep path-style S3 routing and preserve the public hostname. Do not put an interactive Cloudflare Access login in front of either Courier route because the desktop client and presigned S3 `PUT` requests do not implement that login flow.

Cloudflare supplies the original scheme and client address headers. Only the fixed cloudflared container address is trusted to supply them. The Registry prefers `CF-Connecting-IP` over `X-Forwarded-For` for Cloudflare traffic.

Cloudflare Free and Pro plans limit request bodies to 100 MB. Courier therefore uses 64 MiB multipart parts. A cache-bypass rule for `s3.icyseascolab.io` is recommended; S3 upload requests must not be transformed. Confirm that zone settings do not reduce the maximum upload size below 64 MiB.

Copy the remotely managed tunnel token into `CLOUDFLARE_TUNNEL_TOKEN` in `.env`. Anyone holding this token can run the tunnel, so handle it as a credential and rotate it if exposed.

## Start and validate services

Back up existing state before upgrading. Build the shared Registry image first, then start the isolated stack and tunnel:

```bash
docker compose build data-registry
docker compose --profile cloudflare up -d postgres seaweedfs data-registry ingest-worker cloudflared
docker compose ps
docker compose logs --tail=100 data-registry ingest-worker cloudflared
```

The Registry container applies Alembic migrations before starting. Validate through the public HTTPS endpoint:

```bash
curl --fail https://courier.icyseascolab.io/
curl --fail https://courier.icyseascolab.io/health
curl --fail https://courier.icyseascolab.io/ready
```

Complete the beta acceptance transfer from a separate client to prove that a presigned multipart upload traverses the S3 tunnel and reaches independent verification. Confirm that `/admin/` and `/api/v1/admin/overview` return `403` from a client outside `REGISTRY_ADMIN_ALLOWED_NETWORKS`. Also confirm that ports 8010 and 8333 are loopback-only and that ports 5432 and 9333 are not published on the host.

## Create beta access

Open `https://courier.icyseascolab.io/admin/` from an address explicitly included in `REGISTRY_ADMIN_ALLOWED_NETWORKS`:

1. Create an active project with its stable project code.
2. Issue a single-use invitation with an expiry and a suitable maximum transfer size.
3. Send the tester the Registry HTTPS address and invitation code through an approved channel.
4. Never send the admin key to a beta tester.

In Courier, the tester enters the Registry address and invitation code. Courier requires HTTPS for non-loopback Registry addresses, saves the selected endpoint, stores credentials in the operating-system vault, and binds every new transfer to that Registry. Changing the active Registry later does not redirect existing transfers.

Ask testers to leave source files in place and unchanged until the transfer reaches **Complete**. `Finalizing` and `Verifying` do not mean that scientific integrity has been confirmed.

## Beta acceptance test

Before inviting external testers, complete this sequence from a separate beta client computer:

1. Install the signed beta package on a clean user account.
2. Enter `https://courier.icyseascolab.io` and a single-use invitation.
3. Upload nested, Unicode, empty, and small files that produce at least one transport pack.
4. Upload a file larger than the 8 MiB pack-member threshold.
5. Pause during upload, quit Courier, reopen it, and resume.
6. Restart the Registry and verification worker during a separate transfer and confirm recovery.
7. Confirm that an expired access token renews without re-entering the invitation.
8. Confirm that every successful transfer progresses through `Finalizing` and `Verifying` before `Complete`.
9. Corrupt a test object and confirm that verification fails with evidence in the admin console.
10. Confirm that completed pack caches disappear from the client after status refresh.

Record the Courier version, Registry image/version, operating system, dataset shape, transfer ID, and outcome for every acceptance run.

## Backup and restore

Back up PostgreSQL and the SeaweedFS volume together. For a consistent maintenance backup:

1. Stop the Registry and verification worker so transfer state cannot change.
2. Create a PostgreSQL `pg_dump` archive.
3. Snapshot or archive the SeaweedFS data volume while SeaweedFS is stopped.
4. Copy both artifacts and the deployment configuration to separate storage.
5. Restart the services and confirm `/ready`.

Example database dump:

```bash
docker compose stop data-registry ingest-worker
docker compose exec -T postgres pg_dump -U registry -Fc registry > registry.dump
docker compose stop seaweedfs
# Snapshot or archive the volume reported by: docker volume inspect <seaweedfs-volume>
docker compose start seaweedfs data-registry ingest-worker
```

Test restoration into an isolated Compose project before the beta begins and periodically thereafter. A backup is not considered valid until a restored Registry can locate and verify its corresponding SeaweedFS objects.

## Operations during beta

- Review failed and long-running transfers in the admin console each day the beta is active.
- Watch host disk space, container restarts, PostgreSQL health, and worker logs.
- Retain the immutable manifest digest and file-level verification evidence when investigating a transfer.
- Revoke unused or compromised invitations immediately.
- Use the controlled admin retry only after identifying the reason verification failed.
- Keep one Registry process and one verification worker. The current in-process rate limiter is designed for this topology.
- Schedule cleanup of abandoned multipart uploads until automatic abort and retention handling are implemented.

## Rollback

Do not downgrade a live v3 database in place. If an upgrade must be rolled back:

1. Stop Courier intake, the Registry, and the worker.
2. Preserve logs and the failed deployment state for diagnosis.
3. Restore the coordinated pre-upgrade PostgreSQL and SeaweedFS backups.
4. Deploy the previously tested application version.
5. Run readiness and a disposable transfer test before reopening invitations.

Local desktop migration 009 is additive: it records the active Registry and the Registry assigned to each transfer. Preserve the user's Courier application-data directory when replacing the desktop build.

## Beta exit criteria

The closed beta is successful when representative transfers repeatedly survive interruption, independently verify, and can be diagnosed by an operator; backup restoration is demonstrated; no admin route is reachable outside the approved networks; and testers can configure the Registry without terminal commands.
