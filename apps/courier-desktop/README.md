# Courier Desktop

The Tauri 2 and Svelte desktop interface for Icy Seas Courier. Inventory and transfer state are provided by `courier-core`; the frontend contains no hashing, persistence, or transfer logic.

## Development

```bash
../../scripts/dev-up.sh
npm install
npm run check
npm run tauri dev
```

The general workspace home shows saved project access, recent transfer history, and resumable work before asking the user to start anything new. The interface validates and stores the selected Registry endpoint and exchanges project-scoped Registry invitations. Upload invitations present only authorized projects, register immutable manifests, and upload directly through short-lived part-specific URLs. Download invitations are read-only: they list completed, verified datasets in the authorized projects, then stream, safely unpack, and independently re-verify a selected dataset into a native destination folder. Each new upload is bound to its originating Registry so changing the active endpoint cannot redirect a resume. The app also provides real Rust inventory, a persistent phase-and-heartbeat activity display, measured upload/download throughput, separate in-flight and durably confirmed byte counts, immediate in-flight request cancellation on pause, and reconciliation-backed resume. Transport packaging uses zstd for every file below 8 MiB and tests larger files before retaining a compressed representation; already-compressed large files fall back to their original bytes. Upload users can remember an **Analyze and upload** preference that starts transfer automatically after inventory, hashing, and packaging finish successfully; the default still pauses for review.

Courier defaults to the hosted HTTPS Registry at `https://courier.icyseascolab.io`. Set `COURIER_REGISTRY_URL` for local development or another explicitly configured Registry. Plain HTTP is accepted only for loopback development. Courier stores rotating access and refresh credentials in its OS/app-local credential storage; on macOS, the current build uses a mode-`0600` file in Courier's local app-data directory to avoid launch-policy problems with optional Keychain entitlements. Touch ID, with the normal system-password fallback, unlocks saved project access once for the running app session; token reads and rotations do not produce additional password dialogs. Local SQLite contains only the active endpoint, a non-reversible invitation scope for each transfer, per-transfer Registry assignment, session expiry/project metadata, and Registry transfer/object identities, allowing multiple invitations at one Registry and idempotent registration after restart. Dataset bytes never pass through FastAPI.

## Releases

Signed macOS DMGs, a Windows NSIS installer, and a Linux AppImage are produced by the version-tagged GitHub Actions release workflow. See [Courier Desktop releases](../../docs/desktop-releases.md) for Apple signing setup, release creation, and verification.
