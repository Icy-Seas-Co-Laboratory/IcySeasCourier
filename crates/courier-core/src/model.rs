use std::{fmt, path::PathBuf, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CourierError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    #[default]
    Sha256,
    #[serde(rename = "xxhash3")]
    XxHash3,
    Blake3,
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sha256 => write!(f, "sha256"),
            Self::XxHash3 => write!(f, "xxhash3"),
            Self::Blake3 => write!(f, "blake3"),
        }
    }
}

impl FromStr for HashAlgorithm {
    type Err = CourierError;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "sha256" => Ok(Self::Sha256),
            "xxhash3" | "xxh3" => Ok(Self::XxHash3),
            "blake3" => Ok(Self::Blake3),
            _ => Err(CourierError::Configuration(format!(
                "unsupported hash algorithm: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    Draft,
    Inventorying,
    Ready,
    Uploading,
    Paused,
    Interrupted,
    Finalizing,
    Verifying,
    Complete,
    Failed,
    Cancelled,
}

impl TransferStatus {
    pub fn can_transition_to(self, next: Self) -> bool {
        use TransferStatus::*;
        matches!(
            (self, next),
            (Draft, Inventorying)
                | (Inventorying, Ready)
                | (Inventorying, Failed)
                | (Ready, Uploading)
                | (Uploading, Paused)
                | (Uploading, Interrupted)
                | (Uploading, Finalizing)
                | (Uploading, Failed)
                | (Paused, Uploading)
                | (Interrupted, Uploading)
                | (Finalizing, Verifying)
                | (Finalizing, Failed)
                | (Verifying, Complete)
                | (Verifying, Failed)
                | (_, Cancelled)
        ) && !matches!(self, Complete | Cancelled)
    }
}

impl fmt::Display for TransferStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_value(self).unwrap().as_str().unwrap()
        )
    }
}

impl FromStr for TransferStatus {
    type Err = CourierError;
    fn from_str(value: &str) -> Result<Self> {
        Ok(serde_json::from_str(&format!("\"{value}\""))?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Pending,
    Ready,
    Uploading,
    Uploaded,
    SourceModified,
    Failed,
}

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_value(self).unwrap().as_str().unwrap()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: Uuid,
    pub server_transfer_id: Option<String>,
    pub project_id: Option<String>,
    pub source_root: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status: TransferStatus,
    pub file_count: u64,
    pub original_bytes: u64,
    pub manifest_version: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySessionRecord {
    pub base_url: String,
    pub expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
    pub projects_json: String,
}

impl Transfer {
    pub fn draft(source_root: PathBuf, project_id: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            server_transfer_id: None,
            project_id,
            source_root,
            created_at: now,
            updated_at: now,
            status: TransferStatus::Draft,
            file_count: 0,
            original_bytes: 0,
            manifest_version: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: Uuid,
    pub transfer_id: Uuid,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash_algorithm: HashAlgorithm,
    pub sha256: String,
    pub status: FileStatus,
    pub bytes_completed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartStatus {
    Pending,
    Uploading,
    Complete,
    Failed,
}

impl fmt::Display for PartStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_value(self).unwrap().as_str().unwrap()
        )
    }
}

impl FromStr for PartStatus {
    type Err = CourierError;

    fn from_str(value: &str) -> Result<Self> {
        Ok(serde_json::from_str(&format!("\"{value}\""))?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRecord {
    pub file_id: Uuid,
    pub part_number: u32,
    pub source_offset: u64,
    pub source_length: u64,
    pub transport_length: Option<u64>,
    pub checksum: Option<String>,
    pub etag: Option<String>,
    pub attempt_count: u32,
    pub status: PartStatus,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportObjectKind {
    File,
    Pack,
}

impl fmt::Display for TransportObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::Pack => write!(f, "pack"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportObjectRecord {
    pub id: Uuid,
    pub transfer_id: Uuid,
    pub kind: TransportObjectKind,
    pub compression: String,
    pub encoding_version: u8,
    pub original_bytes: u64,
    pub transport_bytes: Option<u64>,
    pub cache_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportMemberRecord {
    pub object_id: Uuid,
    pub file_id: Uuid,
    pub member_index: u32,
}
