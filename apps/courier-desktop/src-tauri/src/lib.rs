use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use courier_core::{
    FileRecord, FileStatus, HashAlgorithm, InventoryOptions, RegistrySessionRecord, RetryPolicy,
    Transfer, TransferStatus, TransferStore, TransportMemberRecord, TransportObjectKind,
    TransportObjectRecord, digest_file, inventory_transfer_observed,
};
use courier_pack::{PackError, PackOptions, decode_pack, encode_pack, plan_packs};
use courier_registry::{
    ManifestTransportPlan, RegistryClient, RegistryDownloadDataset, RegistryDownloadPlan,
    RegistryInvitationPurpose, RegistryMultipartStore, RegistryObjectBinding, RegistryProject,
};
use courier_transfer::{
    MultipartLimits, PartUploadEvent, UploadError, UploadObserver, complete_uploaded_file,
    plan_parts, upload_missing_parts_observed,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use url::{Host, Url};
use uuid::Uuid;

struct RuntimeState {
    controls: Mutex<HashMap<Uuid, Arc<AtomicBool>>>,
    download_controls: Mutex<HashMap<String, Arc<AtomicBool>>>,
    credentials: Arc<Mutex<HashMap<String, RegistryCredentials>>>,
    session_gate: Arc<tokio::sync::Mutex<()>>,
    device_unlocked: Arc<AtomicBool>,
    diagnostics: Mutex<VecDeque<DiagnosticEvent>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            controls: Mutex::new(HashMap::new()),
            download_controls: Mutex::new(HashMap::new()),
            credentials: Arc::new(Mutex::new(HashMap::new())),
            session_gate: Arc::new(tokio::sync::Mutex::new(())),
            device_unlocked: Arc::new(AtomicBool::new(!cfg!(target_os = "macos"))),
            diagnostics: Mutex::new(VecDeque::new()),
        }
    }
}

const MAX_DIAGNOSTIC_EVENTS: usize = 5_000;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticEvent {
    timestamp: String,
    level: String,
    message: String,
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceAccessStatus {
    has_stored_authorization: bool,
    biometric_available: bool,
    biometric_label: &'static str,
    authentication_required: bool,
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

// Packs are reproducible from the immutable inventory, so retaining every pack
// is unnecessary. Keep at most ten target-sized packs per transfer; an absent
// cache entry is rebuilt immediately before its upload.
const PACK_CACHE_PACKS_PER_TRANSFER: u64 = 10;
const STAGED_PACK_STALE_AFTER: Duration = Duration::from_secs(60 * 60);

fn pack_cache_budget(options: PackOptions) -> u64 {
    options
        .target_pack_size
        .saturating_mul(PACK_CACHE_PACKS_PER_TRANSFER)
}

fn credential_entry(base_url: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(CREDENTIAL_SERVICE, base_url)
        .map_err(|error| credential_store_error("prepare", error))
}

fn invitation_scope(invitation_code: &str) -> String {
    format!(
        "invite:{}",
        blake3::hash(invitation_code.trim().as_bytes()).to_hex()
    )
}

#[cfg(target_os = "windows")]
fn credential_store_error(operation: &str, error: impl std::fmt::Display) -> String {
    format!(
        "Windows Credential Manager denied Courier access while trying to {operation} Registry credentials: {error}. This is a local Windows access or policy issue, not a Registry authorization failure. Ensure Credential Manager is available and run Courier as the same Windows user who accepted the invitation."
    )
}

#[cfg(not(target_os = "windows"))]
fn credential_store_error(operation: &str, error: impl std::fmt::Display) -> String {
    format!(
        "Courier could not {operation} Registry credentials in the operating system secure credential store: {error}"
    )
}

fn open_transfer_store(database: &Path) -> Result<TransferStore, String> {
    TransferStore::open(database).map_err(|error| {
        format!(
            "Courier cannot access local transfer state at {}: {error}. Check that the disk has free space and that the current user account can read and write Courier's local-data folder.",
            database.display()
        )
    })
}

fn cleanup_stale_staged_packs(cache_root: &Path, now: SystemTime) -> Result<u64, String> {
    let packs = cache_root.join("packs");
    let transfer_directories = match fs::read_dir(&packs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(format!(
                "Could not inspect Courier's pack cache {}: {error}",
                packs.display()
            ));
        }
    };
    let mut removed = 0;
    for transfer_directory in transfer_directories {
        let transfer_directory = transfer_directory.map_err(display)?;
        if !transfer_directory.file_type().map_err(display)?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(transfer_directory.path()).map_err(display)? {
            let entry = entry.map_err(display)?;
            if !entry.file_type().map_err(display)?.is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("tmp")
            {
                continue;
            }
            let modified = entry
                .metadata()
                .map_err(display)?
                .modified()
                .map_err(display)?;
            let is_stale = now
                .duration_since(modified)
                .is_ok_and(|age| age >= STAGED_PACK_STALE_AFTER);
            if is_stale {
                fs::remove_file(entry.path()).map_err(|error| {
                    format!(
                        "Could not remove stale staged transport pack {}: {error}",
                        entry.path().display()
                    )
                })?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn archive_corrupt_database(database: &Path) -> Result<PathBuf, String> {
    let parent = database
        .parent()
        .ok_or_else(|| "Courier database has no parent directory".to_string())?;
    let archive = parent.join(format!(
        "courier-db-recovery-{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        Uuid::new_v4()
    ));
    fs::create_dir(&archive).map_err(|error| {
        format!(
            "Could not create a recovery archive for {}: {error}",
            database.display()
        )
    })?;

    let mut moved = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut source_name = database.as_os_str().to_os_string();
        source_name.push(suffix);
        let source = PathBuf::from(source_name);
        if !source.exists() {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        let destination = archive.join(name);
        if let Err(error) = fs::rename(&source, &destination) {
            for (original, archived) in moved.iter().rev() {
                let _ = fs::rename(archived, original);
            }
            let _ = fs::remove_dir(&archive);
            return Err(format!(
                "Could not preserve corrupt database file {} in {}: {error}",
                source.display(),
                archive.display()
            ));
        }
        moved.push((source, destination));
    }
    if moved.is_empty() {
        let _ = fs::remove_dir(&archive);
        return Err(format!(
            "Corrupt database {} disappeared before recovery could preserve it",
            database.display()
        ));
    }
    Ok(archive)
}

fn ensure_database_integrity(database: &Path) -> Result<Option<(PathBuf, String)>, String> {
    if !database.exists() {
        let has_orphaned_sidecar = ["-wal", "-shm"].iter().any(|suffix| {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            Path::new(&sidecar).exists()
        });
        if has_orphaned_sidecar {
            let archive = archive_corrupt_database(database)?;
            return Ok(Some((
                archive,
                "SQLite database is missing but journal sidecars remain".into(),
            )));
        }
        return Ok(None);
    }
    match TransferStore::integrity_check(database) {
        Ok(results) if results.len() == 1 && results[0] == "ok" => Ok(None),
        Ok(results) => {
            let details = results.join("; ");
            let archive = archive_corrupt_database(database)?;
            Ok(Some((archive, details)))
        }
        Err(error) if error.is_database_corruption() => {
            let details = error.to_string();
            let archive = archive_corrupt_database(database)?;
            Ok(Some((archive, details)))
        }
        Err(error) => Err(format!(
            "Could not check local database integrity at {}: {error}",
            database.display()
        )),
    }
}

fn initialize_local_state(app: &AppHandle) -> Result<(), String> {
    let database = database_path(app)?;
    let recovery_archive = ensure_database_integrity(&database)?;
    if let Some((archive, details)) = &recovery_archive {
        record_diagnostic(
            app,
            "warning",
            format!(
                "Local database corruption detected ({details}). The damaged database and SQLite sidecars were preserved at {}. Courier is creating a fresh local database; local history and resumable state are not automatically restored, so needed transfers must be started again.",
                archive.display(),
            ),
        );
    }
    drop(open_transfer_store(&database)?);
    let results = TransferStore::integrity_check(&database).map_err(display)?;
    if results.len() != 1 || results[0] != "ok" {
        return Err(format!(
            "Local database failed its integrity check after startup initialization: {}",
            results.join("; ")
        ));
    }
    if recovery_archive.is_some() {
        record_diagnostic(
            app,
            "info",
            "Fresh local transfer database created and verified after corruption recovery",
        );
    } else {
        record_diagnostic(
            app,
            "info",
            "Local transfer database integrity check passed",
        );
    }
    match cleanup_stale_staged_packs(
        database.parent().unwrap_or_else(|| Path::new(".")),
        SystemTime::now(),
    ) {
        Ok(removed) if removed > 0 => record_diagnostic(
            app,
            "info",
            format!("Removed {removed} stale temporary transport pack(s)"),
        ),
        Ok(_) => {}
        Err(error) => record_diagnostic(app, "warning", error),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn protected_keychain_options(base_url: &str) -> security_framework::passwords::PasswordOptions {
    let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
        CREDENTIAL_SERVICE,
        base_url,
    );
    options.use_protected_keychain();
    options
}

#[cfg(target_os = "macos")]
fn local_development_credentials(
    database: &Path,
) -> Result<HashMap<String, RegistryCredentials>, String> {
    let path = database.with_file_name("registry-credentials.development.json");
    match fs::read(path) {
        Ok(encoded) => serde_json::from_slice(&encoded).map_err(display),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(display(error)),
    }
}

#[cfg(target_os = "macos")]
fn save_local_development_credentials(
    database: &Path,
    session_id: &str,
    credentials: &RegistryCredentials,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let path = database.with_file_name("registry-credentials.development.json");
    let temporary = database.with_file_name(format!(
        ".registry-credentials.development.{}.tmp",
        Uuid::new_v4()
    ));
    let mut credentials_by_registry = local_development_credentials(database)?;
    credentials_by_registry.insert(session_id.to_owned(), credentials.clone());
    let encoded = serde_json::to_vec(&credentials_by_registry).map_err(display)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(display)?;
    let result = (|| -> Result<(), String> {
        output.write_all(&encoded).map_err(display)?;
        output.sync_all().map_err(display)?;
        drop(output);
        fs::rename(&temporary, &path).map_err(display)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(display)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(target_os = "macos")]
fn save_credentials(
    session_id: &str,
    credentials: &RegistryCredentials,
    database: &Path,
) -> Result<(), String> {
    let encoded = serde_json::to_vec(credentials).map_err(display)?;
    let result = security_framework::passwords::set_generic_password_options(
        &encoded,
        protected_keychain_options(session_id),
    );
    match result {
        Ok(()) => Ok(()),
        Err(_) => save_local_development_credentials(database, session_id, credentials),
    }
}

#[cfg(not(target_os = "macos"))]
fn save_credentials(
    session_id: &str,
    credentials: &RegistryCredentials,
    _database: &Path,
) -> Result<(), String> {
    let encoded = serde_json::to_string(credentials).map_err(display)?;
    credential_entry(session_id)?
        .set_password(&encoded)
        .map_err(|error| credential_store_error("save", error))
}

#[cfg(target_os = "macos")]
fn load_persisted_credentials(
    session_id: &str,
    base_url: &str,
    database: &Path,
) -> Result<Option<RegistryCredentials>, String> {
    use security_framework_sys::base::errSecItemNotFound;

    match security_framework::passwords::generic_password(protected_keychain_options(session_id)) {
        Ok(encoded) => serde_json::from_slice(&encoded).map(Some).map_err(display),
        Err(error) if error.code() == errSecItemNotFound || cfg!(debug_assertions) => {
            if let Some(credentials) = local_development_credentials(database)?.remove(session_id) {
                return Ok(Some(credentials));
            }
            // Courier previously used the legacy file-based macOS keychain. Read it once,
            // then migrate to the app-scoped data-protection keychain. The old entry is
            // deliberately left in place so migration cannot destroy the only credential.
            match credential_entry(session_id)
                .or_else(|_| credential_entry(base_url))?
                .get_password()
            {
                Ok(encoded) => {
                    let credentials: RegistryCredentials =
                        serde_json::from_str(&encoded).map_err(display)?;
                    save_credentials(session_id, &credentials, database)?;
                    Ok(Some(credentials))
                }
                Err(keyring::Error::NoEntry) if session_id != base_url => {
                    match credential_entry(base_url)?.get_password() {
                        Ok(encoded) => serde_json::from_str(&encoded).map(Some).map_err(display),
                        Err(keyring::Error::NoEntry) => Ok(None),
                        Err(error) => Err(credential_store_error("read", error)),
                    }
                }
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(credential_store_error("read", error)),
            }
        }
        Err(_) => {
            if let Some(credentials) = local_development_credentials(database)?.remove(session_id) {
                return Ok(Some(credentials));
            }
            match credential_entry(session_id)
                .or_else(|_| credential_entry(base_url))?
                .get_password()
            {
                Ok(encoded) => {
                    let credentials: RegistryCredentials =
                        serde_json::from_str(&encoded).map_err(display)?;
                    save_credentials(session_id, &credentials, database)?;
                    Ok(Some(credentials))
                }
                Err(keyring::Error::NoEntry) if session_id != base_url => {
                    match credential_entry(base_url)?.get_password() {
                        Ok(encoded) => serde_json::from_str(&encoded).map(Some).map_err(display),
                        Err(keyring::Error::NoEntry) => Ok(None),
                        Err(error) => Err(credential_store_error("read", error)),
                    }
                }
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(credential_store_error("read", error)),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn load_persisted_credentials(
    session_id: &str,
    base_url: &str,
    _database: &Path,
) -> Result<Option<RegistryCredentials>, String> {
    match credential_entry(session_id)
        .or_else(|_| credential_entry(base_url))?
        .get_password()
    {
        Ok(encoded) => serde_json::from_str(&encoded).map(Some).map_err(display),
        Err(keyring::Error::NoEntry) if session_id != base_url => {
            match credential_entry(base_url)?.get_password() {
                Ok(encoded) => serde_json::from_str(&encoded).map(Some).map_err(display),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(credential_store_error("read", error)),
            }
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(credential_store_error("read", error)),
    }
}

fn load_credentials(
    session_id: &str,
    base_url: &str,
    cache: &Mutex<HashMap<String, RegistryCredentials>>,
    database: &Path,
) -> Result<Option<RegistryCredentials>, String> {
    if let Some(credentials) = cache
        .lock()
        .map_err(|_| "Registry credential cache is unavailable".to_string())?
        .get(session_id)
        .cloned()
    {
        return Ok(Some(credentials));
    }
    match load_persisted_credentials(session_id, base_url, database)? {
        Some(credentials) => {
            cache
                .lock()
                .map_err(|_| "Registry credential cache is unavailable".to_string())?
                .insert(session_id.to_owned(), credentials.clone());
            Ok(Some(credentials))
        }
        None => Ok(None),
    }
}

fn session_record(
    base_url: String,
    session_id: String,
    session: &courier_registry::RegistrySession,
) -> Result<RegistrySessionRecord, String> {
    Ok(RegistrySessionRecord {
        session_id,
        base_url,
        expires_at: session.expires_at,
        refresh_expires_at: session.refresh_expires_at,
        projects_json: serde_json::to_string(&session.projects).map_err(display)?,
    })
}

fn persist_registry_session(
    store: &TransferStore,
    base_url: &str,
    session_id: &str,
    session: &courier_registry::RegistrySession,
    cache: &Mutex<HashMap<String, RegistryCredentials>>,
    database: &Path,
) -> Result<(), String> {
    let credentials = RegistryCredentials {
        access_token: session.access_token.clone(),
        refresh_token: session.refresh_token.clone(),
    };
    save_credentials(session_id, &credentials, database)?;
    cache
        .lock()
        .map_err(|_| "Registry credential cache is unavailable".to_string())?
        .insert(session_id.to_owned(), credentials);
    store
        .save_registry_session(&session_record(
            base_url.to_owned(),
            session_id.to_owned(),
            session,
        )?)
        .map_err(display)
}

async fn active_registry_session(
    store: &TransferStore,
    session_id: &str,
    base_url: &str,
    database: &std::path::Path,
    credential_cache: &Arc<Mutex<HashMap<String, RegistryCredentials>>>,
    session_gate: &Arc<tokio::sync::Mutex<()>>,
    device_unlocked: &Arc<AtomicBool>,
) -> Result<(RegistryClient, RegistrySessionRecord), String> {
    // Credential refresh tokens rotate. Keep lookup and refresh serialized so concurrent
    // status polls cannot prompt repeatedly or attempt to reuse the same refresh token.
    let _session_guard = session_gate.lock().await;
    let metadata = store
        .registry_session(session_id)
        .map_err(display)?
        .ok_or_else(|| "Enter a Registry invitation to authorize this device".to_string())?;
    if !device_unlocked.load(Ordering::Acquire) {
        return Err("Unlock saved Courier project access before continuing".into());
    }
    let credentials = load_credentials(session_id, base_url, credential_cache, database)?.ok_or_else(|| {
        "Registry credentials are unavailable in the operating system credential vault; enter a new invitation"
            .to_string()
    })?;
    if metadata.refresh_expires_at <= chrono::Utc::now() {
        return Err("Registry authorization expired; enter a new invitation".into());
    }
    if metadata.expires_at > chrono::Utc::now() + chrono::Duration::minutes(5) {
        let observer_database = database.to_path_buf();
        let observer_url = base_url.to_owned();
        let observer_session_id = session_id.to_owned();
        let observer_cache = credential_cache.clone();
        return Ok((
            RegistryClient::renewable(
                base_url,
                credentials.access_token,
                credentials.refresh_token,
                Arc::new(move |session| {
                    let store = TransferStore::open(&observer_database).map_err(display)?;
                    persist_registry_session(
                        &store,
                        &observer_url,
                        &observer_session_id,
                        session,
                        &observer_cache,
                        &observer_database,
                    )
                }),
            ),
            metadata,
        ));
    }
    let refreshed = RegistryClient::unauthenticated(base_url)
        .refresh_session(&credentials.refresh_token)
        .await
        .map_err(display)?;
    persist_registry_session(
        store,
        base_url,
        session_id,
        &refreshed,
        credential_cache,
        database,
    )?;
    let metadata = session_record(base_url.to_owned(), session_id.to_owned(), &refreshed)?;
    let observer_database = database.to_path_buf();
    let observer_url = base_url.to_owned();
    let observer_session_id = session_id.to_owned();
    let observer_cache = credential_cache.clone();
    Ok((
        RegistryClient::renewable(
            base_url,
            refreshed.access_token,
            refreshed.refresh_token,
            Arc::new(move |session| {
                let store = TransferStore::open(&observer_database).map_err(display)?;
                persist_registry_session(
                    &store,
                    &observer_url,
                    &observer_session_id,
                    session,
                    &observer_cache,
                    &observer_database,
                )
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
        &std::env::var("COURIER_REGISTRY_URL")
            .unwrap_or_else(|_| "https://courier.icyseascolab.io".into()),
    )
}

fn configured_registry_url(store: &TransferStore) -> Result<String, String> {
    match store.active_registry().map_err(display)? {
        Some(value) => normalize_registry_url(&value),
        None => default_registry_url(),
    }
}

#[cfg(target_os = "macos")]
fn biometric_available() -> bool {
    use objc2_local_authentication::{LAContext, LAPolicy};

    let context = unsafe { LAContext::new() };
    unsafe {
        context
            .canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics)
            .is_ok()
    }
}

#[cfg(not(target_os = "macos"))]
fn biometric_available() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn authenticate_device_owner() -> Result<(), String> {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    let context = unsafe { LAContext::new() };
    unsafe {
        context
            .canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication)
            .map_err(|_| "Touch ID or device authentication is unavailable".to_string())?;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let reply_sender = sender.clone();
    let reply = RcBlock::new(move |success: Bool, _error: *mut NSError| {
        if let Ok(mut sender) = reply_sender.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(success.as_bool());
        }
    });
    let reason = NSString::from_str("unlock saved Courier project access");
    unsafe {
        context.evaluatePolicy_localizedReason_reply(
            LAPolicy::DeviceOwnerAuthentication,
            &reason,
            &reply,
        );
    }
    match receiver.recv_timeout(Duration::from_secs(120)) {
        Ok(true) => Ok(()),
        Ok(false) => Err("Device authentication was not completed".into()),
        Err(_) => Err("Device authentication timed out".into()),
    }
}

#[cfg(not(target_os = "macos"))]
fn authenticate_device_owner() -> Result<(), String> {
    Err("Biometric device authentication is not available on this platform".into())
}

#[tauri::command]
async fn device_access_status(app: AppHandle) -> Result<DeviceAccessStatus, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(database).map_err(display)?;
        let has_stored_authorization = store
            .active_registry_session_id()
            .map_err(display)?
            .and_then(|session_id| store.registry_session(&session_id).ok().flatten())
            .is_some_and(|session| session.refresh_expires_at > chrono::Utc::now());
        let biometric_available = biometric_available();
        Ok(DeviceAccessStatus {
            has_stored_authorization,
            biometric_available,
            biometric_label: if cfg!(target_os = "macos") && biometric_available {
                "Touch ID"
            } else if cfg!(target_os = "macos") {
                "Mac authentication"
            } else {
                "Device authentication"
            },
            authentication_required: cfg!(target_os = "macos"),
        })
    })
    .await
    .map_err(|error| format!("Device access check failed: {error}"))?
}

#[tauri::command]
async fn authenticate_device(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<(), String> {
    let database = database_path(&app)?;
    let credential_cache = runtime.credentials.clone();
    let device_unlocked = runtime.device_unlocked.clone();
    tauri::async_runtime::spawn_blocking(move || {
        authenticate_device_owner()?;
        let store = TransferStore::open(&database).map_err(display)?;
        if let Some(session_id) = store.active_registry_session_id().map_err(display)? {
            let base_url = store
                .registry_session(&session_id)
                .map_err(display)?
                .map(|session| session.base_url)
                .unwrap_or_default();
            load_credentials(&session_id, &base_url, &credential_cache, &database)?.ok_or_else(
                || "Saved Registry credentials are unavailable; enter a new invitation".to_string(),
            )?;
        }
        device_unlocked.store(true, Ordering::Release);
        Ok(())
    })
    .await
    .map_err(|error| format!("Device authentication failed: {error}"))?
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
    let session_id = invitation_scope(&invitation_code);
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
    let device_unlocked = runtime.device_unlocked.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let store = TransferStore::open(&database).map_err(display)?;
        persist_registry_session(
            &store,
            &base_url,
            &session_id,
            &remote,
            &credential_cache,
            &database,
        )?;
        store
            .set_active_registry_session(&session_id, &base_url)
            .map_err(display)?;
        device_unlocked.store(true, Ordering::Release);
        Ok(())
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
    let device_unlocked = runtime.device_unlocked.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let base_url = configured_registry_url(&store)?;
        let session_id = store
            .active_registry_session_id()
            .map_err(display)?
            .ok_or_else(|| "Enter a Registry invitation to authorize this device".to_string())?;
        tauri::async_runtime::block_on(async {
            match active_registry_session(
                &store,
                &session_id,
                &base_url,
                &database,
                &credential_cache,
                &session_gate,
                &device_unlocked,
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
    pause: Arc<AtomicBool>,
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
    for (object_index, object) in plan.objects.iter().enumerate() {
        if pause.load(Ordering::Acquire) {
            return Err("download paused".into());
        }
        let expected_files = by_object
            .remove(&object.object_id)
            .ok_or_else(|| format!("Manifest does not reference object {}", object.object_id))?;
        record_diagnostic(
            app,
            "info",
            format!(
                "Retrieving object {} of {} for Registry transfer {} ({} files, {} transport bytes)",
                object_index + 1,
                plan.objects.len(),
                plan.dataset.transfer_id,
                expected_files.len(),
                object.transport_bytes.unwrap_or(0)
            ),
        );
        if expected_files
            .iter()
            .all(|file| restored_file_matches(partial, file))
        {
            restored = restored.saturating_add(expected_files.len() as u64);
            received_total = received_total.saturating_add(object.transport_bytes.unwrap_or(0));
            emit_download_progress(
                app,
                plan,
                received_total,
                restored,
                expected_files
                    .last()
                    .map(|file| file.path.clone())
                    .unwrap_or_default(),
            );
            continue;
        }
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
            .download_object_resumable(
                &url,
                &cache_path,
                object.transport_bytes,
                pause.clone(),
                move |object_received| {
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
                },
            )
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
        match object.kind.as_str() {
            "file" => {
                if expected_files.len() != 1 || expected_files[0].transport.member_index != 0 {
                    return Err(format!(
                        "Standalone object {} has invalid membership",
                        object.object_id
                    ));
                }
                let file = expected_files[0];
                record_diagnostic(
                    app,
                    "info",
                    format!("Restoring and verifying downloaded file: {}", file.path),
                );
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
                        record_diagnostic(
                            app,
                            "info",
                            format!("Restoring and verifying downloaded file: {}", file.path),
                        );
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

fn restored_file_matches(root: &Path, file: &DownloadManifestFile) -> bool {
    let Ok(path) = safe_relative_path(root, &file.path) else {
        return false;
    };
    path.is_file() && verify_restored_file(&path, file).is_ok()
}

#[tauri::command]
async fn download_dataset(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    transfer_id: String,
    destination_directory: String,
) -> Result<DownloadResult, String> {
    let pause = Arc::new(AtomicBool::new(false));
    {
        let mut controls = runtime
            .download_controls
            .lock()
            .map_err(|_| "Download controls are unavailable".to_string())?;
        if controls.contains_key(&transfer_id) {
            return Err("This dataset is already being retrieved".into());
        }
        controls.insert(transfer_id.clone(), pause.clone());
    }
    let result = download_dataset_inner(
        app.clone(),
        runtime.credentials.clone(),
        runtime.session_gate.clone(),
        runtime.device_unlocked.clone(),
        transfer_id.clone(),
        destination_directory,
        pause,
    )
    .await;
    if let Ok(mut controls) = runtime.download_controls.lock() {
        controls.remove(&transfer_id);
    }
    if let Err(error) = &result {
        record_diagnostic(
            &app,
            if error.eq_ignore_ascii_case("download paused") {
                "warning"
            } else {
                "error"
            },
            format!(
                "Download {} for Registry transfer {transfer_id}: {error}",
                if error.eq_ignore_ascii_case("download paused") {
                    "paused"
                } else {
                    "stopped"
                }
            ),
        );
    }
    result
}

async fn download_dataset_inner(
    app: AppHandle,
    credential_cache: Arc<Mutex<HashMap<String, RegistryCredentials>>>,
    session_gate: Arc<tokio::sync::Mutex<()>>,
    device_unlocked: Arc<AtomicBool>,
    transfer_id: String,
    destination_directory: String,
    pause: Arc<AtomicBool>,
) -> Result<DownloadResult, String> {
    record_diagnostic(
        &app,
        "info",
        format!("Download started for Registry transfer {transfer_id}"),
    );
    let database = database_path(&app)?;
    let (client, _) = tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let base_url = configured_registry_url(&store)?;
        let session_id = store
            .active_registry_session_id()
            .map_err(display)?
            .ok_or_else(|| "Enter a Registry invitation to authorize this device".to_string())?;
        tauri::async_runtime::block_on(active_registry_session(
            &store,
            &session_id,
            &base_url,
            &database,
            &credential_cache,
            &session_gate,
            &device_unlocked,
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
    let partial = parent.join(format!(".{name}.courier-partial"));
    match fs::symlink_metadata(&partial) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err("The existing Courier recovery path is not a directory".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&partial).map_err(display)?;
        }
        Err(error) => return Err(display(error)),
    }
    let restored = restore_download_plan(&app, &client, &plan, &partial, pause).await;
    let (restored_files, transport_bytes) = match restored {
        Ok(value) => value,
        Err(error) => {
            return Err(error);
        }
    };
    fs::rename(&partial, &destination).map_err(display)?;
    let result = DownloadResult {
        transfer_id: plan.dataset.transfer_id,
        destination: destination.to_string_lossy().into_owned(),
        restored_files,
        original_bytes: plan.dataset.original_bytes,
        transport_bytes,
    };
    record_diagnostic(
        &app,
        "info",
        format!(
            "Download finished for Registry transfer {}",
            result.transfer_id
        ),
    );
    Ok(result)
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
        self.pause.load(Ordering::Acquire)
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

fn remove_cached_packs_except(objects: &[TransportObjectRecord], keep: Uuid) {
    for object in objects {
        if object.id == keep || object.kind != TransportObjectKind::Pack {
            continue;
        }
        if let Some(path) = &object.cache_path
            && let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("Could not evict Courier pack {}: {error}", path.display());
        }
    }
}

fn pack_upload_source(
    object: &TransportObjectRecord,
    object_members: &[&TransportMemberRecord],
    files_by_id: &HashMap<Uuid, &FileRecord>,
    options: PackOptions,
    cache_budget: u64,
) -> Result<FileRecord, String> {
    let path = object
        .cache_path
        .as_ref()
        .ok_or_else(|| format!("Transport pack {} has no local cache path", object.id))?;
    if !path.exists() {
        let parent = path
            .parent()
            .ok_or_else(|| format!("Transport pack {} has no cache directory", object.id))?;
        fs::create_dir_all(parent).map_err(display)?;
        let temporary = parent.join(format!("{}.tmp", object.id));
        let members = object_members
            .iter()
            .map(|member| {
                files_by_id.get(&member.file_id).copied().ok_or_else(|| {
                    format!("Transport object {} references a missing file", object.id)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (transport_bytes, cached) = stage_or_measure_pack(
            &members,
            &temporary,
            path,
            options,
            cache_budget,
            &mut || {},
        )?;
        if !cached || object.transport_bytes != Some(transport_bytes) {
            let _ = fs::remove_file(path);
            return Err(format!(
                "Transport pack {} could not be reproduced; create a new transfer",
                object.id
            ));
        }
    }
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
    Ok(FileRecord {
        id: object.id,
        transfer_id: object.transfer_id,
        relative_path: PathBuf::from(format!("Courier pack {}", object.id)),
        absolute_path: path.clone(),
        size,
        mtime_ns: modified_ns(path)?,
        hash_algorithm: HashAlgorithm::Sha256,
        sha256: String::new(),
        status: FileStatus::Ready,
        bytes_completed: 0,
    })
}

struct PreparedTransportPlan {
    objects: Vec<TransportObjectRecord>,
    members: Vec<TransportMemberRecord>,
    upload_sources: Vec<FileRecord>,
}

#[derive(Clone, Copy)]
struct CacheBoundary {
    capacity: u64,
    remaining: u64,
    already_full: bool,
}

struct CappedWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
}

impl<W: Write> Write for CappedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if self.written.saturating_add(length) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "Courier pack cache budget exceeded",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct CountingWriter<W> {
    inner: W,
    written: Arc<AtomicU64>,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.written.fetch_add(written as u64, Ordering::Relaxed);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn cache_budget_exceeded(error: &PackError) -> bool {
    matches!(error, PackError::Io(error) if error.kind() == io::ErrorKind::StorageFull)
}

fn measure_pack_transport_bytes(
    members: &[&FileRecord],
    options: PackOptions,
) -> Result<u64, String> {
    let written = Arc::new(AtomicU64::new(0));
    let output = CountingWriter {
        inner: io::sink(),
        written: written.clone(),
    };
    encode_pack(members, output, options.zstd_level).map_err(display)?;
    Ok(written.load(Ordering::Relaxed))
}

/// Attempts to persist a pack without exceeding `cache_limit`. If there is not
/// enough budget, calculate its deterministic transport size without staging it.
fn sync_staged_pack(path: &Path) -> Result<(), String> {
    // `sync_all` on a read-only handle returns ERROR_ACCESS_DENIED on Windows.
    // Open the completed temporary pack with write access before flushing it,
    // then drop that handle before the atomic rename below.
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            format!(
                "Could not finalize staged transport pack {}: {error}",
                path.display()
            )
        })
}

fn stage_or_measure_pack(
    members: &[&FileRecord],
    temporary: &Path,
    destination: &Path,
    options: PackOptions,
    cache_limit: u64,
    on_cache_limit: &mut dyn FnMut(),
) -> Result<(u64, bool), String> {
    if cache_limit == 0 {
        on_cache_limit();
        return Ok((measure_pack_transport_bytes(members, options)?, false));
    }
    let output = File::create(temporary).map_err(display)?;
    let result = encode_pack(
        members,
        CappedWriter {
            inner: output,
            written: 0,
            limit: cache_limit,
        },
        options.zstd_level,
    );
    match result {
        Ok(_) => {
            let finalized = (|| -> Result<u64, String> {
                sync_staged_pack(temporary)?;
                let transport_bytes = temporary.metadata().map_err(display)?.len();
                fs::rename(temporary, destination).map_err(|error| {
                    format!(
                        "Could not publish staged transport pack {}: {error}",
                        temporary.display()
                    )
                })?;
                Ok(transport_bytes)
            })();
            match finalized {
                Ok(transport_bytes) => Ok((transport_bytes, true)),
                Err(error) => {
                    let _ = fs::remove_file(temporary);
                    Err(error)
                }
            }
        }
        Err(error) if cache_budget_exceeded(&error) => {
            let _ = fs::remove_file(temporary);
            on_cache_limit();
            Ok((measure_pack_transport_bytes(members, options)?, false))
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(display(error))
        }
    }
}

#[cfg(test)]
fn prepare_transport_plan(
    transfer_id: Uuid,
    files: &[FileRecord],
    cache_root: &Path,
) -> Result<PreparedTransportPlan, String> {
    prepare_transport_plan_with_options(
        transfer_id,
        files,
        cache_root,
        PackOptions::default(),
        pack_cache_budget(PackOptions::default()),
    )
}

fn cache_limit_source_description(members: &[&FileRecord]) -> String {
    match members {
        [] => "an unknown source file".into(),
        [file] => file.relative_path.to_string_lossy().into_owned(),
        files => format!(
            "{} small files ({} through {})",
            files.len(),
            files[0].relative_path.display(),
            files[files.len() - 1].relative_path.display()
        ),
    }
}

#[cfg(test)]
fn prepare_transport_plan_with_options(
    transfer_id: Uuid,
    files: &[FileRecord],
    cache_root: &Path,
    options: PackOptions,
    cache_budget: u64,
) -> Result<PreparedTransportPlan, String> {
    prepare_transport_plan_with_observer(
        transfer_id,
        files,
        cache_root,
        options,
        cache_budget,
        &mut |_, _| {},
    )
}

fn prepare_transport_plan_with_observer(
    transfer_id: Uuid,
    files: &[FileRecord],
    cache_root: &Path,
    options: PackOptions,
    cache_budget: u64,
    on_cache_limit: &mut dyn FnMut(&[&FileRecord], CacheBoundary),
) -> Result<PreparedTransportPlan, String> {
    let plan = plan_packs(files, options).map_err(display)?;
    let pack_directory = cache_root.join("packs").join(transfer_id.to_string());
    if !plan.packs.is_empty() || !plan.standalone.is_empty() {
        fs::create_dir_all(&pack_directory).map_err(display)?;
    }
    let mut objects = Vec::new();
    let mut members = Vec::new();
    let mut upload_sources = Vec::new();
    let mut cached_bytes = 0_u64;

    for pack in plan.packs {
        let object_id = Uuid::new_v4();
        let destination = pack_directory.join(format!("{object_id}.iscpack.zst"));
        let temporary = pack_directory.join(format!("{object_id}.tmp"));
        let remaining_cache = cache_budget.saturating_sub(cached_bytes);
        let mut cache_limit_callback = || {
            on_cache_limit(
                &pack,
                CacheBoundary {
                    capacity: cache_budget,
                    remaining: remaining_cache,
                    already_full: remaining_cache == 0,
                },
            )
        };
        let (transport_bytes, cached) = stage_or_measure_pack(
            &pack,
            &temporary,
            &destination,
            options,
            remaining_cache,
            &mut cache_limit_callback,
        )?;
        if cached {
            cached_bytes = cached_bytes.saturating_add(transport_bytes);
        }
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
            mtime_ns: if cached {
                modified_ns(&destination)?
            } else {
                0
            },
            hash_algorithm: HashAlgorithm::Sha256,
            sha256: String::new(),
            status: FileStatus::Ready,
            bytes_completed: 0,
        });
    }

    for file in plan.standalone {
        // Large files are encoded as one-member packs only when the compressed
        // object is both smaller and can fit within the bounded cache. Otherwise
        // they upload directly from their original source.
        let object_id = Uuid::new_v4();
        let destination = pack_directory.join(format!("{object_id}.iscpack.zst"));
        let temporary = pack_directory.join(format!("{object_id}.tmp"));
        let remaining_cache = cache_budget.saturating_sub(cached_bytes);
        let mut cache_limit_callback = || {
            on_cache_limit(
                &[file],
                CacheBoundary {
                    capacity: cache_budget,
                    remaining: remaining_cache,
                    already_full: remaining_cache == 0,
                },
            );
        };
        let (compressed_size, cached) = stage_or_measure_pack(
            &[file],
            &temporary,
            &destination,
            options,
            remaining_cache,
            &mut cache_limit_callback,
        )?;
        let use_compressed = compressed_size < file.size && compressed_size <= cache_budget;
        if use_compressed {
            if cached {
                cached_bytes = cached_bytes.saturating_add(compressed_size);
            }
            objects.push(TransportObjectRecord {
                id: object_id,
                transfer_id,
                kind: TransportObjectKind::Pack,
                compression: "zstd".into(),
                encoding_version: 2,
                original_bytes: file.size,
                transport_bytes: Some(compressed_size),
                cache_path: Some(destination.clone()),
            });
            members.push(TransportMemberRecord {
                object_id,
                file_id: file.id,
                member_index: 0,
            });
            upload_sources.push(FileRecord {
                id: object_id,
                transfer_id,
                relative_path: PathBuf::from(format!("Courier pack {object_id}")),
                absolute_path: destination.clone(),
                size: compressed_size,
                mtime_ns: if cached {
                    modified_ns(&destination)?
                } else {
                    0
                },
                hash_algorithm: HashAlgorithm::Sha256,
                sha256: String::new(),
                status: FileStatus::Ready,
                bytes_completed: 0,
            });
        } else {
            if cached {
                let _ = fs::remove_file(&destination);
                cached_bytes = cached_bytes.saturating_sub(compressed_size);
            }
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
    let result = tauri::async_runtime::spawn_blocking(move || {
        let source = PathBuf::from(&source_path)
            .canonicalize()
            .map_err(|error| format!("Could not open source: {error}"))?;
        let mut store = open_transfer_store(&database)?;
        let base_url = configured_registry_url(&store)?;
        let session_id = store
            .active_registry_session_id()
            .map_err(display)?
            .ok_or_else(|| "Enter a Registry invitation before preparing a transfer".to_string())?;
        let transfer = Transfer::draft(source.clone(), project_id);
        store.create_transfer(&transfer).map_err(display)?;
        store
            .bind_transfer_registry_session(transfer.id, &base_url, &session_id)
            .map_err(display)?;
        store
            .transition(transfer.id, TransferStatus::Inventorying)
            .map_err(display)?;
        record_diagnostic(
            &worker_app,
            "info",
            format!(
                "Inventory started for local transfer {}: {}",
                transfer.id,
                source.display()
            ),
        );
        let _ = worker_app.emit(
            "courier://inventory-progress",
            InventoryProgressEvent {
                transfer_id: transfer.id,
                files_analyzed: 0,
                total_files: 0,
                bytes_analyzed: 0,
                total_bytes: 0,
                current_path: "Finding files in the selected source…".into(),
                phase: "discovering",
            },
        );
        let mut discovery_logged = false;
        let mut last_inventory_path = PathBuf::new();
        let indexed = inventory_transfer_observed(
            transfer.id,
            &source,
            &InventoryOptions {
                hash_algorithm,
                ..InventoryOptions::default()
            },
            |progress| {
                if !discovery_logged {
                    record_diagnostic(
                        &worker_app,
                        "info",
                        format!(
                            "Source discovery found {} files totaling {} bytes for transfer {}",
                            progress.total_files, progress.total_bytes, transfer.id
                        ),
                    );
                    discovery_logged = true;
                }
                if !progress.current_path.as_os_str().is_empty()
                    && progress.current_path != last_inventory_path
                {
                    let file_number = progress
                        .files_analyzed
                        .saturating_add(1)
                        .min(progress.total_files);
                    record_diagnostic(
                        &worker_app,
                        "info",
                        format!(
                            "Inventorying file {file_number} of {} for transfer {}: {}",
                            progress.total_files,
                            transfer.id,
                            progress.current_path.display()
                        ),
                    );
                    last_inventory_path = progress.current_path.clone();
                }
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
        );
        let result = match indexed {
            Ok(files) => (|| -> Result<Transfer, String> {
                store
                    .replace_inventory(transfer.id, &files)
                    .map_err(display)?;
                record_diagnostic(
                    &worker_app,
                    "info",
                    format!(
                        "Indexed {} source files ({} bytes) for transfer {}",
                        files.len(),
                        files.iter().map(|file| file.size).sum::<u64>(),
                        transfer.id
                    ),
                );
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
                record_diagnostic(
                    &worker_app,
                    "info",
                    format!("Preparing resumable transport packages for transfer {}", transfer.id),
                );
                let cache_root = database
                    .parent()
                    .ok_or_else(|| "Courier data directory is unavailable".to_string())?;
                let total_bytes = files.iter().map(|file| file.size).sum::<u64>();
                let cache_capacity = pack_cache_budget(PackOptions::default());
                let mut report_cache_limit = |members: &[&FileRecord], boundary: CacheBoundary| {
                    let source_description = cache_limit_source_description(members);
                    let capacity_mib = boundary.capacity as f64 / (1024.0 * 1024.0);
                    let remaining_mib = boundary.remaining as f64 / (1024.0 * 1024.0);
                    let detail = if boundary.already_full {
                        format!(
                            "Transport staging cache has no remaining space ({capacity_mib:.0} MiB per-transfer limit). Measuring compression without caching: {source_description}"
                        )
                    } else {
                        format!(
                            "A staging attempt reached its current allowance ({remaining_mib:.0} MiB available; {capacity_mib:.0} MiB per-transfer limit) while processing {source_description}. The temporary pack was discarded; Courier is measuring compressed size without caching, which reads the source again."
                        )
                    };
                    record_diagnostic(&worker_app, "info", detail.clone());
                    let _ = worker_app.emit(
                        "courier://inventory-progress",
                        InventoryProgressEvent {
                            transfer_id: transfer.id,
                            files_analyzed: files.len() as u64,
                            total_files: files.len() as u64,
                            bytes_analyzed: total_bytes,
                            total_bytes,
                            current_path: detail,
                            phase: "packaging",
                        },
                    );
                };
                let plan = prepare_transport_plan_with_observer(
                    transfer.id,
                    &files,
                    cache_root,
                    PackOptions::default(),
                    cache_capacity,
                    &mut report_cache_limit,
                )?;
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
                let ready = store
                    .get_transfer(transfer.id)
                    .map_err(display)?
                    .ok_or_else(|| "Inventory disappeared from local state".to_string())?;
                let transport_bytes = plan
                    .objects
                    .iter()
                    .filter_map(|object| object.transport_bytes)
                    .sum::<u64>();
                record_diagnostic(
                    &worker_app,
                    "info",
                    format!(
                        "Inventory ready for transfer {}: {} files, {} original bytes, {} transport bytes",
                        transfer.id, ready.file_count, ready.original_bytes, transport_bytes
                    ),
                );
                let _ = worker_app.emit(
                    "courier://inventory-progress",
                    InventoryProgressEvent {
                        transfer_id: transfer.id,
                        files_analyzed: ready.file_count,
                        total_files: ready.file_count,
                        bytes_analyzed: ready.original_bytes,
                        total_bytes: ready.original_bytes,
                        current_path: "Inventory and transport preparation complete".into(),
                        phase: "complete",
                    },
                );
                Ok(ready)
            })(),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = &result {
            if store
                .get_transfer(transfer.id)
                .map_err(display)?
                .is_some_and(|current| current.status == TransferStatus::Inventorying)
            {
                store
                    .transition(transfer.id, TransferStatus::Failed)
                    .map_err(display)?;
            }
            record_diagnostic(
                &worker_app,
                "error",
                format!("Inventory failed for local transfer {}: {error}", transfer.id),
            );
            let _ = worker_app.emit(
                "courier://inventory-progress",
                InventoryProgressEvent {
                    transfer_id: transfer.id,
                    files_analyzed: 0,
                    total_files: 0,
                    bytes_analyzed: 0,
                    total_bytes: 0,
                    current_path: error.clone(),
                    phase: "failed",
                },
            );
        }
        result
    })
    .await
    .map_err(|error| format!("Inventory task failed: {error}"))?;
    if let Err(error) = &result {
        record_diagnostic(&app, "error", format!("Inventory command failed: {error}"));
    }
    result
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
async fn clear_incomplete_transfers(app: AppHandle) -> Result<usize, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let targets = store
            .list_transfers()
            .map_err(display)?
            .into_iter()
            .filter(|transfer| transfer.status != TransferStatus::Complete)
            .collect::<Vec<_>>();
        let mut removed = 0;
        for transfer in targets {
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
async fn clear_transfer(app: AppHandle, transfer_id: Uuid) -> Result<bool, String> {
    let database = database_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let store = TransferStore::open(&database).map_err(display)?;
        let transfer = store
            .get_transfer(transfer_id)
            .map_err(display)?
            .ok_or_else(|| format!("Transfer not found: {transfer_id}"))?;
        if transfer.status == TransferStatus::Complete {
            return Err("Completed transfers must be cleared from transfer history instead".into());
        }
        remove_pack_cache(&store, transfer.id);
        remove_transfer_pack_directory(&database, transfer.id)?;
        store.delete_transfer(transfer.id).map_err(display)
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
    let device_unlocked = runtime.device_unlocked.clone();
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
        let session_id = store
            .transfer_registry_session(transfer_id)
            .map_err(display)?
            .ok_or_else(|| "Transfer is not bound to a Registry invitation".to_string())?;
        tauri::async_runtime::block_on(async {
            let (client, _) = active_registry_session(
                &store,
                &session_id,
                &base_url,
                &database,
                &credential_cache,
                &session_gate,
                &device_unlocked,
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
    let device_unlocked = runtime.device_unlocked.clone();

    let worker_app = app.clone();
    let worker = tauri::async_runtime::spawn_blocking(move || {
        run_upload(
            worker_app,
            database,
            transfer_id,
            pause,
            credential_cache,
            session_gate,
            device_unlocked,
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
    device_unlocked: Arc<AtomicBool>,
) -> Result<Transfer, String> {
    record_diagnostic(
        &app,
        "info",
        format!("Upload started for local transfer {transfer_id}"),
    );
    let store = open_transfer_store(&database)?;
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
    let confirmed = Arc::new(AtomicU64::new(0));
    let result: Result<(), String> = tauri::async_runtime::block_on(async {
        let base_url = match store.transfer_registry(transfer_id).map_err(display)? {
            Some(value) => normalize_registry_url(&value)?,
            None => configured_registry_url(&store)?,
        };
        let session_id = store
            .transfer_registry_session(transfer_id)
            .map_err(display)?
            .ok_or_else(|| "Transfer is not bound to a Registry invitation".to_string())?;
        let (client, _) = active_registry_session(
            &store,
            &session_id,
            &base_url,
            &database,
            &credential_cache,
            &session_gate,
            &device_unlocked,
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
        remote
            .set_pause_flag(Some(pause.clone()))
            .map_err(display)?;
        for (object_index, object) in transport_objects.iter().enumerate() {
            let object_members = transport_members
                .iter()
                .filter(|member| member.object_id == object.id)
                .collect::<Vec<_>>();
            if object_members.iter().all(|member| {
                files_by_id
                    .get(&member.file_id)
                    .is_some_and(|file| file.status == FileStatus::Uploaded)
            }) {
                record_diagnostic(
                    &app,
                    "info",
                    format!(
                        "Skipping already completed upload object {} of {} ({}) for transfer {}",
                        object_index + 1,
                        transport_objects.len(),
                        object.id,
                        transfer_id
                    ),
                );
                confirmed.fetch_add(object.original_bytes, Ordering::Relaxed);
                continue;
            }
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
                    // Retain only the object being uploaded. This makes room for a
                    // regenerated cache while preserving at most one bounded pack.
                    remove_cached_packs_except(&transport_objects, object.id);
                    pack_upload_source(
                        object,
                        &object_members,
                        &files_by_id,
                        PackOptions::default(),
                        pack_cache_budget(PackOptions::default()),
                    )?
                }
            };
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
            record_diagnostic(
                &app,
                "info",
                format!(
                    "Uploading object {} of {} for transfer {}: {} ({} files, {} original bytes, {} transport bytes)",
                    object_index + 1,
                    transport_objects.len(),
                    transfer_id,
                    observer.current_file,
                    object_members.len(),
                    object.original_bytes,
                    source.size
                ),
            );
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
            upload_missing_parts_observed(&store, &remote, &source, &retry, &observer)
                .await
                .map_err(display)?;
            remote.set_part_progress_observer(None).map_err(display)?;
            if observer.should_pause() {
                return Err(UploadError::Paused.to_string());
            }
            complete_uploaded_file(&store, &remote, &source, &retry)
                .await
                .map_err(display)?;
            for member in object_members {
                store.mark_file_uploaded(member.file_id).map_err(display)?;
            }
            if object.kind == TransportObjectKind::Pack {
                if let Some(path) = &object.cache_path {
                    fs::remove_file(path).map_err(|error| {
                        format!(
                            "Could not remove uploaded Courier pack {}: {error}",
                            path.display()
                        )
                    })?;
                }
            }
            confirmed.store(
                base_confirmed.saturating_add(object.original_bytes),
                Ordering::Relaxed,
            );
            record_diagnostic(
                &app,
                "info",
                format!(
                    "Upload object {} of {} confirmed for transfer {} ({} original bytes)",
                    object_index + 1,
                    transport_objects.len(),
                    transfer_id,
                    object.original_bytes
                ),
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
            record_diagnostic(
                &app,
                "info",
                format!(
                    "Upload finished for local transfer {transfer_id}; Registry verification is pending"
                ),
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
            record_diagnostic(
                &app,
                "info",
                format!("Upload paused for local transfer {transfer_id}"),
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
            record_diagnostic(
                &app,
                "error",
                format!("Upload interrupted for local transfer {transfer_id}: {error}"),
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
    pause.store(true, Ordering::Release);
    Ok(())
}

#[tauri::command]
fn pause_download(runtime: State<'_, RuntimeState>, transfer_id: String) -> Result<(), String> {
    let controls = runtime
        .download_controls
        .lock()
        .map_err(|_| "Download controls are unavailable".to_string())?;
    let pause = controls
        .get(&transfer_id)
        .ok_or_else(|| "Dataset is not currently being retrieved".to_string())?;
    pause.store(true, Ordering::Release);
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
    fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "Courier cannot create or access its local-data folder {}: {error}. Check that the disk has free space and that the current user account can write to the folder.",
            directory.display()
        )
    })?;
    Ok(directory.join("courier.db"))
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn record_diagnostic(app: &AppHandle, level: &str, message: impl Into<String>) {
    let event = DiagnosticEvent {
        timestamp: chrono::Utc::now().to_rfc3339(),
        level: level.to_owned(),
        message: message.into(),
    };
    if let Ok(mut diagnostics) = app.state::<RuntimeState>().diagnostics.lock() {
        if diagnostics.len() == MAX_DIAGNOSTIC_EVENTS {
            diagnostics.pop_front();
        }
        diagnostics.push_back(event.clone());
    }
    let _ = app.emit("courier://diagnostic", event);
}

fn diagnostic_report(events: &[DiagnosticEvent]) -> String {
    let mut report = format!(
        "Icy Seas Courier diagnostics\nGenerated: {}\nCourier version: {}\nPlatform: {} {}\n\n",
        chrono::Utc::now().to_rfc3339(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    report.push_str("Events (newest last)\n");
    if events.is_empty() {
        report.push_str("No diagnostic events have been recorded in this Courier session.\n");
    } else {
        for event in events {
            report.push_str(&format!(
                "{} [{}] {}\n",
                event.timestamp,
                event.level.to_uppercase(),
                event.message
            ));
        }
    }
    report.push_str(
        "\nPrivacy note: Courier diagnostics do not include invitation codes, Registry session tokens, or presigned URL query strings. Diagnostics may include local source/file paths and the path to a preserved recovery database. The in-memory session log retains the most recent 5,000 events.\n",
    );
    report
}

#[tauri::command]
fn diagnostic_events(runtime: State<'_, RuntimeState>) -> Result<Vec<DiagnosticEvent>, String> {
    runtime
        .diagnostics
        .lock()
        .map(|events| events.iter().cloned().collect())
        .map_err(|_| "Courier diagnostics are unavailable".to_string())
}

#[tauri::command]
fn open_diagnostics_window(app: AppHandle) -> Result<(), String> {
    let window = if let Some(window) = app.get_webview_window("diagnostics") {
        window
    } else {
        WebviewWindowBuilder::new(
            &app,
            "diagnostics",
            WebviewUrl::App("index.html?window=diagnostics".into()),
        )
        .title("Courier Diagnostics")
        .inner_size(760.0, 520.0)
        .min_inner_size(560.0, 340.0)
        .resizable(true)
        .build()
        .map_err(display)?
    };
    window.show().map_err(display)?;
    window.set_focus().map_err(display)?;
    Ok(())
}

#[tauri::command]
fn diagnostic_report_text(runtime: State<'_, RuntimeState>) -> Result<String, String> {
    let events = runtime
        .diagnostics
        .lock()
        .map_err(|_| "Courier diagnostics are unavailable".to_string())?
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    Ok(diagnostic_report(&events))
}

#[tauri::command]
fn save_diagnostic_report(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    path: PathBuf,
) -> Result<(), String> {
    let report = diagnostic_report_text(runtime)?;
    fs::write(&path, report)
        .map_err(|error| format!("Could not save diagnostics to {}: {error}", path.display()))?;
    record_diagnostic(
        &app,
        "info",
        format!("Saved diagnostic report to {}", path.display()),
    );
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(RuntimeState::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            record_diagnostic(
                app.handle(),
                "info",
                "Courier started; diagnostics are ready for this session",
            );
            if let Err(error) = initialize_local_state(app.handle()) {
                record_diagnostic(
                    app.handle(),
                    "error",
                    format!("Local state startup check failed: {error}"),
                );
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            device_access_status,
            authenticate_device,
            registry_endpoint,
            exchange_invitation,
            current_authorization,
            download_dataset,
            create_inventory,
            list_transfers,
            transfer_sizes,
            clear_transfers,
            clear_incomplete_transfers,
            clear_transfer,
            refresh_transfer_status,
            start_upload,
            pause_upload,
            pause_download,
            diagnostic_events,
            diagnostic_report_text,
            save_diagnostic_report,
            open_diagnostics_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running Icy Seas Courier");
}

#[cfg(test)]
mod tests {
    use std::fs;

    use courier_core::{InventoryOptions, inventory_transfer};
    use courier_pack::decode_pack;
    use filetime::{FileTime, set_file_mtime};

    use super::*;

    #[test]
    fn credential_store_errors_explain_the_local_remediation_boundary() {
        let message = credential_store_error("read", "Access is denied (os error 5)");
        assert!(message.contains("secure credential"));
        #[cfg(target_os = "windows")]
        {
            assert!(message.contains("Windows Credential Manager"));
            assert!(message.contains("not a Registry authorization failure"));
        }
    }

    #[test]
    fn staged_pack_sync_uses_a_writable_handle() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("transport.tmp");
        fs::write(&staged, b"pack bytes").unwrap();

        sync_staged_pack(&staged).unwrap();
    }

    #[test]
    fn startup_cleanup_removes_only_stale_unpublished_packs() {
        let directory = tempfile::tempdir().unwrap();
        let pack_directory = directory.path().join("packs").join("transfer-1");
        fs::create_dir_all(&pack_directory).unwrap();
        let stale = pack_directory.join("stale.tmp");
        let recent = pack_directory.join("recent.tmp");
        let published = pack_directory.join("published.iscpack.zst");
        fs::write(&stale, b"stale").unwrap();
        fs::write(&recent, b"recent").unwrap();
        fs::write(&published, b"published").unwrap();
        let now = SystemTime::now();
        set_file_mtime(
            &stale,
            FileTime::from_system_time(now - STAGED_PACK_STALE_AFTER - Duration::from_secs(1)),
        )
        .unwrap();

        assert_eq!(
            cleanup_stale_staged_packs(directory.path(), now).unwrap(),
            1
        );
        assert!(!stale.exists());
        assert!(recent.exists());
        assert!(published.exists());
    }

    #[test]
    fn corrupt_database_is_archived_before_a_fresh_store_is_created() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("courier.db");
        let damaged_bytes = b"not a SQLite database";
        fs::write(&database, damaged_bytes).unwrap();

        let (archive, _) = ensure_database_integrity(&database).unwrap().unwrap();

        assert!(!database.exists());
        assert_eq!(fs::read(archive.join("courier.db")).unwrap(), damaged_bytes);
        drop(open_transfer_store(&database).unwrap());
        assert_eq!(TransferStore::integrity_check(&database).unwrap(), ["ok"]);
    }

    #[test]
    fn database_recovery_archive_preserves_sqlite_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("courier.db");
        let wal = directory.path().join("courier.db-wal");
        let shm = directory.path().join("courier.db-shm");
        fs::write(&database, b"damaged database").unwrap();
        fs::write(&wal, b"write-ahead log").unwrap();
        fs::write(&shm, b"shared memory index").unwrap();

        let archive = archive_corrupt_database(&database).unwrap();

        assert_eq!(
            fs::read(archive.join("courier.db")).unwrap(),
            b"damaged database"
        );
        assert_eq!(
            fs::read(archive.join("courier.db-wal")).unwrap(),
            b"write-ahead log"
        );
        assert_eq!(
            fs::read(archive.join("courier.db-shm")).unwrap(),
            b"shared memory index"
        );
    }

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

    #[cfg(target_os = "macos")]
    #[test]
    fn development_credentials_are_private_and_registry_scoped() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("courier.db");
        let first = RegistryCredentials {
            access_token: "first-access".into(),
            refresh_token: "first-refresh".into(),
        };
        let second = RegistryCredentials {
            access_token: "second-access".into(),
            refresh_token: "second-refresh".into(),
        };
        save_local_development_credentials(&database, "https://one.example.test", &first).unwrap();
        save_local_development_credentials(&database, "https://two.example.test", &second).unwrap();

        let saved = local_development_credentials(&database).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(
            saved["https://one.example.test"].refresh_token,
            "first-refresh"
        );
        assert_eq!(
            fs::metadata(database.with_file_name("registry-credentials.development.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
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

    #[test]
    fn transport_cache_is_bounded_and_missing_packs_are_planned_for_regeneration() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        for name in ["a.bin", "b.bin", "c.bin"] {
            fs::write(source.join(name), name.as_bytes()).unwrap();
        }
        let transfer_id = Uuid::new_v4();
        let files = inventory_transfer(transfer_id, &source, &InventoryOptions::default()).unwrap();
        let options = PackOptions {
            maximum_member_size: 8,
            target_pack_size: 8,
            zstd_level: 3,
        };
        // One independently encoded pack is larger than this limit, so none can
        // be retained. Their measured byte lengths still form a valid plan.
        let mut cache_limit_events = Vec::new();
        let mut observe_cache_limit = |members: &[&FileRecord], boundary: CacheBoundary| {
            cache_limit_events.push((
                members[0].relative_path.clone(),
                boundary.capacity,
                boundary.remaining,
                boundary.already_full,
            ));
        };
        let plan = prepare_transport_plan_with_observer(
            transfer_id,
            &files,
            directory.path(),
            options,
            8,
            &mut observe_cache_limit,
        )
        .unwrap();
        assert_eq!(cache_limit_events.len(), 3);
        assert!(
            cache_limit_events
                .iter()
                .all(|(_, capacity, remaining, full)| {
                    *capacity == 8 && *remaining == 8 && !full
                })
        );

        assert_eq!(plan.objects.len(), 3);
        assert!(
            plan.objects
                .iter()
                .all(|object| object.transport_bytes.is_some())
        );
        assert!(plan.objects.iter().all(|object| {
            object
                .cache_path
                .as_ref()
                .is_some_and(|path| !path.exists())
        }));
        let cache = directory.path().join("packs").join(transfer_id.to_string());
        let cached_bytes = fs::read_dir(cache)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum::<u64>();
        assert!(cached_bytes <= 8);

        let object = &plan.objects[0];
        let object_members = plan
            .members
            .iter()
            .filter(|member| member.object_id == object.id)
            .collect::<Vec<_>>();
        let files_by_id = files
            .iter()
            .map(|file| (file.id, file))
            .collect::<HashMap<_, _>>();
        let rebuilt = pack_upload_source(
            object,
            &object_members,
            &files_by_id,
            options,
            object.transport_bytes.unwrap(),
        )
        .unwrap();
        assert_eq!(rebuilt.size, object.transport_bytes.unwrap());
        assert!(rebuilt.absolute_path.exists());
    }

    #[test]
    fn compressible_large_files_use_zstd_when_it_saves_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("large.bin"), vec![b'x'; 9 * 1024 * 1024]).unwrap();
        let transfer_id = Uuid::new_v4();
        let files = inventory_transfer(transfer_id, &source, &InventoryOptions::default()).unwrap();

        let plan = prepare_transport_plan(transfer_id, &files, directory.path()).unwrap();

        assert_eq!(plan.objects.len(), 1);
        assert_eq!(plan.objects[0].kind, TransportObjectKind::Pack);
        assert_eq!(plan.objects[0].compression, "zstd");
        assert!(plan.objects[0].transport_bytes.unwrap() < files[0].size);
        assert_eq!(plan.members[0].member_index, 0);
    }
}
