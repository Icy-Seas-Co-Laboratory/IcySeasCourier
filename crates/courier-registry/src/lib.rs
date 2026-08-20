use std::{
    collections::HashMap,
    path::{Component, Path},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use courier_core::{
    FileRecord, HashAlgorithm, Transfer, TransportMemberRecord, TransportObjectRecord,
};
use courier_transfer::{MultipartStore, RemotePart, StoreError, UploadSession};
use reqwest::{Client, StatusCode};
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

    pub async fn download_object(
        &self,
        url: &str,
        destination: &Path,
        mut progress: impl FnMut(u64),
    ) -> Result<u64, RegistryError> {
        use tokio::io::AsyncWriteExt;

        let mut response = self.http.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(RegistryError::Rejected {
                status,
                detail: "object storage rejected the download".into(),
            });
        }
        let mut output = tokio::fs::File::create(destination).await?;
        let mut received = 0_u64;
        while let Some(chunk) = response.chunk().await? {
            output.write_all(&chunk).await?;
            received = received.saturating_add(chunk.len() as u64);
            progress(received);
        }
        output.flush().await?;
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
}

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
        }
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
        let response = self
            .client
            .http
            .put(authorization.url)
            .body(bytes)
            .send()
            .await
            .map_err(map_transport)?;
        if !response.status().is_success() {
            return Err(map_status(
                response.status(),
                "presigned part upload rejected".into(),
            ));
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
    }
}

fn map_transport(error: reqwest::Error) -> StoreError {
    if error.is_timeout() || error.is_connect() {
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

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::atomic::{AtomicBool, Ordering},
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
}
