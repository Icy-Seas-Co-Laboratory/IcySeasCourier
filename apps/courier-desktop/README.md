# Courier Desktop

The Tauri 2 and Svelte desktop interface for Icy Seas Courier. Inventory and transfer state are provided by `courier-core`; the frontend contains no hashing, persistence, or transfer logic.

## Development

```bash
../../scripts/dev-up.sh
npm install
npm run check
npm run tauri dev
```

The interface exchanges Registry invitations, presents only authorized projects, registers immutable manifests, and uploads directly through short-lived part-specific URLs. It also provides native directory selection, real Rust inventory, live progress, cooperative pause, and reconciliation-backed resume.

Set `COURIER_REGISTRY_URL` to override the default `http://127.0.0.1:8010`. Courier stores rotating access and refresh credentials in the operating system credential vault. Local SQLite contains only session expiry/project metadata and Registry transfer/file identities, allowing idempotent registration and accepted-part reconciliation after restart. Dataset bytes never pass through FastAPI.
