# Courier instructions for Codex

Applies to the whole repository; no nested `AGENTS.md` files exist.

## Efficient workflow

- Begin with `git status --short`; preserve unrelated user changes.
- Find narrowly with `rg`; skip `target/`, `node_modules/`, caches, generated bundles, `.signing/`, `.env`, databases, and exports.
- Read only the relevant README/architecture/ADR. Make the smallest safe patch and run focused validation first.
- Do not reset, delete, commit, publish, or expose secrets unless explicitly requested.
- Keep reports concise: outcome, validation, and remaining manual/external steps.

## Hybrid model routing

Use the least expensive model that can safely complete the stage; escalate with context rather than restarting:

- **Luna**: fast repository search, file/README inventory, routine formatting, simple mechanical edits, and narrow test execution.
- **Terra**: default for implementation, debugging, test-writing, code review, and ordinary cross-file changes.
- **Sol**: architecture decisions, security/authentication/cryptography, data-integrity or migration work, deployment/network changes, signing/notarization, and ambiguous high-impact reviews.

Preferred flow: Luna discovers and scopes → Terra implements and tests → Sol reviews only when the change touches a listed high-risk area or Terra reports uncertainty. Keep one model on a focused subtask; pass summaries and file paths, not full repository dumps.

## Security

- Never print or commit credentials, tokens, private keys, `.env`, `.p12`, or `.p8` contents.
- Production uses HTTPS, unique secrets, short-lived tokens, OS credential storage, and project-scoped invitations. Download invitations are read-only.
- Development-only credentials or fallbacks must be clearly labeled.

## Architecture invariants

- Rust owns transfer behavior: `courier-core` (inventory/state/recovery), `courier-pack` (deterministic packs), `courier-transfer` (multipart/reconciliation), and `courier-registry` (Registry client/sessions).
- CLI and Tauri are adapters. `apps/courier-desktop/src/App.svelte` is presentation only; data operations and native dialogs belong in Rust/Tauri.
- `apps/data-registry` is the FastAPI/PostgreSQL control plane. It handles authorization, metadata, manifests, audit, and verification; it never proxies dataset bytes.
- S3/SeaweedFS is the data plane with short-lived Registry-issued URLs.
- SQLite is the local recovery source of truth. Preserve immutable manifest v3, source-mutation checks, accepted-part reconciliation, and explicit state transitions.
- `complete` means independent Registry verification, not merely uploaded bytes. Keep logical files separate from opaque transport objects.

## Where to look

| Task | Files |
| --- | --- |
| Design | `README.md`, `docs/architecture.md`, `docs/adr/` |
| Rust transfer | `crates/`, focused crate tests |
| Desktop | `apps/courier-desktop/README.md`, `src/App.svelte`, `src-tauri/src/lib.rs` |
| Registry | `apps/data-registry/README.md`, `data_registry/api.py`, `tests/` |
| Deployment | `docker-compose.yml`, `scripts/dev-up.sh`, `docs/beta-deployment.md` |
| Releases | `scripts/build-desktop-macos.sh`, `.github/workflows/release-desktop.yml`, `docs/desktop-releases.md` |

## Validation

From the repository root:

```sh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Prefer `cargo check/test -p <package>` for focused Rust work. The ignored SeaweedFS test requires services:

```sh
cargo test -p courier-transfer --test seaweedfs -- --ignored
```

Desktop:

```sh
cd apps/courier-desktop
npm run check
npm run build
npm run tauri dev
```

There is no JavaScript `test` script; desktop Rust tests use `cargo test -p courier-desktop`.

Registry:

```sh
cd apps/data-registry
uv sync --dev
uv run pytest
uv run ruff check .
```

## Local stack

`./scripts/dev-up.sh` starts the isolated Compose stack. Defaults: Registry container `8010`, host gateway `8020`, VPN/admin gateway `8021`, S3/SeaweedFS host port `8333`. PostgreSQL is internal; preserve `courier-backend`/`courier-edge` network isolation. Use a separate S3 tunnel hostname in deployments.

```sh
./scripts/dev-up.sh
./scripts/dev-invite.sh
./scripts/dev-e2e.sh
```

Use `./scripts/demo-start.sh --no-launch` for a prepared demo. Never copy development admin keys into output or production docs.

## Change-specific checks

- Transfer changes: test transitions, pause/resume, restart reconciliation, mutation checks, and part accounting as relevant.
- Manifest/Registry changes: update schema, docs, tests, idempotency, and project authorization together.
- Downloads: enforce safe relative paths, size/digest verification, and atomic publication.
- UI: keep live status truthful and distinguish local, authorization, storage, and verification errors.
- Config/deployment: update examples and docs without secrets.
- macOS releases: use `scripts/build-desktop-macos.sh`; verify strict codesign and entitlements. Staple/notarize only the exact DMG being distributed.

For diagnostic requests, explain without modifying code. For implementation requests, patch, validate proportionally, and call out any manual deployment step.
