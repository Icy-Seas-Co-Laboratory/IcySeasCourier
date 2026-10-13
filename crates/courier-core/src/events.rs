use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persisted operational events deliberately exclude free-form error messages,
/// paths, request bodies, credentials, and signed URLs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEvent {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub kind: String,
    pub details: TransferEventDetails,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferEventDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::TransferStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_state: Option<crate::TransferStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<TransferErrorClass>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferErrorClass {
    Transient,
    AuthorizationExpired,
    UploadNotFound,
    Permanent,
    Paused,
    Local,
    SourceChanged,
    Reconciliation,
    InvalidPlan,
    RetriesExhausted,
}
