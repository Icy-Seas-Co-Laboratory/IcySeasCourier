# Courier closed-beta setup and deployment

This runbook describes a small, controlled Courier beta using one Registry process, one verification worker, PostgreSQL, and local SeaweedFS. It is not a high-availability design. Beta users and operators must connect through the approved VPN.

## Deployment topology

Use one dedicated Linux host with persistent local storage:

```text
VPN client
  ├─ HTTPS registry name ── reverse proxy ── 127.0.0.1:8010 ── Registry
  └─ HTTPS objects name  ── reverse proxy ── 127.0.0.1:8333 ── SeaweedFS S3
                                                   │
                          PostgreSQL + SeaweedFS local volumes
                                                   │
                                      one verification worker
```

Expose only the reverse proxy on the VPN interface. Keep PostgreSQL, the Registry backend, the SeaweedFS S3 backend, and the SeaweedFS master port bound to loopback. Courier's admin console and admin API independently reject addresses outside loopback and `100.64.0.0/16`.

## Prerequisites

- A maintained Linux host with Docker Engine and Compose.
- Two VPN-resolvable HTTPS names, one for the Registry and one for SeaweedFS S3.
- TLS certificates accepted by beta client computers.
- Enough local space for incoming datasets, multipart staging, PostgreSQL, and operational headroom.
- A second storage location for backups. A backup on the Courier host is not sufficient.
- A signed and notarized Courier beta build for each supported desktop platform.

## Configure the host

Copy `.env.example` to `.env`, restrict it to the deployment account, and replace every development value. Generate URL-safe secrets rather than passwords requiring database-URL escaping.

```dotenv
REGISTRY_ENVIRONMENT=beta
POSTGRES_PASSWORD=<unique-url-safe-password>
REGISTRY_ADMIN_API_KEY=<unique-random-secret>
REGISTRY_TOKEN_PEPPER=<unique-random-secret>

AWS_ACCESS_KEY_ID=<unique-seaweed-access-key>
AWS_SECRET_ACCESS_KEY=<unique-seaweed-secret-key>
REGISTRY_S3_BUCKET=icy-seas-incoming
REGISTRY_S3_PUBLIC_ENDPOINT_URL=https://objects.example.internal

REGISTRY_REQUIRE_HTTPS=true
REGISTRY_BIND_ADDRESS=127.0.0.1
SEAWEEDFS_BIND_ADDRESS=127.0.0.1
REGISTRY_ADMIN_ALLOWED_NETWORKS=127.0.0.1/32,::1/128,100.64.0.0/16

# Replace these with the immediate reverse-proxy source address or narrow CIDR
# as observed inside the Registry container. Never trust all addresses.
REGISTRY_TRUSTED_PROXY_NETWORKS=<proxy-source-address>/32
REGISTRY_FORWARDED_ALLOW_IPS=127.0.0.1,<proxy-source-address>/32
```

Keep the default single-process rate limits initially. They can be adjusted in `.env` without introducing a shared rate-limit service.

## Configure TLS and proxying

The reverse proxy must preserve `Host`, set `X-Forwarded-Proto: https`, and supply the original client address in `X-Forwarded-For`. A minimal Caddy-style topology is:

```caddyfile
registry.example.internal {
    reverse_proxy 127.0.0.1:8010
}

objects.example.internal {
    reverse_proxy 127.0.0.1:8333
}
```

Use the actual VPN names and the organization's certificate mechanism. Determine the immediate proxy address visible inside the Registry container and use only that address in `REGISTRY_TRUSTED_PROXY_NETWORKS`. Include that address and `127.0.0.1` in `REGISTRY_FORWARDED_ALLOW_IPS`; loopback is needed by the container health check. Incorrect proxy trust either blocks legitimate admin access or permits spoofed client addresses.

The host firewall should allow HTTPS only on loopback and the VPN interface. Ports 5432, 8010, 8333, and 9333 should not accept traffic from other interfaces.

## Start and validate services

Back up existing state before upgrading. Then build and start the stack:

```bash
docker compose up --build -d postgres seaweedfs data-registry ingest-worker
docker compose ps
docker compose logs --tail=100 data-registry ingest-worker
```

The Registry container applies Alembic migrations before starting. Validate through the public HTTPS endpoint:

```bash
curl --fail https://registry.example.internal/health
curl --fail https://registry.example.internal/ready
```

From a VPN client, confirm that the admin console loads and accepts the admin key. From a non-VPN address, confirm that `/admin/` and `/api/v1/admin/overview` return `403`. Also confirm that ports 8010, 8333, 9333, and 5432 are unreachable directly from the network.

## Create beta access

Open `https://registry.example.internal/admin/` from localhost or the VPN:

1. Create an active project with its stable project code.
2. Issue a single-use invitation with an expiry and a suitable maximum transfer size.
3. Send the tester the Registry HTTPS address and invitation code through an approved channel.
4. Never send the admin key to a beta tester.

In Courier, the tester enters the Registry address and invitation code. Courier requires HTTPS for non-loopback Registry addresses, saves the selected endpoint, stores credentials in the operating-system vault, and binds every new transfer to that Registry. Changing the active Registry later does not redirect existing transfers.

Ask testers to leave source files in place and unchanged until the transfer reaches **Complete**. `Finalizing` and `Verifying` do not mean that scientific integrity has been confirmed.

## Beta acceptance test

Before inviting external testers, complete this sequence from a separate VPN-connected computer:

1. Install the signed beta package on a clean user account.
2. Enter the HTTPS Registry address and a single-use invitation.
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
