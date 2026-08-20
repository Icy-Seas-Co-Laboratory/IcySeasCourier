# Local functional demonstration

This demonstration exercises the desktop workflow, Registry control plane, PostgreSQL state, direct SeaweedFS data plane, and restart-aware Courier state on one machine.

## One-command live demo

```bash
./scripts/demo-start.sh
```

The script checks prerequisites, starts the complete service stack, prepares a small synthetic CTD package, issues a fresh single-use invitation, prints a concise presenter walkthrough, and launches Courier. Use `--no-launch` to prepare everything without opening the desktop application.

During the demonstration, open the Registry Operations Console at
`http://127.0.0.1:8020/admin/`. The launcher prints the local development admin
key. Use the console to show the incoming transfer, its transition through
verification, the immutable manifest digest, file-level checksum evidence, and
the corresponding audit events.

## Start the services

```bash
./scripts/dev-up.sh
./scripts/dev-invite.sh
```

Copy the single-use invitation printed by the second command.

To make the development Registry's admin console available through Headscale, opt in when starting the stack:

```bash
./scripts/dev-up.sh --tailscale
```

The option requires an installed Tailscale CLI connected to Headscale. It configures persistent HTTP Serve to Courier's dedicated loopback-only VPN origin and prints the tailnet URL. Tailnet transport remains encrypted, but the browser URL is HTTP because Headscale cannot currently provision Serve certificates. Use it only on a tailnet whose policy restricts the intended users, especially when the Registry still has development credentials. Ordinary local development does not change host-level Tailscale state. The equivalent environment switch is `COURIER_ENABLE_TAILSCALE_SERVE=1`; use `COURIER_TAILSCALE_HTTP_PORT` only if the tailnet endpoint cannot use port 80. The script prints the matching command for disabling Serve later.

## Start Courier

```bash
cd apps/courier-desktop
npm install
npm run tauri dev
```

In Courier:

1. enter the development invitation;
2. select the authorized demonstration project;
3. choose a small folder containing representative files;
4. review the inventory and configured digest summary;
5. start the upload and observe confirmed-byte progress;
6. optionally pause and resume;
7. observe **Finalizing**, then **Verifying**, and finally **Complete** after every logical file matches its immutable manifest entry.

The access and rotating refresh credentials are stored in the operating system credential vault. SQLite stores only non-secret session metadata, local transfer state, Registry IDs, part state, and source paths.

## Automated checks

```bash
./scripts/dev-e2e.sh
cargo test --workspace
cd apps/data-registry && uv run pytest
cd ../courier-desktop && npm run check
```

The Python smoke test covers the deployed API, object store, worker, and verified completion. The ignored Rust live test can additionally be run with a freshly issued `COURIER_TEST_INVITATION` to exercise the native client through refresh, registration, manifest submission, multipart upload, independent verification, and completion.
