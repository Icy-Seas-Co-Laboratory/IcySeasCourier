use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSession {
    pub object_key: String,
    pub upload_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePart {
    pub part_number: u32,
    pub etag: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("transient object-store failure: {0}")]
    Transient(String),
    #[error("upload authorization expired")]
    AuthorizationExpired,
    #[error("multipart upload no longer exists")]
    UploadNotFound,
    #[error("permanent object-store failure: {0}")]
    Permanent(String),
}

impl StoreError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::AuthorizationExpired)
    }
}

/// The narrow data-plane contract used by Courier. Implementations may use
/// direct S3 credentials in local development or Registry-issued URLs later.
#[async_trait]
pub trait MultipartStore: Send + Sync {
    async fn begin(&self, object_key: &str) -> Result<UploadSession, StoreError>;
    async fn list_parts(&self, session: &UploadSession) -> Result<Vec<RemotePart>, StoreError>;
    async fn upload_part(
        &self,
        session: &UploadSession,
        part_number: u32,
        bytes: Vec<u8>,
    ) -> Result<RemotePart, StoreError>;
    async fn complete(
        &self,
        session: &UploadSession,
        parts: &[RemotePart],
    ) -> Result<(), StoreError>;
    /// Checks only whether the finalized object exists. Courier uses this to
    /// resolve an ambiguous completion response; it never reads object bytes.
    async fn object_exists(&self, object_key: &str) -> Result<bool, StoreError>;
    async fn abort(&self, session: &UploadSession) -> Result<(), StoreError>;
}
