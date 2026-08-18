use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use courier_core::{
    HashAlgorithm, InventoryOptions, RegistrySessionRecord, RetryPolicy, Transfer, TransferStatus,
    TransferStore, inventory_transfer_observed,
};
use courier_registry::{
    RegistryClient, RegistryFileBinding, RegistryMultipartStore, RegistryProject,
};
use courier_transfer::{
    MultipartLimits, PartUploadEvent, UploadError, UploadObserver, complete_uploaded_file,
    plan_parts, upload_missing_parts_observed,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

#[derive(Default)]
struct RuntimeState {
    controls: Mutex<HashMap<Uuid, Arc<AtomicBool>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferProgressEvent {
    transfer_id: Uuid,
    confirmed_bytes: u64,
    total_bytes: u64,
    current_file: String,
    status: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InventoryProgressEvent {
    transfer_id: Uuid,
    files_analyzed: u64,
    total_files: u64,
    bytes_analyzed: u64,
    total_bytes: u64,
    current_path: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryAuthorization {
    expires_at: chrono::DateTime<chrono::Utc>,
    projects: Vec<RegistryProject>,
    hash_algorithm: HashAlgorithm,
}

#[derive(Clone, Serialize, Deserialize)]
struct RegistryCredentials {
    access_token: String,
    refresh_token: String,
}

const CREDENTIAL_SERVICE: &str = "co.icyseas.courier.registry";

fn credential_entry(base_url: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(CREDENTIAL_SERVICE, base_url).map_err(display)
}

fn save_credentials(base_url: &str, credentials: &RegistryCredentials) -> Result<(), String> {
    let encoded = serde_json::to_string(credentials).map_err(display)?;
    credential_entry(base_url)?
        .set_password(&encoded)
        .map_err(display)
}

fn load_credentials(base_url: &str) -> Result<Option<RegistryCredentials>, String> {
    match credential_entry(base_url)?.get_password() {
        Ok(encoded) => serde_json::from_str(&encoded).map(Some).map_err(display),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(display(error)),
    }
}

fn session_record(
    base_url: String,
    session: &courier_registry::RegistrySession,
) -> Result<RegistrySessionRecord, String> {
    Ok(RegistrySessionRecord {
        base_url,
        expires_at: session.expires_at,
        refresh_expires_at: session.refresh_expires_at,
        projects_json: serde_json::to_string(&session.projects).map_err(display)?,
    })
}

fn persist_registry_session(
    store: &TransferStore,
    base_url: &str,
    session: &courier_registry::RegistrySession,
) -> Result<(), String> {
    save_credentials(
        base_url,
        &RegistryCredentials {
            access_token: session.access_token.clone(),
            refresh_token: session.refresh_token.clone(),
        },
    )?;
    store
        .save_registry_session(&session_record(base_url.to_owned(), session)?)
        .map_err(display)
}

async fn active_registry_session(
    store: &TransferStore,
    base_url: &str,
) -> Result<(RegistryClient, RegistrySessionRecord), String> {
    let metadata = store
        .registry_session(base_url)
        .map_err(display)?
        .ok_or_else(|| "Enter a Registry invitation to authorize this device".to_string())?;
    let credentials = load_credentials(base_url)?.ok_or_else(|| {
        "Registry credentials are unavailable in the operating system credential vault; enter a new invitation"
            .to_string()
    })?;
    if metadata.refresh_expires_at <= chrono::Utc::now() {
        return Err("Registry authorization expired; enter a new invitation".into());
    }
    if metadata.expires_at > chrono::Utc::now() + chrono::Duration::minutes(5) {
        return Ok((
            RegistryClient::authenticated(base_url, credentials.access_token),
            metadata,
        ));
    }
    let refreshed = RegistryClient::unauthenticated(base_url)
        .refresh_session(&credentials.refresh_token)
        .await
        .map_err(display)?;
    persist_registry_session(store, base_url, &refreshed)?;
    let metadata = session_record(base_url.to_owned(), &refreshed)?;
    Ok((
        RegistryClient::authenticated(base_url, refreshed.access_token),
        metadata,
    ))
}

fn registry_url() -> String {
    std::env::var("COURIER_REGISTRY_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8010".into())
        .trim_end_matches('/')
        .to_owned()
}

#[tauri::command]
async fn exchange_invitation(
    app: AppHandle,
    invitation_code: String,
) -> Result<RegistryAuthorization, String> {
    let base_url = registry_url();
    let remote = RegistryClient::unauthenticated(&base_url)
        .exchange_invitation(invitation_code.trim(), "courier-desktop")
        .await
        .map_err(display)?;
    let authorization = RegistryAuthorization {
        expires_at: remote.expires_at,
        projects: remote.projects.clone(),
        hash_algorithm: RegistryClient::unauthenticated(&base_url)
            .system_config()
            .await
            .map_err(display)?
            .hash_algorithm,
    };
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let store = TransferStore::open(database).map_err(display)?;
        persist_registry_session(&store, &base_url, &remote)
    })
    .await
    .map_err(|error| format!("Session save failed: {error}"))?
    .map_err(display)?;
    Ok(authorization)
}

#[tauri::command]
async fn current_authorization(app: AppHandle) -> Result<Option<RegistryAuthorization>, String> {
    let database = database_path(&app)?;
    let base_url = registry_url();
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(database).map_err(display)?;
        tauri::async_runtime::block_on(async {
            match active_registry_session(&store, &base_url).await {
                Ok((_, record)) => Ok(Some(RegistryAuthorization {
                    expires_at: record.expires_at,
                    projects: serde_json::from_str(&record.projects_json).map_err(display)?,
                    hash_algorithm: RegistryClient::unauthenticated(&base_url)
                        .system_config()
                        .await
                        .map_err(display)?
                        .hash_algorithm,
                })),
                Err(error)
                    if error.contains("enter a new invitation")
                        || error.contains("Enter a Registry invitation") =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })
    })
    .await
    .map_err(|error| format!("Session lookup failed: {error}"))?
}

struct DesktopObserver {
    app: AppHandle,
    transfer_id: Uuid,
    pause: Arc<AtomicBool>,
    confirmed: Arc<AtomicU64>,
    base_confirmed: u64,
    total: u64,
    current_file: String,
}

impl UploadObserver for DesktopObserver {
    fn should_pause(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }

    fn part_confirmed(&self, event: PartUploadEvent) {
        let confirmed = self
            .confirmed
            .fetch_add(event.source_bytes, Ordering::Relaxed)
            .saturating_add(event.source_bytes);
        let _ = self.app.emit(
            "courier://progress",
            TransferProgressEvent {
                transfer_id: self.transfer_id,
                confirmed_bytes: confirmed,
                total_bytes: self.total,
                current_file: self.current_file.clone(),
                status: "uploading",
            },
        );
    }

    fn reconciled(&self, source_bytes_confirmed: u64) {
        let confirmed = self.base_confirmed.saturating_add(source_bytes_confirmed);
        self.confirmed.store(confirmed, Ordering::Relaxed);
        let _ = self.app.emit(
            "courier://progress",
            TransferProgressEvent {
                transfer_id: self.transfer_id,
                confirmed_bytes: confirmed,
                total_bytes: self.total,
                current_file: self.current_file.clone(),
                status: "uploading",
            },
        );
    }
}

#[tauri::command]
async fn create_inventory(
    app: AppHandle,
    source_path: String,
    project_id: Option<String>,
    hash_algorithm: HashAlgorithm,
) -> Result<Transfer, String> {
    let database = database_path(&app)?;
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let source = PathBuf::from(&source_path)
            .canonicalize()
            .map_err(|error| format!("Could not open source: {error}"))?;
        let mut store = TransferStore::open(database).map_err(display)?;
        let transfer = Transfer::draft(source.clone(), project_id);
        store.create_transfer(&transfer).map_err(display)?;
        store
            .transition(transfer.id, TransferStatus::Inventorying)
            .map_err(display)?;
        match inventory_transfer_observed(
            transfer.id,
            &source,
            &InventoryOptions {
                hash_algorithm,
                ..InventoryOptions::default()
            },
            |progress| {
                let _ = worker_app.emit(
                    "courier://inventory-progress",
                    InventoryProgressEvent {
                        transfer_id: transfer.id,
                        files_analyzed: progress.files_analyzed,
                        total_files: progress.total_files,
                        bytes_analyzed: progress.bytes_analyzed,
                        total_bytes: progress.total_bytes,
                        current_path: progress.current_path.to_string_lossy().into_owned(),
                    },
                );
            },
        ) {
            Ok(files) => {
                store
                    .replace_inventory(transfer.id, &files)
                    .map_err(display)?;
                for file in &files {
                    let parts = plan_parts(file.id, file.size, MultipartLimits::default())
                        .map_err(display)?;
                    store.replace_part_plan(file.id, &parts).map_err(display)?;
                }
                store
                    .transition(transfer.id, TransferStatus::Ready)
                    .map_err(display)?;
                store
                    .get_transfer(transfer.id)
                    .map_err(display)?
                    .ok_or_else(|| "Inventory disappeared from local state".to_string())
            }
            Err(error) => {
                store
                    .transition(transfer.id, TransferStatus::Failed)
                    .map_err(display)?;
                Err(error.to_string())
            }
        }
    })
    .await
    .map_err(|error| format!("Inventory task failed: {error}"))?
}

#[tauri::command]
async fn list_transfers(app: AppHandle) -> Result<Vec<Transfer>, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        TransferStore::open(database)
            .and_then(|store| store.list_transfers())
            .map_err(display)
    })
    .await
    .map_err(|error| format!("Transfer lookup failed: {error}"))?
}

#[tauri::command]
async fn refresh_transfer_status(app: AppHandle, transfer_id: Uuid) -> Result<Transfer, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(database).map_err(display)?;
        let transfer = store
            .get_transfer(transfer_id)
            .map_err(display)?
            .ok_or_else(|| format!("Transfer not found: {transfer_id}"))?;
        let Some(server_transfer_id) = transfer.server_transfer_id.as_deref() else {
            return Ok(transfer);
        };
        let base_url = registry_url();
        tauri::async_runtime::block_on(async {
            let (client, _) = active_registry_session(&store, &base_url).await?;
            let remote = client
                .transfer_status(server_transfer_id)
                .await
                .map_err(display)?;
            let mut current = transfer.status;
            let target = match remote.status.as_str() {
                "verifying" => Some(TransferStatus::Verifying),
                "complete" => Some(TransferStatus::Complete),
                "failed" => Some(TransferStatus::Failed),
                _ => None,
            };
            if target == Some(TransferStatus::Complete) && current == TransferStatus::Finalizing {
                store
                    .transition(transfer_id, TransferStatus::Verifying)
                    .map_err(display)?;
                current = TransferStatus::Verifying;
            }
            if let Some(target) = target.filter(|target| *target != current) {
                store.transition(transfer_id, target).map_err(display)?;
            }
            store
                .get_transfer(transfer_id)
                .map_err(display)?
                .ok_or_else(|| "Transfer disappeared from local state".to_string())
        })
    })
    .await
    .map_err(|error| format!("Status refresh failed: {error}"))?
}

#[tauri::command]
async fn start_upload(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    transfer_id: Uuid,
) -> Result<Transfer, String> {
    let database = database_path(&app)?;
    let pause = Arc::new(AtomicBool::new(false));
    runtime
        .controls
        .lock()
        .map_err(|_| "Upload controls are unavailable".to_string())?
        .insert(transfer_id, pause.clone());

    let worker_app = app.clone();
    let worker = tauri::async_runtime::spawn_blocking(move || {
        run_upload(worker_app, database, transfer_id, pause)
    })
    .await;

    runtime
        .controls
        .lock()
        .map_err(|_| "Upload controls are unavailable".to_string())?
        .remove(&transfer_id);
    worker.map_err(|error| format!("Upload task failed: {error}"))?
}

fn run_upload(
    app: AppHandle,
    database: PathBuf,
    transfer_id: Uuid,
    pause: Arc<AtomicBool>,
) -> Result<Transfer, String> {
    let store = TransferStore::open(database).map_err(display)?;
    let transfer = store
        .get_transfer(transfer_id)
        .map_err(display)?
        .ok_or_else(|| format!("Transfer not found: {transfer_id}"))?;
    match transfer.status {
        TransferStatus::Ready | TransferStatus::Paused | TransferStatus::Interrupted => store
            .transition(transfer_id, TransferStatus::Uploading)
            .map_err(display)?,
        TransferStatus::Uploading => {}
        status => return Err(format!("Cannot upload a transfer in state {status}")),
    }

    let retry = RetryPolicy::default();
    let files = store.files_for_transfer(transfer_id).map_err(display)?;
    let confirmed = Arc::new(AtomicU64::new(0));
    let result: Result<(), String> = tauri::async_runtime::block_on(async {
        let base_url = registry_url();
        let (client, _) = active_registry_session(&store, &base_url).await?;
        let project_code = transfer
            .project_id
            .as_deref()
            .ok_or_else(|| "Transfer has no Registry project".to_string())?;
        let source_name = transfer
            .source_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dataset");
        let server_transfer_id = match &transfer.server_transfer_id {
            Some(id) => id.clone(),
            None => {
                let registered = client
                    .register_transfer(
                        &transfer,
                        project_code,
                        source_name,
                        files
                            .first()
                            .map(|file| file.hash_algorithm)
                            .unwrap_or_default(),
                    )
                    .await
                    .map_err(display)?;
                store
                    .bind_registry_transfer(transfer.id, &registered.public_id)
                    .map_err(display)?;
                registered.public_id
            }
        };
        let receipt = client
            .submit_manifest(
                &transfer,
                &server_transfer_id,
                project_code,
                source_name,
                &files,
            )
            .await
            .map_err(display)?;
        for local in &files {
            let path = local.relative_path.to_string_lossy().replace('\\', "/");
            let registered = receipt
                .files
                .iter()
                .find(|file| file.relative_path == path)
                .ok_or_else(|| format!("Registry omitted manifest file {path}"))?;
            store
                .bind_registry_file(local.id, registered.id, &registered.object_key)
                .map_err(display)?;
        }
        let bindings = files
            .iter()
            .map(|file| {
                let (server_file_id, object_key) = store
                    .registry_file_binding(file.id)
                    .map_err(display)?
                    .ok_or_else(|| format!("Registry binding missing for {}", file.id))?;
                Ok(RegistryFileBinding {
                    server_file_id,
                    object_key,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let remote = RegistryMultipartStore::new(client.clone(), &server_transfer_id, bindings);
        for file in &files {
            if file.status == courier_core::FileStatus::Uploaded {
                confirmed.fetch_add(file.size, Ordering::Relaxed);
                continue;
            }
            let base_confirmed = confirmed.load(Ordering::Relaxed);
            let observer = DesktopObserver {
                app: app.clone(),
                transfer_id,
                pause: pause.clone(),
                confirmed: confirmed.clone(),
                base_confirmed,
                total: transfer.original_bytes,
                current_file: file.relative_path.to_string_lossy().into_owned(),
            };
            upload_missing_parts_observed(&store, &remote, file, &retry, &observer)
                .await
                .map_err(display)?;
            if observer.should_pause() {
                return Err(UploadError::Paused.to_string());
            }
            complete_uploaded_file(&store, &remote, file, &retry)
                .await
                .map_err(display)?;
            store.mark_file_uploaded(file.id).map_err(display)?;
        }
        client
            .finalize_transfer(&server_transfer_id)
            .await
            .map_err(display)?;
        Ok(())
    });

    match result {
        Ok(()) => {
            store
                .transition(transfer_id, TransferStatus::Finalizing)
                .map_err(display)?;
            emit_status(
                &app,
                transfer_id,
                transfer.original_bytes,
                transfer.original_bytes,
                "finalizing",
            );
        }
        Err(error) if error == UploadError::Paused.to_string() => {
            store
                .transition(transfer_id, TransferStatus::Paused)
                .map_err(display)?;
            emit_status(
                &app,
                transfer_id,
                confirmed.load(Ordering::Relaxed),
                transfer.original_bytes,
                "paused",
            );
        }
        Err(error) => {
            store
                .transition(transfer_id, TransferStatus::Interrupted)
                .map_err(display)?;
            emit_status(
                &app,
                transfer_id,
                confirmed.load(Ordering::Relaxed),
                transfer.original_bytes,
                "interrupted",
            );
            return Err(error);
        }
    }
    store
        .get_transfer(transfer_id)
        .map_err(display)?
        .ok_or_else(|| "Transfer disappeared from local state".to_string())
}

#[tauri::command]
fn pause_upload(runtime: State<'_, RuntimeState>, transfer_id: Uuid) -> Result<(), String> {
    let controls = runtime
        .controls
        .lock()
        .map_err(|_| "Upload controls are unavailable".to_string())?;
    let pause = controls
        .get(&transfer_id)
        .ok_or_else(|| "Transfer is not currently uploading".to_string())?;
    pause.store(true, Ordering::Relaxed);
    Ok(())
}

fn emit_status(
    app: &AppHandle,
    transfer_id: Uuid,
    confirmed_bytes: u64,
    total_bytes: u64,
    status: &'static str,
) {
    let _ = app.emit(
        "courier://progress",
        TransferProgressEvent {
            transfer_id,
            confirmed_bytes,
            total_bytes,
            current_file: String::new(),
            status,
        },
    );
}

fn database_path(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app.path().app_local_data_dir().map_err(display)?;
    fs::create_dir_all(&directory).map_err(display)?;
    Ok(directory.join("courier.db"))
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(RuntimeState::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            exchange_invitation,
            current_authorization,
            create_inventory,
            list_transfers,
            refresh_transfer_status,
            start_upload,
            pause_upload
        ])
        .run(tauri::generate_context!())
        .expect("error while running Icy Seas Courier");
}
