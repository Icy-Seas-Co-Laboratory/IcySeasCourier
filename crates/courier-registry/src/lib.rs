use std::{collections::HashMap, path::Component, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use courier_core::{FileRecord, HashAlgorithm, Transfer};
use courier_transfer::{MultipartStore, RemotePart, StoreError, UploadSession};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Registry request failed: {0}")]
    Transport(#[from] reqwest::Error),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<ManifestDigest>,
    transport: ManifestTransport,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestDigest {
    algorithm: HashAlgorithm,
    value: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestTransport {
    compression: &'static str,
    encoding_version: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryFile {
    pub id: Uuid,
    pub relative_path: String,
    pub object_key: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestReceipt {
    pub transfer_id: String,
    pub manifest_sha256: String,
    pub files: Vec<RegistryFile>,
}

#[derive(Debug, Clone)]
pub struct RegistryClient {
    base_url: String,
    bearer: Option<String>,
    http: Client,
}

impl RegistryClient {
    pub async fn system_config(&self) -> Result<RegistrySystemConfig, RegistryError> {
        self.send_json(
            self.http
                .get(format!("{}/api/v1/system/config", self.base_url)),
        )
        .await
    }
    pub fn unauthenticated(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            bearer: None,
            http: Client::new(),
        }
    }

    pub fn authenticated(base_url: impl Into<String>, bearer: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            bearer: Some(bearer.into()),
            http: Client::new(),
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

        self.send_json(
            self.http
                .post(format!("{}/api/v1/auth/sessions/refresh", self.base_url))
                .json(&RefreshRequest { refresh_token }),
        )
        .await
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
    ) -> Result<ManifestReceipt, RegistryError> {
        let manifest_files = files
            .iter()
            .map(|file| {
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
                    sha256: (transfer.manifest_version == 1).then(|| file.sha256.clone()),
                    digest: (transfer.manifest_version == 2).then(|| ManifestDigest {
                        algorithm: file.hash_algorithm,
                        value: file.sha256.clone(),
                    }),
                    transport: ManifestTransport {
                        compression: "none",
                        encoding_version: 1,
                    },
                })
            })
            .collect::<Result<Vec<_>, RegistryError>>()?;
        let payload = Manifest {
            schema: "icy-seas-transfer-manifest",
            version: transfer.manifest_version,
            transfer_id: server_transfer_id,
            project: project_code,
            created_at: transfer.created_at,
            courier: ManifestCourier {
                version: env!("CARGO_PKG_VERSION"),
                platform: std::env::consts::OS,
                transport_encoding_version: 1,
            },
            source: ManifestSource { name: source_name },
            summary: ManifestSummary {
                file_count: transfer.file_count,
                original_bytes: transfer.original_bytes,
            },
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
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            RegistryError::State("authenticated Registry session required".into())
        })?;
        Ok(request.bearer_auth(bearer))
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, RegistryError> {
        let response = request.send().await?;
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
pub struct RegistryFileBinding {
    pub server_file_id: Uuid,
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
        bindings: impl IntoIterator<Item = RegistryFileBinding>,
    ) -> Self {
        Self {
            client,
            server_transfer_id: server_transfer_id.into(),
            files: Arc::new(
                bindings
                    .into_iter()
                    .map(|binding| (binding.object_key, binding.server_file_id))
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
            "{}/api/v1/transfers/{}/files/{file_id}/multipart{suffix}",
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
                        "{}/api/v1/transfers/{}/files/{file_id}/object",
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
    use std::path::PathBuf;

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
}
