# Courier Desktop

The Tauri 2 and Svelte desktop interface for Icy Seas Courier. Inventory and transfer state are provided by `courier-core`; the frontend contains no hashing, persistence, or transfer logic.

## Development

```bash
../../scripts/dev-up.sh
npm install
npm run check
npm run tauri dev
```

The interface validates and stores the selected Registry endpoint, exchanges Registry invitations, presents only authorized projects, registers immutable manifests, and uploads directly through short-lived part-specific URLs. Each new transfer is bound to its originating Registry so changing the active endpoint cannot redirect a resume. It also provides native directory selection, real Rust inventory, live progress, cooperative pause, and reconciliation-backed resume.

Enter the supplied HTTPS Registry address in Courier. `COURIER_REGISTRY_URL` remains a development default only; it is no longer required for a packaged application. Plain HTTP is accepted only for loopback development. Courier stores rotating access and refresh credentials in the operating system credential vault. Local SQLite contains only the active endpoint, per-transfer Registry assignment, session expiry/project metadata, and Registry transfer/object identities, allowing idempotent registration and accepted-part reconciliation after restart. Dataset bytes never pass through FastAPI.
