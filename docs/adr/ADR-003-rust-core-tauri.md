# ADR-003: Rust core with interface adapters

Status: Accepted

Transfer behavior belongs in independent Rust crates. The CLI, future Tauri 2 desktop UI, headless clients, and instrument uploaders call the same core rather than reimplementing reliability logic.

