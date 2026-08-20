# Courier Desktop

The Tauri 2 and Svelte desktop interface for Icy Seas Courier. Inventory and transfer state are provided by `courier-core`; the frontend contains no hashing, persistence, or transfer logic.

## Development

```bash
../../scripts/dev-up.sh
npm install
npm run check
npm run tauri dev
```

The interface validates and stores the selected Registry endpoint and exchanges project-scoped Registry invitations. Upload invitations present only authorized projects, register immutable manifests, and upload directly through short-lived part-specific URLs. Download invitations are read-only: they list completed, verified datasets in the authorized projects, then stream, safely unpack, and independently re-verify a selected dataset into a native destination folder. Each new upload is bound to its originating Registry so changing the active endpoint cannot redirect a resume. The app also provides real Rust inventory, live progress, cooperative pause, and reconciliation-backed resume.

Enter the supplied HTTPS Registry address in Courier. `COURIER_REGISTRY_URL` remains a development default only; it is no longer required for a packaged application. Plain HTTP is accepted only for loopback development. Courier stores rotating access and refresh credentials in the operating system credential vault. Local SQLite contains only the active endpoint, per-transfer Registry assignment, session expiry/project metadata, and Registry transfer/object identities, allowing idempotent registration and accepted-part reconciliation after restart. Dataset bytes never pass through FastAPI.

## Releases

Signed macOS DMGs, a Windows NSIS installer, and a Linux AppImage are produced by the version-tagged GitHub Actions release workflow. See [Courier Desktop releases](../../docs/desktop-releases.md) for Apple signing setup, release creation, and verification.
