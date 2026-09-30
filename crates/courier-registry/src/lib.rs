use std::{
    collections::HashMap,
    path::{Component, Path},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use courier_core::{
    FileRecord, HashAlgorithm, Transfer, TransportMemberRecord, TransportObjectRecord,
};
use courier_transfer::{MultipartStore, RemotePart, StoreError, UploadSession};
use reqwest::{Client, StatusCode, header::HeaderMap};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Registry request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Registry download I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Registry rejected the request ({status}): {detail}")]
    Rejected { status: StatusCode, detail: String },
    #[error("Registry response did not match local transfer state: {0}")]
    State(String),
    #[error("download paused")]
    Paused,
}

#[derive(Debug, Clone, Serialize)]
pub struct InvitationExchange<'a> {
    pub invitation_code: &'a str,
    pub client_identifier: &'a str,
    pub courier_version: &'a str,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryProject {
    pub id: Uuid,
    pub project_code: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistrySession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
    pub projects: Vec<RegistryProject>,
    pub purpose: RegistryInvitationPurpose,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RegistryInvitationPurpose {
    Upload,
    Download,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAuthorization {
    pub projects: Vec<RegistryProject>,
    pub purpose: RegistryInvitationPurpose,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryDownloadDataset {
    pub transfer_id: String,
    pub project_code: String,
    pub source_name: String,
    pub file_count: u64,
    pub original_bytes: u64,
    pub transport_bytes: Option<u64>,
    pub verified_at: DateTime<Utc>,
    pub hash_algorithm: HashAlgorithm,
}

#[derive(Clone, Deserialize)]
pub struct RegistryDownloadObject {
    pub object_id: Uuid,
    pub kind: String,
    pub compression: String,
    pub encoding_version: u8,
    pub original_bytes: u64,
    pub transport_bytes: Option<u64>,
    pub url: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct RegistryDownloadPlan {
    pub dataset: RegistryDownloadDataset,
    pub expires_in_seconds: u64,
    pub manifest: serde_json::Value,
    pub objects: Vec<RegistryDownloadObject>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistrySystemConfig {
    pub hash_algorithm: HashAlgorithm,
}

#[derive(Debug, Clone, Serialize)]
struct TransferCreate<'a> {
    project_code: &'a str,
    source_name: &'a str,
    file_count: u64,
    original_bytes: u64,
    manifest_version: u8,
    courier_version: &'a str,
    idempotency_key: String,
    hash_algorithm: HashAlgorithm,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryTransfer {
    #[serde(alias = "transfer_id")]
    pub public_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryTransferStatus {
    pub transfer_id: String,
    pub status: String,
    pub manifest_sha256: Option<String>,
    pub verification_attempt_count: u32,
    pub verification_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Manifest<'a> {
    schema: &'static str,
    version: u8,
    transfer_id: &'a str,
    project: &'a str,
    created_at: DateTime<Utc>,
    courier: ManifestCourier<'a>,
    source: ManifestSource<'a>,
    summary: ManifestSummary,
    transport_objects: Vec<ManifestTransportObject>,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestCourier<'a> {
    version: &'a str,
    platform: &'a str,
    transport_encoding_version: u8,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestSource<'a> {
    name: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestSummary {
    file_count: u64,
    original_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestFile {
    path: String,
    size: u64,
    mtime: DateTime<Utc>,
    digest: ManifestDigest,
    transport: ManifestFileTransport,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestDigest {
    algorithm: HashAlgorithm,
    value: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestTransportObject {
    id: Uuid,
    kind: String,
    compression: String,
    encoding_version: u8,
    original_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestFileTransport {
    object_id: Uuid,
    member_index: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryFile {
    pub id: Uuid,
    pub relative_path: String,
    pub object_key: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryObject {
    pub id: Uuid,
    pub object_key: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestReceipt {
    pub transfer_id: String,
    pub manifest_sha256: String,
    pub files: Vec<RegistryFile>,
    pub transport_objects: Vec<RegistryObject>,
}

#[derive(Debug, Clone, Copy)]
pub struct ManifestTransportPlan<'a> {
    pub objects: &'a [TransportObjectRecord],
    pub members: &'a [TransportMemberRecord],
}

#[derive(Clone)]
pub struct RegistryClient {
    base_url: String,
    auth: Option<Arc<Mutex<AuthState>>>,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    session_observer: Option<SessionObserver>,
    http: Client,
}

#[derive(Debug)]
struct AuthState {
    access_token: String,
    refresh_token: Option<String>,
}

type SessionObserver = Arc<dyn Fn(&RegistrySession) -> Result<(), String> + Send + Sync>;

fn http_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30 * 60))
        .build()
        .expect("Courier HTTP client configuration is valid")
}

impl RegistryClient {
    pub async fn system_config(&self) -> Result<RegistrySystemConfig, RegistryError> {
        self.send_json(
            self.http
                .get(format!("{}/api/v1/system/config", self.base_url)),
        )
        .await
    }

    pub async fn session_authorization(&self) -> Result<RegistryAuthorization, RegistryError> {
        self.send_json(
            self.authorized(
                self.http
                    .get(format!("{}/api/v1/auth/session", self.base_url)),
            )?,
        )
        .await
    }

    pub async fn downloadable_datasets(
        &self,
    ) -> Result<Vec<RegistryDownloadDataset>, RegistryError> {
        self.send_json(
            self.authorized(self.http.get(format!("{}/api/v1/downloads", self.base_url)))?,
        )
        .await
    }

    pub async fn download_plan(
        &self,
        transfer_id: &str,
    ) -> Result<RegistryDownloadPlan, RegistryError> {
        self.send_json(
            self.authorized(
                self.http
                    .post(format!("{}/api/v1/downloads/{transfer_id}", self.base_url)),
            )?,
        )
        .await
    }

    pub async fn authorize_download_object(
        &self,
        transfer_id: &str,
        object_id: Uuid,
    ) -> Result<RegistryDownloadObject, RegistryError> {
        self.send_json(self.authorized(self.http.post(format!(
            "{}/api/v1/downloads/{transfer_id}/objects/{object_id}/authorize",
            self.base_url
        )))?)
        .await
    }

    pub async fn download_object_resumable(
        &self,
        url: &str,
        destination: &Path,
        expected_bytes: Option<u64>,
        pause: Arc<AtomicBool>,
        mut progress: impl FnMut(u64),
    ) -> Result<u64, RegistryError> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        let partial_path = destination.with_extension("part");
        if let Ok(metadata) = tokio::fs::metadata(destination).await {
            let size = metadata.len();
            if expected_bytes.is_none_or(|expected| expected == size) {
                progress(size);
                return Ok(size);
            }
            tokio::fs::remove_file(destination).await?;
        }

        let mut offset = tokio::fs::metadata(&partial_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if expected_bytes.is_some_and(|expected| offset > expected) {
            tokio::fs::remove_file(&partial_path).await?;
            offset = 0;
        }
        if pause.load(Ordering::Acquire) {
            return Err(RegistryError::Paused);
        }
        if expected_bytes == Some(offset) && offset > 0 {
            tokio::fs::rename(&partial_path, destination).await?;
            progress(offset);
            return Ok(offset);
        }

        let mut request = self.http.get(url);
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let sending = request.send();
        tokio::pin!(sending);
        let mut response = loop {
            tokio::select! {
                result = &mut sending => break result?,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if pause.load(Ordering::Acquire) {
                        return Err(RegistryError::Paused);
                    }
                }
            }
        };
        let status = response.status();
        if !status.is_success() {
            let error = map_object_store_response(response).await;
            return Err(RegistryError::State(error.to_string()));
        }

        let append = if offset > 0 && status == StatusCode::PARTIAL_CONTENT {
            let content_range = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if !content_range.starts_with(&format!("bytes {offset}-")) {
                return Err(RegistryError::State(format!(
                    "object storage returned an invalid Content-Range for resume at byte {offset}"
                )));
            }
            true
        } else if offset > 0 && status == StatusCode::OK {
            // Some compatible S3 services ignore Range. Restart from zero rather
            // than appending a full response to the existing partial file.
            offset = 0;
            false
        } else if status == StatusCode::PARTIAL_CONTENT && offset == 0 {
            return Err(RegistryError::State(
                "object storage returned a partial response without a resume offset".into(),
            ));
        } else {
            false
        };

        let mut output = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&partial_path)
            .await?;
        if append {
            output.seek(std::io::SeekFrom::End(0)).await?;
        }
        let mut received = offset;
        progress(received);
        loop {
            let chunk = tokio::select! {
                result = response.chunk() => result?,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if pause.load(Ordering::Acquire) {
                        output.flush().await?;
                        output.sync_data().await?;
                        return Err(RegistryError::Paused);
                    }
                    continue;
                }
            };
            let Some(chunk) = chunk else { break };
            if pause.load(Ordering::Acquire) {
                output.flush().await?;
                output.sync_data().await?;
                return Err(RegistryError::Paused);
            }
            output.write_all(&chunk).await?;
            received = received.saturating_add(chunk.len() as u64);
            progress(received);
        }
        output.flush().await?;
        output.sync_data().await?;
        drop(output);
        if expected_bytes.is_some_and(|expected| expected != received) {
            return Err(RegistryError::State(format!(
                "downloaded object has {received} bytes; expected {}",
                expected_bytes.unwrap_or_default()
            )));
        }
        tokio::fs::rename(&partial_path, destination).await?;
        Ok(received)
    }
    pub fn unauthenticated(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            auth: None,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            session_observer: None,
            http: http_client(),
        }
    }

    pub fn authenticated(base_url: impl Into<String>, bearer: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            auth: Some(Arc::new(Mutex::new(AuthState {
                access_token: bearer.into(),
                refresh_token: None,
            }))),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            session_observer: None,
            http: http_client(),
        }
    }

    pub fn renewable(
        base_url: impl Into<String>,
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        session_observer: SessionObserver,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            auth: Some(Arc::new(Mutex::new(AuthState {
                access_token: access_token.into(),
                refresh_token: Some(refresh_token.into()),
            }))),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            session_observer: Some(session_observer),
            http: http_client(),
        }
    }

    pub async fn exchange_invitation(
        &self,
        invitation_code: &str,
        client_identifier: &str,
    ) -> Result<RegistrySession, RegistryError> {
        self.send_json(
            self.http
                .post(format!(
                    "{}/api/v1/auth/invitations/exchange",
                    self.base_url
                ))
                .json(&InvitationExchange {
                    invitation_code,
                    client_identifier,
                    courier_version: env!("CARGO_PKG_VERSION"),
                }),
        )
        .await
    }

    pub async fn refresh_session(
        &self,
        refresh_token: &str,
    ) -> Result<RegistrySession, RegistryError> {
        #[derive(Serialize)]
        struct RefreshRequest<'a> {
            refresh_token: &'a str,
        }

        let response = self
            .http
            .post(format!("{}/api/v1/auth/sessions/refresh", self.base_url))
            .json(&RefreshRequest { refresh_token })
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(RegistryError::Rejected {
                status,
                detail: response.text().await.unwrap_or_default(),
            });
        }
        Ok(response.json().await?)
    }

    pub async fn register_transfer(
        &self,
        transfer: &Transfer,
        project_code: &str,
        source_name: &str,
        hash_algorithm: HashAlgorithm,
    ) -> Result<RegistryTransfer, RegistryError> {
        self.send_json(
            self.authorized(
                self.http
                    .post(format!("{}/api/v1/transfers", self.base_url)),
            )?
            .json(&TransferCreate {
                project_code,
                source_name,
                file_count: transfer.file_count,
                original_bytes: transfer.original_bytes,
                manifest_version: transfer.manifest_version,
                courier_version: env!("CARGO_PKG_VERSION"),
                idempotency_key: transfer.id.to_string(),
                hash_algorithm,
            }),
        )
        .await
    }

    pub async fn submit_manifest(
        &self,
        transfer: &Transfer,
        server_transfer_id: &str,
        project_code: &str,
        source_name: &str,
        files: &[FileRecord],
        transport: ManifestTransportPlan<'_>,
    ) -> Result<ManifestReceipt, RegistryError> {
        let member_by_file = transport
            .members
            .iter()
            .map(|member| (member.file_id, member))
            .collect::<HashMap<_, _>>();
        let manifest_files = files
            .iter()
            .map(|file| {
                let member = member_by_file.get(&file.id).ok_or_else(|| {
                    RegistryError::State(format!("transport plan omitted logical file {}", file.id))
                })?;
                let seconds = file.mtime_ns.div_euclid(1_000_000_000);
                let nanos = file.mtime_ns.rem_euclid(1_000_000_000) as u32;
                let mtime = Utc
                    .timestamp_opt(seconds, nanos)
                    .single()
                    .ok_or_else(|| RegistryError::State("invalid file modification time".into()))?;
                Ok(ManifestFile {
                    path: portable_relative_path(file)?,
                    size: file.size,
                    mtime,
                    digest: ManifestDigest {
                        algorithm: file.hash_algorithm,
                        value: file.sha256.clone(),
                    },
                    transport: ManifestFileTransport {
                        object_id: member.object_id,
                        member_index: member.member_index,
                    },
                })
            })
            .collect::<Result<Vec<_>, RegistryError>>()?;
        let payload = Manifest {
            schema: "icy-seas-transfer-manifest",
            version: 3,
            transfer_id: server_transfer_id,
            project: project_code,
            created_at: transfer.created_at,
            courier: ManifestCourier {
                version: env!("CARGO_PKG_VERSION"),
                platform: std::env::consts::OS,
                transport_encoding_version: 2,
            },
            source: ManifestSource { name: source_name },
            summary: ManifestSummary {
                file_count: transfer.file_count,
                original_bytes: transfer.original_bytes,
            },
            transport_objects: transport
                .objects
                .iter()
                .map(|object| ManifestTransportObject {
                    id: object.id,
                    kind: object.kind.to_string(),
                    compression: object.compression.clone(),
                    encoding_version: object.encoding_version,
                    original_bytes: object.original_bytes,
                })
                .collect(),
            files: manifest_files,
        };
        self.send_json(
            self.authorized(self.http.put(format!(
                "{}/api/v1/transfers/{server_transfer_id}/manifest",
                self.base_url
            )))?
            .json(&payload),
        )
        .await
    }

    pub async fn finalize_transfer(
        &self,
        server_transfer_id: &str,
    ) -> Result<RegistryTransfer, RegistryError> {
        self.send_json(self.authorized(self.http.post(format!(
            "{}/api/v1/transfers/{server_transfer_id}/finalize",
            self.base_url
        )))?)
        .await
    }

    pub async fn transfer_status(
        &self,
        server_transfer_id: &str,
    ) -> Result<RegistryTransferStatus, RegistryError> {
        self.send_json(self.authorized(self.http.get(format!(
            "{}/api/v1/transfers/{server_transfer_id}",
            self.base_url
        )))?)
        .await
    }

    fn authorized(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, RegistryError> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            RegistryError::State("authenticated Registry session required".into())
        })?;
        let bearer = auth
            .lock()
            .map_err(|_| RegistryError::State("Registry credentials are unavailable".into()))?
            .access_token
            .clone();
        Ok(request.bearer_auth(bearer))
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, RegistryError> {
        let retry = request.try_clone();
        let mut response = request.send().await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && let (Some(auth), Some(observer), Some(retry)) =
                (&self.auth, &self.session_observer, retry)
        {
            let _guard = self.refresh_lock.lock().await;
            let refresh_token = auth
                .lock()
                .map_err(|_| RegistryError::State("Registry credentials are unavailable".into()))?
                .refresh_token
                .clone()
                .ok_or_else(|| {
                    RegistryError::State("renewable Registry session required".into())
                })?;
            let session = self.refresh_session(&refresh_token).await?;
            observer(&session).map_err(RegistryError::State)?;
            {
                let mut state = auth.lock().map_err(|_| {
                    RegistryError::State("Registry credentials are unavailable".into())
                })?;
                state.access_token = session.access_token.clone();
                state.refresh_token = Some(session.refresh_token.clone());
            }
            response = retry.bearer_auth(&session.access_token).send().await?;
        }
        let status = response.status();
        if !status.is_success() {
            return Err(RegistryError::Rejected {
                status,
                detail: response.text().await.unwrap_or_default(),
            });
        }
        Ok(response.json().await?)
    }
}

fn portable_relative_path(file: &FileRecord) -> Result<String, RegistryError> {
    let mut components = Vec::new();
    for component in file.relative_path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            _ => {
                return Err(RegistryError::State(
                    "file path is not safely relative".into(),
                ));
            }
        }
    }
    Ok(components.join("/"))
}

#[derive(Debug, Clone)]
pub struct RegistryObjectBinding {
    pub server_object_id: Uuid,
    pub object_key: String,
}

#[derive(Clone)]
pub struct RegistryMultipartStore {
    client: RegistryClient,
    server_transfer_id: String,
    files: Arc<HashMap<String, Uuid>>,
    part_progress: SharedPartProgressObserver,
    pause_flag: SharedPauseFlag,
}

type PartProgressObserver = Arc<dyn Fn(u64) + Send + Sync>;
type SharedPartProgressObserver = Arc<Mutex<Option<PartProgressObserver>>>;
type SharedPauseFlag = Arc<Mutex<Option<Arc<AtomicBool>>>>;

#[derive(Deserialize)]
struct MultipartResponse {
    upload_id: String,
}

#[derive(Deserialize)]
struct PartsResponse {
    parts: Vec<PartResponse>,
}

#[derive(Deserialize)]
struct PartResponse {
    part_number: u32,
    etag: String,
    size: Option<u64>,
}

#[derive(Deserialize)]
struct AuthorizationResponse {
    url: String,
}

#[derive(Deserialize)]
struct ObjectStatusResponse {
    exists: bool,
}

#[derive(Serialize)]
struct CompleteRequest<'a> {
    parts: Vec<CompletePart<'a>>,
}

#[derive(Serialize)]
struct CompletePart<'a> {
    part_number: u32,
    etag: &'a str,
    size: u64,
}

impl RegistryMultipartStore {
    pub fn new(
        client: RegistryClient,
        server_transfer_id: impl Into<String>,
        bindings: impl IntoIterator<Item = RegistryObjectBinding>,
    ) -> Self {
        Self {
            client,
            server_transfer_id: server_transfer_id.into(),
            files: Arc::new(
                bindings
                    .into_iter()
                    .map(|binding| (binding.object_key, binding.server_object_id))
                    .collect(),
            ),
            part_progress: Arc::new(Mutex::new(None)),
            pause_flag: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_part_progress_observer(
        &self,
        observer: Option<PartProgressObserver>,
    ) -> Result<(), StoreError> {
        *self.part_progress.lock().map_err(|_| {
            StoreError::Permanent("upload progress monitor is unavailable".into())
        })? = observer;
        Ok(())
    }

    pub fn set_pause_flag(&self, pause: Option<Arc<AtomicBool>>) -> Result<(), StoreError> {
        *self
            .pause_flag
            .lock()
            .map_err(|_| StoreError::Permanent("upload pause control is unavailable".into()))? =
            pause;
        Ok(())
    }

    fn file_id(&self, object_key: &str) -> Result<Uuid, StoreError> {
        self.files.get(object_key).copied().ok_or_else(|| {
            StoreError::Permanent("object key is not bound to this Registry transfer".into())
        })
    }

    fn endpoint(&self, file_id: Uuid, suffix: &str) -> String {
        format!(
            "{}/api/v1/transfers/{}/objects/{file_id}/multipart{suffix}",
            self.client.base_url, self.server_transfer_id
        )
    }

    async fn registry_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, StoreError> {
        self.client
            .send_json(request)
            .await
            .map_err(map_registry_error)
    }
}

#[async_trait]
impl MultipartStore for RegistryMultipartStore {
    async fn begin(&self, object_key: &str) -> Result<UploadSession, StoreError> {
        let file_id = self.file_id(object_key)?;
        let response: MultipartResponse = self
            .registry_json(
                self.client
                    .authorized(self.client.http.post(self.endpoint(file_id, "")))
                    .map_err(map_registry_error)?,
            )
            .await?;
        Ok(UploadSession {
            object_key: object_key.into(),
            upload_id: response.upload_id,
        })
    }

    async fn list_parts(&self, session: &UploadSession) -> Result<Vec<RemotePart>, StoreError> {
        let file_id = self.file_id(&session.object_key)?;
        let response: PartsResponse = self
            .registry_json(
                self.client
                    .authorized(self.client.http.get(self.endpoint(file_id, "/parts")))
                    .map_err(map_registry_error)?,
            )
            .await?;
        Ok(response
            .parts
            .into_iter()
            .map(|part| RemotePart {
                part_number: part.part_number,
                etag: part.etag,
                size: part.size.unwrap_or(0),
            })
            .collect())
    }

    async fn upload_part(
        &self,
        session: &UploadSession,
        part_number: u32,
        bytes: Vec<u8>,
    ) -> Result<RemotePart, StoreError> {
        let file_id = self.file_id(&session.object_key)?;
        let authorization: AuthorizationResponse = self
            .registry_json(
                self.client
                    .authorized(
                        self.client.http.post(
                            self.endpoint(file_id, &format!("/parts/{part_number}/authorize")),
                        ),
                    )
                    .map_err(map_registry_error)?,
            )
            .await?;
        let size = bytes.len() as u64;
        let content = Arc::new(bytes);
        let progress = self
            .part_progress
            .lock()
            .map_err(|_| StoreError::Permanent("upload progress monitor is unavailable".into()))?
            .clone();
        let pause = self
            .pause_flag
            .lock()
            .map_err(|_| StoreError::Permanent("upload pause control is unavailable".into()))?
            .clone();
        if pause
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(StoreError::Paused);
        }
        let stream_pause = pause.clone();
        let stream = futures_util::stream::unfold((content, 0_usize), move |(content, offset)| {
            let progress = progress.clone();
            let pause = stream_pause.clone();
            async move {
                if offset >= content.len() {
                    return None;
                }
                if pause
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Acquire))
                {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "upload paused",
                        )),
                        (content, offset),
                    ));
                }
                const CHUNK_SIZE: usize = 256 * 1024;
                let end = offset.saturating_add(CHUNK_SIZE).min(content.len());
                let chunk = bytes::Bytes::copy_from_slice(&content[offset..end]);
                if let Some(observer) = progress {
                    observer(end as u64);
                }
                Some((Ok::<_, std::io::Error>(chunk), (content, end)))
            }
        });
        let request = self
            .client
            .http
            .put(authorization.url)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(stream))
            .send();
        tokio::pin!(request);
        let response = loop {
            tokio::select! {
                result = &mut request => {
                    if pause
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Acquire))
                    {
                        return Err(StoreError::Paused);
                    }
                    break result.map_err(map_transport)?;
                }
                _ = tokio::time::sleep(Duration::from_millis(25)), if pause.is_some() => {
                    if pause
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Acquire))
                    {
                        return Err(StoreError::Paused);
                    }
                }
            }
        };
        if !response.status().is_success() {
            return Err(map_object_store_response(response).await);
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| StoreError::Permanent("part upload response omitted ETag".into()))?
            .to_owned();
        Ok(RemotePart {
            part_number,
            etag,
            size,
        })
    }

    async fn complete(
        &self,
        session: &UploadSession,
        parts: &[RemotePart],
    ) -> Result<(), StoreError> {
        let file_id = self.file_id(&session.object_key)?;
        let payload = CompleteRequest {
            parts: parts
                .iter()
                .map(|part| CompletePart {
                    part_number: part.part_number,
                    etag: &part.etag,
                    size: part.size,
                })
                .collect(),
        };
        let _: serde_json::Value = self
            .registry_json(
                self.client
                    .authorized(
                        self.client
                            .http
                            .post(self.endpoint(file_id, "/complete"))
                            .json(&payload),
                    )
                    .map_err(map_registry_error)?,
            )
            .await?;
        Ok(())
    }

    async fn object_exists(&self, object_key: &str) -> Result<bool, StoreError> {
        let file_id = self.file_id(object_key)?;
        let response: ObjectStatusResponse = self
            .registry_json(
                self.client
                    .authorized(self.client.http.get(format!(
                        "{}/api/v1/transfers/{}/objects/{file_id}/object",
                        self.client.base_url, self.server_transfer_id
                    )))
                    .map_err(map_registry_error)?,
            )
            .await?;
        Ok(response.exists)
    }

    async fn abort(&self, _session: &UploadSession) -> Result<(), StoreError> {
        Err(StoreError::Permanent(
            "Registry upload cancellation is not implemented".into(),
        ))
    }
}

fn map_registry_error(error: RegistryError) -> StoreError {
    match error {
        RegistryError::Transport(error) => map_transport(error),
        RegistryError::Io(error) => StoreError::Permanent(error.to_string()),
        RegistryError::Rejected { status, detail } => map_status(status, detail),
        RegistryError::State(detail) => StoreError::Permanent(detail),
        RegistryError::Paused => StoreError::Permanent("download paused".into()),
    }
}

fn map_transport(error: reqwest::Error) -> StoreError {
    // A request can fail after it has been built and while its body is being
    // sent (for example, if the connection is reset). Those are transport
    // failures too, and retrying the same multipart part is safe. Builder and
    // other local/configuration errors remain permanent.
    if error.is_timeout() || error.is_connect() || error.is_request() || error.is_body() {
        StoreError::Transient(error.to_string())
    } else {
        StoreError::Permanent(error.to_string())
    }
}

fn map_status(status: StatusCode, detail: String) -> StoreError {
    match status.as_u16() {
        401 | 403 => StoreError::AuthorizationExpired,
        404 => StoreError::UploadNotFound,
        408 | 425 | 429 | 500..=599 => StoreError::Transient(detail),
        _ => StoreError::Permanent(detail),
    }
}

async fn map_object_store_response(response: reqwest::Response) -> StoreError {
    let status = response.status();
    let destination = response.url().host_str().map(str::to_owned);
    let headers = response.headers().clone();
    let body = response_body_preview(response).await;
    let (detail, cloudflare, cloudflare_challenge) =
        object_store_response_detail(status, destination.as_deref(), &headers, &body);

    if cloudflare_challenge {
        return StoreError::Permanent(detail);
    }

    // A Cloudflare-generated 400 is not an S3 validation result. It can be a
    // transient edge or tunnel failure, and every retry obtains a fresh
    // presigned URL before replaying the idempotent part PUT.
    if cloudflare && status == StatusCode::BAD_REQUEST {
        StoreError::Transient(detail)
    } else {
        map_status(status, detail)
    }
}

async fn response_body_preview(response: reqwest::Response) -> String {
    use futures_util::StreamExt;

    const MAX_RESPONSE_BYTES: usize = 1_024;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while bytes.len() < MAX_RESPONSE_BYTES {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = MAX_RESPONSE_BYTES - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn object_store_response_detail(
    status: StatusCode,
    destination: Option<&str>,
    headers: &HeaderMap,
    body: &str,
) -> (String, bool, bool) {
    let cloudflare = header_value(headers, "cf-ray").is_some()
        || header_value(headers, "server")
            .is_some_and(|value| value.eq_ignore_ascii_case("cloudflare"));
    let provider = if cloudflare {
        "Cloudflare"
    } else {
        "object store"
    };
    let mut evidence = vec![format!("HTTP {status}"), format!("provider {provider}")];
    let cloudflare_challenge = cloudflare && is_cloudflare_challenge(body);
    if cloudflare_challenge {
        evidence.push("Cloudflare security verification challenge detected".into());
    }
    if let Some(destination) = destination {
        evidence.push(format!("destination {destination}"));
    }
    for name in [
        "cf-ray",
        "server",
        "content-type",
        "x-amz-request-id",
        "x-amz-id-2",
        "x-goog-request-id",
    ] {
        if let Some(value) = header_value(headers, name) {
            evidence.push(format!("{name} {value}"));
        }
    }
    let mut detail = format!("Object-store request rejected ({})", evidence.join("; "));
    if let Some(preview) = safe_response_preview(body) {
        detail.push_str(&format!(". Response preview: {preview}"));
    }
    if cloudflare_challenge {
        detail.push_str(
            ". Courier cannot complete a browser CAPTCHA or JavaScript challenge during a presigned object-store request; ask the Courier administrator to exempt the S3 upload hostname from Cloudflare challenges.",
        );
    }
    (detail, cloudflare, cloudflare_challenge)
}

fn is_cloudflare_challenge(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    [
        "cf-chl-",
        "/cdn-cgi/challenge-platform",
        "just a moment...",
        "captcha",
    ]
    .iter()
    .any(|marker| body.contains(marker))
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(truncate_diagnostic(value, 160))
}

fn safe_response_preview(body: &str) -> Option<String> {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    let lower = compact.to_ascii_lowercase();
    if [
        "x-amz-signature",
        "x-amz-credential",
        "x-amz-security-token",
        "authorization:",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Some("omitted because the response contained signed-request material".into());
    }
    Some(truncate_diagnostic(&compact, 480))
}

fn truncate_diagnostic(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let truncated = value.chars().take(limit).collect::<String>();
    format!("{truncated}…")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use courier_core::FileStatus;

    use super::*;

    #[test]
    fn manifest_paths_are_portable_and_relative() {
        let file = FileRecord {
            id: Uuid::new_v4(),
            transfer_id: Uuid::new_v4(),
            relative_path: PathBuf::from("casts").join("cast-001.csv"),
            absolute_path: PathBuf::from("/source/casts/cast-001.csv"),
            size: 10,
            mtime_ns: 0,
            hash_algorithm: HashAlgorithm::Sha256,
            sha256: "0".repeat(64),
            status: FileStatus::Ready,
            bytes_completed: 0,
        };
        assert_eq!(portable_relative_path(&file).unwrap(), "casts/cast-001.csv");
    }

    #[test]
    fn status_mapping_preserves_retry_and_authorization_meaning() {
        assert!(matches!(
            map_status(StatusCode::UNAUTHORIZED, String::new()),
            StoreError::AuthorizationExpired
        ));
        assert!(matches!(
            map_status(StatusCode::SERVICE_UNAVAILABLE, String::new()),
            StoreError::Transient(_)
        ));
        assert!(matches!(
            map_status(StatusCode::UNPROCESSABLE_ENTITY, String::new()),
            StoreError::Permanent(_)
        ));
    }

    #[tokio::test]
    async fn interrupted_request_send_is_retryable_but_builder_errors_are_not() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_headers = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request_headers
                .windows(4)
                .any(|window| window == b"\r\n\r\n")
            {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request_headers.extend_from_slice(&buffer[..count]);
            }
            // Closing without a response simulates a peer dropping an upload.
        });

        let send_error = Client::new()
            .put(format!("http://{address}/part"))
            .body(vec![0_u8; 8 * 1024 * 1024])
            .send()
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(map_transport(send_error).is_retryable());

        let builder_error = Client::new().get("not a valid URL").build().unwrap_err();
        assert!(matches!(
            map_transport(builder_error),
            StoreError::Permanent(_)
        ));
    }

    #[test]
    fn cloudflare_rejections_include_safe_support_evidence_and_retry() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", "a1b2c3d4-SEA".parse().unwrap());
        headers.insert("server", "cloudflare".parse().unwrap());
        headers.insert("content-type", "text/html".parse().unwrap());
        let (detail, cloudflare, challenge) = object_store_response_detail(
            StatusCode::BAD_REQUEST,
            Some("s3.icyseascolab.io"),
            &headers,
            "<html><title>400 Bad Request</title><center>cloudflare</center></html>",
        );

        assert!(cloudflare);
        assert!(!challenge);
        assert!(detail.contains("HTTP 400 Bad Request"));
        assert!(detail.contains("destination s3.icyseascolab.io"));
        assert!(detail.contains("cf-ray a1b2c3d4-SEA"));
        assert!(matches!(
            if cloudflare {
                StoreError::Transient(detail)
            } else {
                StoreError::Permanent(detail)
            },
            StoreError::Transient(_)
        ));
    }

    #[test]
    fn object_store_diagnostics_keep_s3_request_ids_but_redact_signed_material() {
        let mut headers = HeaderMap::new();
        headers.insert("x-amz-request-id", "request-123".parse().unwrap());
        let (detail, cloudflare, challenge) = object_store_response_detail(
            StatusCode::FORBIDDEN,
            Some("s3.example.test"),
            &headers,
            "Signature failure X-Amz-Signature=should-not-appear",
        );

        assert!(!cloudflare);
        assert!(!challenge);
        assert!(detail.contains("x-amz-request-id request-123"));
        assert!(detail.contains("omitted because the response contained signed-request material"));
        assert!(!detail.contains("should-not-appear"));
    }

    #[test]
    fn cloudflare_challenges_tell_the_user_that_browser_verification_is_unsupported() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", "challenge-ray".parse().unwrap());
        headers.insert("server", "cloudflare".parse().unwrap());
        let (detail, cloudflare, challenge) = object_store_response_detail(
            StatusCode::FORBIDDEN,
            Some("s3.icyseascolab.io"),
            &headers,
            "<html><title>Just a moment...</title><div id=cf-chl-widget></div></html>",
        );

        assert!(cloudflare);
        assert!(challenge);
        assert!(detail.contains("security verification challenge detected"));
        assert!(detail.contains("cannot complete a browser CAPTCHA"));
    }

    #[tokio::test]
    async fn renewable_client_refreshes_after_unauthorized_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (index, expected) in [
                "/api/v1/transfers/ISC-TR-TEST",
                "/api/v1/auth/sessions/refresh",
                "/api/v1/transfers/ISC-TR-TEST",
            ]
            .into_iter()
            .enumerate()
            {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|value| value == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                assert!(request.contains(expected));
                let (status, body) = match index {
                    0 => ("401 Unauthorized", r#"{"detail":"expired"}"#),
                    1 => (
                        "200 OK",
                        r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_at":"2030-01-01T00:00:00Z","refresh_expires_at":"2030-02-01T00:00:00Z","projects":[],"purpose":"upload"}"#,
                    ),
                    _ => (
                        "200 OK",
                        r#"{"transfer_id":"ISC-TR-TEST","status":"complete","manifest_sha256":null,"verification_attempt_count":1,"verification_error":null}"#,
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let observed = Arc::new(AtomicBool::new(false));
        let observer_flag = observed.clone();
        let client = RegistryClient::renewable(
            format!("http://{address}"),
            "old-access",
            "old-refresh",
            Arc::new(move |session| {
                assert_eq!(session.refresh_token, "new-refresh");
                observer_flag.store(true, Ordering::SeqCst);
                Ok(())
            }),
        );

        let status = client.transfer_status("ISC-TR-TEST").await.unwrap();
        assert_eq!(status.status, "complete");
        assert!(observed.load(Ordering::SeqCst));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn registry_upload_stream_reports_live_bytes_without_changing_the_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected = vec![0x5a_u8; 900_000];
        let expected_for_server = expected.clone();
        let server = std::thread::spawn(move || {
            let (mut authorization, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let count = authorization.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|value| value == b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!(r#"{{"url":"http://{address}/object"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            authorization.write_all(response.as_bytes()).unwrap();

            let (mut upload, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let count = upload.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if let Some(index) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(headers.contains(&format!("content-length: {}", expected_for_server.len())));
            while request.len() - header_end < expected_for_server.len() {
                let count = upload.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            assert_eq!(&request[header_end..], expected_for_server.as_slice());
            upload
                .write_all(
                    b"HTTP/1.1 200 OK\r\nETag: \"streamed\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        let object_id = Uuid::new_v4();
        let store = RegistryMultipartStore::new(
            RegistryClient::authenticated(format!("http://{address}"), "test-token"),
            "ISC-TR-STREAM",
            [RegistryObjectBinding {
                server_object_id: object_id,
                object_key: "opaque/object".into(),
            }],
        );
        let observed = Arc::new(AtomicU64::new(0));
        let observed_for_callback = observed.clone();
        store
            .set_part_progress_observer(Some(Arc::new(move |bytes| {
                observed_for_callback.store(bytes, Ordering::SeqCst);
            })))
            .unwrap();
        let result = store
            .upload_part(
                &UploadSession {
                    object_key: "opaque/object".into(),
                    upload_id: "upload-id".into(),
                },
                1,
                expected.clone(),
            )
            .await
            .unwrap();
        assert_eq!(result.size, expected.len() as u64);
        assert_eq!(observed.load(Ordering::SeqCst), expected.len() as u64);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn registry_upload_pause_cancels_an_in_flight_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut authorization, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let count = authorization.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|value| value == b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!(r#"{{"url":"http://{address}/object"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            authorization.write_all(response.as_bytes()).unwrap();

            let (mut upload, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let count = upload.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|value| value == b"\r\n\r\n") {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        });

        let object_id = Uuid::new_v4();
        let store = RegistryMultipartStore::new(
            RegistryClient::authenticated(format!("http://{address}"), "test-token"),
            "ISC-TR-PAUSE",
            [RegistryObjectBinding {
                server_object_id: object_id,
                object_key: "opaque/object".into(),
            }],
        );
        let pause = Arc::new(AtomicBool::new(false));
        store.set_pause_flag(Some(pause.clone())).unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pause.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let result = store
            .upload_part(
                &UploadSession {
                    object_key: "opaque/object".into(),
                    upload_id: "upload-id".into(),
                },
                1,
                vec![0x5a_u8; 8 * 1024 * 1024],
            )
            .await;
        assert!(matches!(result, Err(StoreError::Paused)));
        assert!(started.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }
}
