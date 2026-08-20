use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use courier_core::{
    FileRecord, FileStatus, HashAlgorithm, InventoryOptions, RegistrySessionRecord, RetryPolicy,
    Transfer, TransferStatus, TransferStore, TransportMemberRecord, TransportObjectKind,
    TransportObjectRecord, digest_file, inventory_transfer_observed,
};
use courier_pack::{PackOptions, decode_pack, encode_pack, plan_packs};
use courier_registry::{
    ManifestTransportPlan, RegistryClient, RegistryDownloadDataset, RegistryDownloadPlan,
    RegistryInvitationPurpose, RegistryMultipartStore, RegistryObjectBinding, RegistryProject,
};
use courier_transfer::{
    MultipartLimits, PartUploadEvent, UploadError, UploadObserver, complete_uploaded_file,
    plan_parts, upload_missing_parts_observed,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use url::{Host, Url};
use uuid::Uuid;

struct RuntimeState {
    controls: Mutex<HashMap<Uuid, Arc<AtomicBool>>>,
    credentials: Arc<Mutex<HashMap<String, RegistryCredentials>>>,
    session_gate: Arc<tokio::sync::Mutex<()>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            controls: Mutex::new(HashMap::new()),
            credentials: Arc::new(Mutex::new(HashMap::new())),
            session_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferProgressEvent {
    transfer_id: Uuid,
    confirmed_bytes: u64,
    sent_bytes: u64,
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
    phase: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferSizes {
    original_bytes: u64,
    transport_bytes: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryAuthorization {
    registry_url: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    projects: Vec<RegistryProject>,
    hash_algorithm: HashAlgorithm,
    purpose: RegistryInvitationPurpose,
    downloads: Vec<RegistryDownloadDataset>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgressEvent {
    transfer_id: String,
    received_bytes: u64,
    total_bytes: u64,
    restored_files: u64,
    total_files: u64,
    current_file: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadResult {
    transfer_id: String,
    destination: String,
    restored_files: u64,
    original_bytes: u64,
    transport_bytes: u64,
}

#[derive(Deserialize)]
struct DownloadManifest {
    files: Vec<DownloadManifestFile>,
}

#[derive(Deserialize)]
struct DownloadManifestFile {
    path: String,
    size: u64,
    mtime: chrono::DateTime<chrono::Utc>,
    digest: DownloadDigest,
    transport: DownloadTransport,
}

#[derive(Deserialize)]
struct DownloadDigest {
    algorithm: HashAlgorithm,
    value: String,
}

#[derive(Deserialize)]
struct DownloadTransport {
    object_id: Uuid,
    member_index: u32,
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

fn load_credentials(
    base_url: &str,
    cache: &Mutex<HashMap<String, RegistryCredentials>>,
) -> Result<Option<RegistryCredentials>, String> {
    if let Some(credentials) = cache
        .lock()
        .map_err(|_| "Registry credential cache is unavailable".to_string())?
        .get(base_url)
        .cloned()
    {
        return Ok(Some(credentials));
    }
    match credential_entry(base_url)?.get_password() {
        Ok(encoded) => {
            let credentials: RegistryCredentials =
                serde_json::from_str(&encoded).map_err(display)?;
            cache
                .lock()
                .map_err(|_| "Registry credential cache is unavailable".to_string())?
                .insert(base_url.to_owned(), credentials.clone());
            Ok(Some(credentials))
        }
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
    cache: &Mutex<HashMap<String, RegistryCredentials>>,
) -> Result<(), String> {
    let credentials = RegistryCredentials {
        access_token: session.access_token.clone(),
        refresh_token: session.refresh_token.clone(),
    };
    save_credentials(base_url, &credentials)?;
    cache
        .lock()
        .map_err(|_| "Registry credential cache is unavailable".to_string())?
        .insert(base_url.to_owned(), credentials);
    store
        .save_registry_session(&session_record(base_url.to_owned(), session)?)
        .map_err(display)
}

async fn active_registry_session(
    store: &TransferStore,
    base_url: &str,
    database: &std::path::Path,
    credential_cache: &Arc<Mutex<HashMap<String, RegistryCredentials>>>,
    session_gate: &Arc<tokio::sync::Mutex<()>>,
) -> Result<(RegistryClient, RegistrySessionRecord), String> {
    // Credential refresh tokens rotate. Keep lookup and refresh serialized so concurrent
    // status polls cannot prompt repeatedly or attempt to reuse the same refresh token.
    let _session_guard = session_gate.lock().await;
    let metadata = store
        .registry_session(base_url)
        .map_err(display)?
        .ok_or_else(|| "Enter a Registry invitation to authorize this device".to_string())?;
    let credentials = load_credentials(base_url, credential_cache)?.ok_or_else(|| {
        "Registry credentials are unavailable in the operating system credential vault; enter a new invitation"
            .to_string()
    })?;
    if metadata.refresh_expires_at <= chrono::Utc::now() {
        return Err("Registry authorization expired; enter a new invitation".into());
    }
    if metadata.expires_at > chrono::Utc::now() + chrono::Duration::minutes(5) {
        let observer_database = database.to_path_buf();
        let observer_url = base_url.to_owned();
        let observer_cache = credential_cache.clone();
        return Ok((
            RegistryClient::renewable(
                base_url,
                credentials.access_token,
                credentials.refresh_token,
                Arc::new(move |session| {
                    let store = TransferStore::open(&observer_database).map_err(display)?;
                    persist_registry_session(&store, &observer_url, session, &observer_cache)
                }),
            ),
            metadata,
        ));
    }
    let refreshed = RegistryClient::unauthenticated(base_url)
        .refresh_session(&credentials.refresh_token)
        .await
        .map_err(display)?;
    persist_registry_session(store, base_url, &refreshed, credential_cache)?;
    let metadata = session_record(base_url.to_owned(), &refreshed)?;
    let observer_database = database.to_path_buf();
    let observer_url = base_url.to_owned();
    let observer_cache = credential_cache.clone();
    Ok((
        RegistryClient::renewable(
            base_url,
            refreshed.access_token,
            refreshed.refresh_token,
            Arc::new(move |session| {
                let store = TransferStore::open(&observer_database).map_err(display)?;
                persist_registry_session(&store, &observer_url, session, &observer_cache)
            }),
        ),
        metadata,
    ))
}

fn normalize_registry_url(value: &str) -> Result<String, String> {
    let parsed = Url::parse(value.trim()).map_err(|_| "Enter a valid Registry URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Registry URL must use HTTPS".into());
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err("Registry URL must contain only a scheme, host, and optional port".into());
    }
    let host = parsed
        .host()
        .ok_or_else(|| "Registry URL must include a host".to_string())?;
    let is_loopback = match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    };
    if parsed.scheme() != "https" && !is_loopback {
        return Err("Remote Registry connections require HTTPS".into());
    }
    Ok(parsed.origin().ascii_serialization())
}

fn default_registry_url() -> Result<String, String> {
    normalize_registry_url(
        &std::env::var("COURIER_REGISTRY_URL").unwrap_or_else(|_| "http://127.0.0.1:8020".into()),
    )
}

fn configured_registry_url(store: &TransferStore) -> Result<String, String> {
    match store.active_registry().map_err(display)? {
        Some(value) => normalize_registry_url(&value),
        None => default_registry_url(),
    }
}

#[tauri::command]
async fn registry_endpoint(app: AppHandle) -> Result<String, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(database).map_err(display)?;
        configured_registry_url(&store)
    })
    .await
    .map_err(|error| format!("Registry setting lookup failed: {error}"))?
}

#[tauri::command]
async fn exchange_invitation(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    registry_url: String,
    invitation_code: String,
) -> Result<RegistryAuthorization, String> {
    let base_url = normalize_registry_url(&registry_url)?;
    let remote = RegistryClient::unauthenticated(&base_url)
        .exchange_invitation(invitation_code.trim(), "courier-desktop")
        .await
        .map_err(display)?;
    let downloads = if remote.purpose == RegistryInvitationPurpose::Download {
        RegistryClient::authenticated(&base_url, &remote.access_token)
            .downloadable_datasets()
            .await
            .map_err(display)?
    } else {
        Vec::new()
    };
    let authorization = RegistryAuthorization {
        registry_url: base_url.clone(),
        expires_at: remote.expires_at,
        projects: remote.projects.clone(),
        purpose: remote.purpose,
        downloads,
        hash_algorithm: RegistryClient::unauthenticated(&base_url)
            .system_config()
            .await
            .map_err(display)?
            .hash_algorithm,
    };
    let database = database_path(&app)?;
    let credential_cache = runtime.credentials.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let store = TransferStore::open(&database).map_err(display)?;
        persist_registry_session(&store, &base_url, &remote, &credential_cache)?;
        store.set_active_registry(&base_url).map_err(display)
    })
    .await
    .map_err(|error| format!("Session save failed: {error}"))?
    .map_err(display)?;
    Ok(authorization)
}

#[tauri::command]
async fn current_authorization(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<Option<RegistryAuthorization>, String> {
    let database = database_path(&app)?;
    let credential_cache = runtime.credentials.clone();
    let session_gate = runtime.session_gate.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let base_url = configured_registry_url(&store)?;
        tauri::async_runtime::block_on(async {
            match active_registry_session(
                &store,
                &base_url,
                &database,
                &credential_cache,
                &session_gate,
            )
            .await
            {
                Ok((client, record)) => {
                    let remote = client.session_authorization().await.map_err(display)?;
                    let downloads = if remote.purpose == RegistryInvitationPurpose::Download {
                        client.downloadable_datasets().await.map_err(display)?
                    } else {
                        Vec::new()
                    };
                    Ok(Some(RegistryAuthorization {
                        registry_url: base_url.clone(),
                        expires_at: record.expires_at,
                        projects: remote.projects,
                        purpose: remote.purpose,
                        downloads,
                        hash_algorithm: RegistryClient::unauthenticated(&base_url)
                            .system_config()
                            .await
                            .map_err(display)?
                            .hash_algorithm,
                    }))
                }
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

fn safe_relative_path(root: &Path, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains('\\') {
        return Err(format!("Manifest contains an unsafe path: {value}"));
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!("Manifest contains an unsafe path: {value}"));
    }
    Ok(root.join(path))
}

fn safe_dataset_name(source_name: &str, transfer_id: &str) -> String {
    let candidate = Path::new(source_name);
    match (candidate.file_name(), candidate.components().count()) {
        (Some(name), 1) if !name.is_empty() => name.to_string_lossy().into_owned(),
        _ => transfer_id.to_owned(),
    }
}

fn validate_download_manifest(manifest: &DownloadManifest) -> Result<(), String> {
    let mut paths = HashSet::new();
    for file in &manifest.files {
        safe_relative_path(Path::new("."), &file.path)?;
        if !paths.insert(file.path.to_lowercase()) {
            return Err(format!(
                "Manifest contains a duplicate or case-colliding path: {}",
                file.path
            ));
        }
    }
    Ok(())
}

fn verify_restored_file(path: &Path, file: &DownloadManifestFile) -> Result<(), String> {
    let metadata = path.metadata().map_err(display)?;
    if metadata.len() != file.size {
        return Err(format!("Restored size mismatch for {}", file.path));
    }
    let actual = digest_file(path, file.digest.algorithm).map_err(display)?;
    if actual != file.digest.value {
        return Err(format!("Restored digest mismatch for {}", file.path));
    }
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(
            file.mtime.timestamp(),
            file.mtime.timestamp_subsec_nanos(),
        ),
    )
    .map_err(display)
}

fn emit_download_progress(
    app: &AppHandle,
    plan: &RegistryDownloadPlan,
    received_bytes: u64,
    restored_files: u64,
    current_file: String,
) {
    let _ = app.emit(
        "courier://download-progress",
        DownloadProgressEvent {
            transfer_id: plan.dataset.transfer_id.clone(),
            received_bytes,
            total_bytes: plan.dataset.transport_bytes.unwrap_or(0),
            restored_files,
            total_files: plan.dataset.file_count,
            current_file,
        },
    );
}

async fn restore_download_plan(
    app: &AppHandle,
    client: &RegistryClient,
    plan: &RegistryDownloadPlan,
    partial: &Path,
) -> Result<(u64, u64), String> {
    let manifest: DownloadManifest =
        serde_json::from_value(plan.manifest.clone()).map_err(display)?;
    validate_download_manifest(&manifest)?;
    if manifest.files.len() as u64 != plan.dataset.file_count {
        return Err("Download manifest file count does not match the verified dataset".into());
    }

    let cache = partial.join(".courier-transport");
    fs::create_dir_all(&cache).map_err(display)?;
    let mut by_object: HashMap<Uuid, Vec<&DownloadManifestFile>> = HashMap::new();
    for file in &manifest.files {
        by_object
            .entry(file.transport.object_id)
            .or_default()
            .push(file);
    }
    for files in by_object.values_mut() {
        files.sort_by_key(|file| file.transport.member_index);
    }

    let mut received_total = 0_u64;
    let mut restored = 0_u64;
    for object in &plan.objects {
        let authorization = client
            .authorize_download_object(&plan.dataset.transfer_id, object.object_id)
            .await
            .map_err(display)?;
        let url = authorization
            .url
            .ok_or_else(|| "Registry omitted the authorized object URL".to_string())?;
        let cache_path = cache.join(object.object_id.to_string());
        let before = received_total;
        let app_for_progress = app.clone();
        let transfer_for_progress = plan.dataset.transfer_id.clone();
        let total_transport = plan.dataset.transport_bytes.unwrap_or(0);
        let total_files = plan.dataset.file_count;
        let restored_before = restored;
        let received = client
            .download_object(&url, &cache_path, move |object_received| {
                let _ = app_for_progress.emit(
                    "courier://download-progress",
                    DownloadProgressEvent {
                        transfer_id: transfer_for_progress.clone(),
                        received_bytes: before.saturating_add(object_received),
                        total_bytes: total_transport,
                        restored_files: restored_before,
                        total_files,
                        current_file: "Downloading verified transport…".into(),
                    },
                );
            })
            .await
            .map_err(display)?;
        if let Some(expected) = object.transport_bytes
            && received != expected
        {
            return Err(format!(
                "Downloaded object {} has an unexpected size",
                object.object_id
            ));
        }
        received_total = received_total.saturating_add(received);
        let expected_files = by_object
            .remove(&object.object_id)
            .ok_or_else(|| format!("Manifest does not reference object {}", object.object_id))?;

        match object.kind.as_str() {
            "file" => {
                if expected_files.len() != 1 || expected_files[0].transport.member_index != 0 {
                    return Err(format!(
                        "Standalone object {} has invalid membership",
                        object.object_id
                    ));
                }
                let file = expected_files[0];
                let destination = safe_relative_path(partial, &file.path)?;
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(display)?;
                }
                fs::copy(&cache_path, &destination).map_err(display)?;
                verify_restored_file(&destination, file)?;
                restored = restored.saturating_add(1);
                emit_download_progress(app, plan, received_total, restored, file.path.clone());
            }
            "pack" => {
                let mut seen = 0_usize;
                decode_pack(
                    File::open(&cache_path).map_err(display)?,
                    |header, reader| {
                        let file = expected_files.get(seen).ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "pack has extra members",
                            )
                        })?;
                        if header.path != file.path
                            || header.size != file.size
                            || header.digest_algorithm != file.digest.algorithm
                            || header.digest != file.digest.value
                            || file.transport.member_index as usize != seen
                        {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "pack member does not match the immutable manifest",
                            )
                            .into());
                        }
                        let destination =
                            safe_relative_path(partial, &file.path).map_err(|error| {
                                std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                            })?;
                        if let Some(parent) = destination.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        let mut output = File::create(&destination)?;
                        let copied = std::io::copy(reader, &mut output)?;
                        if copied != file.size {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "pack member size mismatch",
                            )
                            .into());
                        }
                        verify_restored_file(&destination, file).map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                        })?;
                        seen += 1;
                        Ok(())
                    },
                )
                .map_err(display)?;
                if seen != expected_files.len() {
                    return Err(format!(
                        "Pack {} omitted manifest members",
                        object.object_id
                    ));
                }
                restored = restored.saturating_add(seen as u64);
                let current = expected_files
                    .last()
                    .map(|file| file.path.clone())
                    .unwrap_or_default();
                emit_download_progress(app, plan, received_total, restored, current);
            }
            value => {
                return Err(format!(
                    "Unsupported Courier transport object kind: {value}"
                ));
            }
        }
        fs::remove_file(&cache_path).map_err(display)?;
    }
    if !by_object.is_empty() || restored != manifest.files.len() as u64 {
        return Err("Download plan omitted one or more manifest files".into());
    }
    fs::remove_dir(&cache).map_err(display)?;
    let metadata = partial.join("courier-metadata");
    fs::create_dir_all(&metadata).map_err(display)?;
    fs::write(
        metadata.join("manifest.json"),
        serde_json::to_vec_pretty(&plan.manifest).map_err(display)?,
    )
    .map_err(display)?;
    Ok((restored, received_total))
}

#[tauri::command]
async fn download_dataset(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    transfer_id: String,
    destination_directory: String,
) -> Result<DownloadResult, String> {
    let database = database_path(&app)?;
    let credential_cache = runtime.credentials.clone();
    let session_gate = runtime.session_gate.clone();
    let (client, _) = tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let base_url = configured_registry_url(&store)?;
        tauri::async_runtime::block_on(active_registry_session(
            &store,
            &base_url,
            &database,
            &credential_cache,
            &session_gate,
        ))
    })
    .await
    .map_err(|error| format!("Session lookup failed: {error}"))??;
    let plan = client.download_plan(&transfer_id).await.map_err(display)?;
    let parent = PathBuf::from(destination_directory);
    if !parent.is_dir() {
        return Err("Choose an existing destination folder".into());
    }
    let name = safe_dataset_name(&plan.dataset.source_name, &plan.dataset.transfer_id);
    let destination = parent.join(&name);
    if destination.exists() {
        return Err(format!(
            "A file or folder named {name} already exists at the destination"
        ));
    }
    let partial = parent.join(format!(".{name}.courier-partial-{}", Uuid::new_v4()));
    fs::create_dir(&partial).map_err(display)?;
    let restored = restore_download_plan(&app, &client, &plan, &partial).await;
    let (restored_files, transport_bytes) = match restored {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_dir_all(&partial);
            return Err(error);
        }
    };
    fs::rename(&partial, &destination).map_err(|error| {
        let _ = fs::remove_dir_all(&partial);
        display(error)
    })?;
    Ok(DownloadResult {
        transfer_id: plan.dataset.transfer_id,
        destination: destination.to_string_lossy().into_owned(),
        restored_files,
        original_bytes: plan.dataset.original_bytes,
        transport_bytes,
    })
}

struct DesktopObserver {
    app: AppHandle,
    transfer_id: Uuid,
    pause: Arc<AtomicBool>,
    confirmed: Arc<AtomicU64>,
    base_confirmed: u64,
    total: u64,
    current_file: String,
    object_original_bytes: u64,
    object_transport_bytes: u64,
    object_confirmed_transport_bytes: Arc<AtomicU64>,
}

impl UploadObserver for DesktopObserver {
    fn should_pause(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }

    fn part_confirmed(&self, event: PartUploadEvent) {
        let object_confirmed = self
            .object_confirmed_transport_bytes
            .fetch_add(event.source_bytes, Ordering::Relaxed)
            .saturating_add(event.source_bytes);
        let confirmed = self
            .base_confirmed
            .saturating_add(self.scaled(object_confirmed));
        self.confirmed.store(confirmed, Ordering::Relaxed);
        let _ = self.app.emit(
            "courier://progress",
            TransferProgressEvent {
                transfer_id: self.transfer_id,
                confirmed_bytes: confirmed,
                sent_bytes: confirmed,
                total_bytes: self.total,
                current_file: self.current_file.clone(),
                status: "uploading",
            },
        );
    }

    fn reconciled(&self, source_bytes_confirmed: u64) {
        self.object_confirmed_transport_bytes
            .store(source_bytes_confirmed, Ordering::Relaxed);
        let confirmed = self
            .base_confirmed
            .saturating_add(self.scaled(source_bytes_confirmed));
        self.confirmed.store(confirmed, Ordering::Relaxed);
        let _ = self.app.emit(
            "courier://progress",
            TransferProgressEvent {
                transfer_id: self.transfer_id,
                confirmed_bytes: confirmed,
                sent_bytes: confirmed,
                total_bytes: self.total,
                current_file: self.current_file.clone(),
                status: "uploading",
            },
        );
    }
}

impl DesktopObserver {
    fn scaled(&self, transport_bytes: u64) -> u64 {
        scale_transport_progress(
            transport_bytes,
            self.object_original_bytes,
            self.object_transport_bytes,
        )
    }
}

fn scale_transport_progress(
    transport_bytes: u64,
    original_bytes: u64,
    total_transport: u64,
) -> u64 {
    if total_transport == 0 || transport_bytes >= total_transport {
        original_bytes
    } else {
        ((transport_bytes as u128 * original_bytes as u128) / total_transport as u128) as u64
    }
}

fn modified_ns(path: &Path) -> Result<i64, String> {
    let modified = path
        .metadata()
        .map_err(display)?
        .modified()
        .map_err(display)?;
    let duration = modified.duration_since(UNIX_EPOCH).map_err(display)?;
    i64::try_from(duration.as_secs() as i128 * 1_000_000_000_i128 + duration.subsec_nanos() as i128)
        .map_err(display)
}

fn remove_pack_cache(store: &TransferStore, transfer_id: Uuid) {
    let Ok(objects) = store.transport_objects(transfer_id) else {
        return;
    };
    let mut directories = Vec::new();
    for path in objects.into_iter().filter_map(|object| object.cache_path) {
        if let Some(parent) = path.parent() {
            directories.push(parent.to_path_buf());
        }
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Could not remove Courier pack {}: {error}", path.display());
        }
    }
    directories.sort();
    directories.dedup();
    for directory in directories {
        if let Err(error) = fs::remove_dir(&directory)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "Could not remove Courier pack directory {}: {error}",
                directory.display()
            );
        }
    }
}

fn remove_transfer_pack_directory(database: &Path, transfer_id: Uuid) -> Result<(), String> {
    let cache_root = database
        .parent()
        .ok_or_else(|| "Courier data directory is unavailable".to_string())?;
    let directory = cache_root.join("packs").join(transfer_id.to_string());
    match fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Could not remove Courier pack cache {}: {error}",
            directory.display()
        )),
    }
}

struct PreparedTransportPlan {
    objects: Vec<TransportObjectRecord>,
    members: Vec<TransportMemberRecord>,
    upload_sources: Vec<FileRecord>,
}

fn prepare_transport_plan(
    transfer_id: Uuid,
    files: &[FileRecord],
    cache_root: &Path,
) -> Result<PreparedTransportPlan, String> {
    let options = PackOptions::default();
    let plan = plan_packs(files, options).map_err(display)?;
    let pack_directory = cache_root.join("packs").join(transfer_id.to_string());
    if !plan.packs.is_empty() {
        fs::create_dir_all(&pack_directory).map_err(display)?;
    }
    let mut objects = Vec::new();
    let mut members = Vec::new();
    let mut upload_sources = Vec::new();

    for pack in plan.packs {
        let object_id = Uuid::new_v4();
        let destination = pack_directory.join(format!("{object_id}.iscpack.zst"));
        let temporary = pack_directory.join(format!("{object_id}.tmp"));
        let result = (|| -> Result<(), String> {
            let mut output = File::create(&temporary).map_err(display)?;
            encode_pack(&pack, &mut output, options.zstd_level).map_err(display)?;
            output.sync_all().map_err(display)?;
            fs::rename(&temporary, &destination).map_err(display)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let transport_bytes = destination.metadata().map_err(display)?.len();
        let original_bytes = pack
            .iter()
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        objects.push(TransportObjectRecord {
            id: object_id,
            transfer_id,
            kind: TransportObjectKind::Pack,
            compression: "zstd".into(),
            encoding_version: 2,
            original_bytes,
            transport_bytes: Some(transport_bytes),
            cache_path: Some(destination.clone()),
        });
        for (member_index, file) in pack.iter().enumerate() {
            members.push(TransportMemberRecord {
                object_id,
                file_id: file.id,
                member_index: member_index as u32,
            });
        }
        upload_sources.push(FileRecord {
            id: object_id,
            transfer_id,
            relative_path: PathBuf::from(format!("Courier pack {object_id}")),
            absolute_path: destination.clone(),
            size: transport_bytes,
            mtime_ns: modified_ns(&destination)?,
            hash_algorithm: HashAlgorithm::Sha256,
            sha256: String::new(),
            status: FileStatus::Ready,
            bytes_completed: 0,
        });
    }

    for file in plan.standalone {
        objects.push(TransportObjectRecord {
            id: file.id,
            transfer_id,
            kind: TransportObjectKind::File,
            compression: "none".into(),
            encoding_version: 1,
            original_bytes: file.size,
            transport_bytes: Some(file.size),
            cache_path: None,
        });
        members.push(TransportMemberRecord {
            object_id: file.id,
            file_id: file.id,
            member_index: 0,
        });
        upload_sources.push(file.clone());
    }
    Ok(PreparedTransportPlan {
        objects,
        members,
        upload_sources,
    })
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
        let mut store = TransferStore::open(&database).map_err(display)?;
        let base_url = configured_registry_url(&store)?;
        let transfer = Transfer::draft(source.clone(), project_id);
        store.create_transfer(&transfer).map_err(display)?;
        store
            .bind_transfer_registry(transfer.id, &base_url)
            .map_err(display)?;
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
                        phase: "analyzing",
                    },
                );
            },
        ) {
            Ok(files) => {
                store
                    .replace_inventory(transfer.id, &files)
                    .map_err(display)?;
                let _ = worker_app.emit(
                    "courier://inventory-progress",
                    InventoryProgressEvent {
                        transfer_id: transfer.id,
                        files_analyzed: files.len() as u64,
                        total_files: files.len() as u64,
                        bytes_analyzed: files.iter().map(|file| file.size).sum(),
                        total_bytes: files.iter().map(|file| file.size).sum(),
                        current_path: "Creating compressed, resumable transport packages".into(),
                        phase: "packaging",
                    },
                );
                let cache_root = database
                    .parent()
                    .ok_or_else(|| "Courier data directory is unavailable".to_string())?;
                let plan = prepare_transport_plan(transfer.id, &files, cache_root)?;
                store
                    .replace_transport_plan(transfer.id, &plan.objects, &plan.members)
                    .map_err(display)?;
                for source in &plan.upload_sources {
                    let parts = plan_parts(source.id, source.size, MultipartLimits::default())
                        .map_err(display)?;
                    store
                        .replace_part_plan(source.id, &parts)
                        .map_err(display)?;
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
async fn transfer_sizes(app: AppHandle, transfer_id: Uuid) -> Result<TransferSizes, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(database).map_err(display)?;
        let transfer = store
            .get_transfer(transfer_id)
            .map_err(display)?
            .ok_or_else(|| format!("Transfer not found: {transfer_id}"))?;
        let objects = store.transport_objects(transfer_id).map_err(display)?;
        let transport_bytes = objects
            .iter()
            .map(|object| object.transport_bytes)
            .collect::<Option<Vec<_>>>()
            .map(|sizes| sizes.into_iter().sum());
        Ok(TransferSizes {
            original_bytes: transfer.original_bytes,
            transport_bytes,
        })
    })
    .await
    .map_err(|error| format!("Transfer size lookup failed: {error}"))?
}

#[tauri::command]
async fn clear_transfers(app: AppHandle, status: TransferStatus) -> Result<usize, String> {
    if !matches!(
        status,
        TransferStatus::Inventorying | TransferStatus::Complete
    ) {
        return Err("Only inventorying or completed transfers can be cleared".into());
    }
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let targets = store
            .list_transfers()
            .map_err(display)?
            .into_iter()
            .filter(|transfer| transfer.status == status)
            .collect::<Vec<_>>();
        let mut removed = 0;
        for transfer in targets {
            // Pack files are Courier-owned cache. Original source paths are never removed.
            remove_pack_cache(&store, transfer.id);
            remove_transfer_pack_directory(&database, transfer.id)?;
            if store.delete_transfer(transfer.id).map_err(display)? {
                removed += 1;
            }
        }
        Ok(removed)
    })
    .await
    .map_err(|error| format!("Transfer cleanup failed: {error}"))?
}

#[tauri::command]
async fn refresh_transfer_status(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    transfer_id: Uuid,
) -> Result<Transfer, String> {
    let database = database_path(&app)?;
    let credential_cache = runtime.credentials.clone();
    let session_gate = runtime.session_gate.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let transfer = store
            .get_transfer(transfer_id)
            .map_err(display)?
            .ok_or_else(|| format!("Transfer not found: {transfer_id}"))?;
        let Some(server_transfer_id) = transfer.server_transfer_id.as_deref() else {
            return Ok(transfer);
        };
        let base_url = match store.transfer_registry(transfer_id).map_err(display)? {
            Some(value) => normalize_registry_url(&value)?,
            None => configured_registry_url(&store)?,
        };
        tauri::async_runtime::block_on(async {
            let (client, _) = active_registry_session(
                &store,
                &base_url,
                &database,
                &credential_cache,
                &session_gate,
            )
            .await?;
            let remote = client
                .transfer_status(server_transfer_id)
                .await
                .map_err(display)?;
            let target = match remote.status.as_str() {
                "verifying" => Some(TransferStatus::Verifying),
                "complete" => Some(TransferStatus::Complete),
                "failed" => Some(TransferStatus::Failed),
                _ => None,
            };
            if let Some(target) = target {
                store
                    .reconcile_registry_status(transfer_id, target)
                    .map_err(display)?;
            }
            if remote.status == "complete" {
                remove_pack_cache(&store, transfer_id);
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
    let credential_cache = runtime.credentials.clone();
    let session_gate = runtime.session_gate.clone();

    let worker_app = app.clone();
    let worker = tauri::async_runtime::spawn_blocking(move || {
        run_upload(
            worker_app,
            database,
            transfer_id,
            pause,
            credential_cache,
            session_gate,
        )
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
    credential_cache: Arc<Mutex<HashMap<String, RegistryCredentials>>>,
    session_gate: Arc<tokio::sync::Mutex<()>>,
) -> Result<Transfer, String> {
    let store = TransferStore::open(&database).map_err(display)?;
    let transfer = store
        .get_transfer(transfer_id)
        .map_err(display)?
        .ok_or_else(|| format!("Transfer not found: {transfer_id}"))?;
    if transfer.manifest_version != 3 {
        return Err(
            "This transfer uses an unsupported legacy manifest; create a new transfer".into(),
        );
    }
    match transfer.status {
        TransferStatus::Ready | TransferStatus::Paused | TransferStatus::Interrupted => store
            .transition(transfer_id, TransferStatus::Uploading)
            .map_err(display)?,
        TransferStatus::Uploading => {}
        status => return Err(format!("Cannot upload a transfer in state {status}")),
    }
    emit_upload_activity(
        &app,
        transfer_id,
        0,
        transfer.original_bytes,
        "Preparing secure Registry session",
    );

    let retry = RetryPolicy::default();
    let files = store.files_for_transfer(transfer_id).map_err(display)?;
    let transport_objects = store.transport_objects(transfer_id).map_err(display)?;
    let transport_members = store.transport_members(transfer_id).map_err(display)?;
    if !files.is_empty() && transport_objects.is_empty() {
        return Err("This transfer has no v3 transport plan; create a new transfer".into());
    }
    let files_by_id = files
        .iter()
        .map(|file| (file.id, file))
        .collect::<HashMap<_, _>>();
    let mut upload_sources = HashMap::new();
    for object in &transport_objects {
        let object_members = transport_members
            .iter()
            .filter(|member| member.object_id == object.id)
            .collect::<Vec<_>>();
        let source = match object.kind {
            TransportObjectKind::File => {
                let member = object_members
                    .first()
                    .ok_or_else(|| format!("Transport object {} has no member", object.id))?;
                (*files_by_id.get(&member.file_id).ok_or_else(|| {
                    format!("Transport object {} references a missing file", object.id)
                })?)
                .clone()
            }
            TransportObjectKind::Pack => {
                let path = object.cache_path.clone().ok_or_else(|| {
                    format!("Transport pack {} has no local cache path", object.id)
                })?;
                let size = path
                    .metadata()
                    .map_err(|error| {
                        format!(
                            "Could not open cached transport pack {}: {error}",
                            path.display()
                        )
                    })?
                    .len();
                if object.transport_bytes != Some(size) {
                    return Err(format!(
                        "Cached transport pack {} changed; create a new transfer",
                        path.display()
                    ));
                }
                FileRecord {
                    id: object.id,
                    transfer_id,
                    relative_path: PathBuf::from(format!("Courier pack {}", object.id)),
                    absolute_path: path.clone(),
                    size,
                    mtime_ns: modified_ns(&path)?,
                    hash_algorithm: HashAlgorithm::Sha256,
                    sha256: String::new(),
                    status: FileStatus::Ready,
                    bytes_completed: 0,
                }
            }
        };
        upload_sources.insert(object.id, source);
    }
    let confirmed = Arc::new(AtomicU64::new(0));
    let result: Result<(), String> = tauri::async_runtime::block_on(async {
        let base_url = match store.transfer_registry(transfer_id).map_err(display)? {
            Some(value) => normalize_registry_url(&value)?,
            None => configured_registry_url(&store)?,
        };
        let (client, _) = active_registry_session(
            &store,
            &base_url,
            &database,
            &credential_cache,
            &session_gate,
        )
        .await?;
        emit_upload_activity(
            &app,
            transfer_id,
            confirmed.load(Ordering::Relaxed),
            transfer.original_bytes,
            "Registering dataset and immutable manifest",
        );
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
                ManifestTransportPlan {
                    objects: &transport_objects,
                    members: &transport_members,
                },
            )
            .await
            .map_err(display)?;
        for local in &transport_objects {
            let registered = receipt
                .transport_objects
                .iter()
                .find(|object| object.id == local.id)
                .ok_or_else(|| format!("Registry omitted transport object {}", local.id))?;
            store
                .bind_registry_object(local.id, registered.id, &registered.object_key)
                .map_err(display)?;
        }
        let bindings = transport_objects
            .iter()
            .map(|object| {
                let (server_object_id, object_key) = store
                    .registry_object_binding(object.id)
                    .map_err(display)?
                    .ok_or_else(|| format!("Registry binding missing for {}", object.id))?;
                Ok(RegistryObjectBinding {
                    server_object_id,
                    object_key,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let remote = RegistryMultipartStore::new(client.clone(), &server_transfer_id, bindings);
        for object in &transport_objects {
            let object_members = transport_members
                .iter()
                .filter(|member| member.object_id == object.id)
                .collect::<Vec<_>>();
            if object_members.iter().all(|member| {
                files_by_id
                    .get(&member.file_id)
                    .is_some_and(|file| file.status == FileStatus::Uploaded)
            }) {
                confirmed.fetch_add(object.original_bytes, Ordering::Relaxed);
                continue;
            }
            let source = upload_sources
                .get(&object.id)
                .ok_or_else(|| format!("Upload source missing for {}", object.id))?;
            let base_confirmed = confirmed.load(Ordering::Relaxed);
            let object_confirmed_transport_bytes = Arc::new(AtomicU64::new(0));
            let observer = DesktopObserver {
                app: app.clone(),
                transfer_id,
                pause: pause.clone(),
                confirmed: confirmed.clone(),
                base_confirmed,
                total: transfer.original_bytes,
                current_file: match object.kind {
                    TransportObjectKind::File => {
                        source.relative_path.to_string_lossy().into_owned()
                    }
                    TransportObjectKind::Pack => {
                        format!("Uploading packed group ({} files)", object_members.len())
                    }
                },
                object_original_bytes: object.original_bytes,
                object_transport_bytes: source.size,
                object_confirmed_transport_bytes: object_confirmed_transport_bytes.clone(),
            };
            let progress_app = app.clone();
            let progress_file = observer.current_file.clone();
            let progress_object_original = object.original_bytes;
            let progress_object_transport = source.size;
            let progress_confirmed_transport = object_confirmed_transport_bytes.clone();
            let progress_confirmed_logical = confirmed.clone();
            let progress_total = transfer.original_bytes;
            let progress_throttle = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
            remote
                .set_part_progress_observer(Some(Arc::new(move |part_sent| {
                    let now = Instant::now();
                    let Ok(mut last_emit) = progress_throttle.lock() else {
                        return;
                    };
                    if now.duration_since(*last_emit) < Duration::from_millis(200) {
                        return;
                    }
                    *last_emit = now;
                    let transport_sent = progress_confirmed_transport
                        .load(Ordering::Relaxed)
                        .saturating_add(part_sent);
                    let object_sent = scale_transport_progress(
                        transport_sent,
                        progress_object_original,
                        progress_object_transport,
                    );
                    let _ = progress_app.emit(
                        "courier://progress",
                        TransferProgressEvent {
                            transfer_id,
                            confirmed_bytes: progress_confirmed_logical.load(Ordering::Relaxed),
                            sent_bytes: base_confirmed.saturating_add(object_sent),
                            total_bytes: progress_total,
                            current_file: progress_file.clone(),
                            status: "uploading",
                        },
                    );
                })))
                .map_err(display)?;
            emit_upload_activity(
                &app,
                transfer_id,
                confirmed.load(Ordering::Relaxed),
                transfer.original_bytes,
                &observer.current_file,
            );
            upload_missing_parts_observed(&store, &remote, source, &retry, &observer)
                .await
                .map_err(display)?;
            remote.set_part_progress_observer(None).map_err(display)?;
            if observer.should_pause() {
                return Err(UploadError::Paused.to_string());
            }
            complete_uploaded_file(&store, &remote, source, &retry)
                .await
                .map_err(display)?;
            for member in object_members {
                store.mark_file_uploaded(member.file_id).map_err(display)?;
            }
            confirmed.store(
                base_confirmed.saturating_add(object.original_bytes),
                Ordering::Relaxed,
            );
        }
        emit_upload_activity(
            &app,
            transfer_id,
            confirmed.load(Ordering::Relaxed),
            transfer.original_bytes,
            "Finalizing upload with the Registry",
        );
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

fn emit_upload_activity(
    app: &AppHandle,
    transfer_id: Uuid,
    confirmed_bytes: u64,
    total_bytes: u64,
    current_file: &str,
) {
    let _ = app.emit(
        "courier://progress",
        TransferProgressEvent {
            transfer_id,
            confirmed_bytes,
            sent_bytes: confirmed_bytes,
            total_bytes,
            current_file: current_file.to_owned(),
            status: "uploading",
        },
    );
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
            sent_bytes: confirmed_bytes,
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
            registry_endpoint,
            exchange_invitation,
            current_authorization,
            download_dataset,
            create_inventory,
            list_transfers,
            transfer_sizes,
            clear_transfers,
            refresh_transfer_status,
            start_upload,
            pause_upload
        ])
        .run(tauri::generate_context!())
        .expect("error while running Icy Seas Courier");
}

#[cfg(test)]
mod tests {
    use std::fs;

    use courier_core::{InventoryOptions, inventory_transfer};
    use courier_pack::decode_pack;

    use super::*;

    #[test]
    fn registry_urls_require_https_except_on_loopback() {
        assert_eq!(
            normalize_registry_url(" https://registry.example.test:8443/ ").unwrap(),
            "https://registry.example.test:8443"
        );
        assert_eq!(
            normalize_registry_url("http://127.0.0.1:8020").unwrap(),
            "http://127.0.0.1:8020"
        );
        assert!(normalize_registry_url("http://100.64.1.2:8010").is_err());
        assert!(normalize_registry_url("https://user@registry.example.test").is_err());
        assert!(normalize_registry_url("https://registry.example.test/prefix").is_err());
    }

    #[test]
    fn download_manifest_paths_are_strictly_relative_and_collision_safe() {
        let root = Path::new("/tmp/courier-destination");
        assert_eq!(
            safe_relative_path(root, "casts/001.csv").unwrap(),
            root.join("casts/001.csv")
        );
        for unsafe_path in ["../secret", "/absolute", "casts\\windows.csv", "./file"] {
            assert!(safe_relative_path(root, unsafe_path).is_err());
        }

        let file = |path: &str| DownloadManifestFile {
            path: path.into(),
            size: 0,
            mtime: chrono::Utc::now(),
            digest: DownloadDigest {
                algorithm: HashAlgorithm::Sha256,
                value: "0".repeat(64),
            },
            transport: DownloadTransport {
                object_id: Uuid::nil(),
                member_index: 0,
            },
        };
        assert!(
            validate_download_manifest(&DownloadManifest {
                files: vec![file("Data.csv"), file("data.csv")],
            })
            .is_err()
        );
    }

    #[test]
    fn small_files_become_a_cached_resumable_pack() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.csv"), b"a,1\n").unwrap();
        fs::write(source.join("b.csv"), b"b,2\n").unwrap();
        let transfer_id = Uuid::new_v4();
        let files = inventory_transfer(transfer_id, &source, &InventoryOptions::default()).unwrap();

        let plan = prepare_transport_plan(transfer_id, &files, directory.path()).unwrap();

        assert_eq!(plan.objects.len(), 1);
        assert_eq!(plan.objects[0].kind, TransportObjectKind::Pack);
        assert_eq!(plan.members.len(), 2);
        assert_eq!(plan.upload_sources.len(), 1);
        assert_eq!(
            plan.objects[0].transport_bytes,
            Some(
                plan.upload_sources[0]
                    .absolute_path
                    .metadata()
                    .unwrap()
                    .len()
            )
        );
        let mut paths = Vec::new();
        decode_pack(
            File::open(&plan.upload_sources[0].absolute_path).unwrap(),
            |header, reader| {
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes)?;
                paths.push(header.path);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(paths, ["a.csv", "b.csv"]);
    }
}
