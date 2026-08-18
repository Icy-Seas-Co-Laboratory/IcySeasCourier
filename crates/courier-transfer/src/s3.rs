use std::fmt::Debug;

use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::{
    Client,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
};
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;

use crate::{MultipartStore, RemotePart, StoreError, UploadSession};

#[derive(Debug, Clone)]
pub struct S3StoreConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint_url: Option<String>,
    pub force_path_style: bool,
}

impl S3StoreConfig {
    pub fn seaweedfs(bucket: impl Into<String>, endpoint_url: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            region: "us-east-1".into(),
            endpoint_url: Some(endpoint_url.into()),
            force_path_style: true,
        }
    }
}

#[derive(Clone)]
pub struct S3MultipartStore {
    client: Client,
    bucket: String,
}

impl S3MultipartStore {
    pub async fn from_config(config: S3StoreConfig) -> Self {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()));
        if let Some(endpoint) = &config.endpoint_url {
            loader = loader.endpoint_url(endpoint);
        }
        let shared = loader.load().await;
        let service = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(config.force_path_style)
            .build();
        Self {
            client: Client::from_conf(service),
            bucket: config.bucket,
        }
    }

    pub fn from_client(client: Client, bucket: impl Into<String>) -> Self {
        Self {
            client,
            bucket: bucket.into(),
        }
    }

    pub async fn ensure_bucket(&self) -> Result<(), StoreError> {
        match self.client.head_bucket().bucket(&self.bucket).send().await {
            Ok(_) => Ok(()),
            Err(_) => self
                .client
                .create_bucket()
                .bucket(&self.bucket)
                .send()
                .await
                .map(|_| ())
                .map_err(map_sdk_error),
        }
    }
}

#[async_trait]
impl MultipartStore for S3MultipartStore {
    async fn begin(&self, object_key: &str) -> Result<UploadSession, StoreError> {
        let output = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(object_key)
            .send()
            .await
            .map_err(map_sdk_error)?;
        let upload_id = output
            .upload_id()
            .ok_or_else(|| StoreError::Permanent("S3 omitted multipart upload ID".into()))?;
        Ok(UploadSession {
            object_key: object_key.into(),
            upload_id: upload_id.into(),
        })
    }

    async fn list_parts(&self, session: &UploadSession) -> Result<Vec<RemotePart>, StoreError> {
        let mut marker = None;
        let mut result = Vec::new();
        loop {
            let output = self
                .client
                .list_parts()
                .bucket(&self.bucket)
                .key(&session.object_key)
                .upload_id(&session.upload_id)
                .set_part_number_marker(marker)
                .send()
                .await
                .map_err(map_sdk_error)?;
            for part in output.parts() {
                let number = part
                    .part_number()
                    .and_then(|number| u32::try_from(number).ok())
                    .ok_or_else(|| {
                        StoreError::Permanent("S3 returned an invalid part number".into())
                    })?;
                let size = u64::try_from(part.size().unwrap_or_default()).map_err(|_| {
                    StoreError::Permanent("S3 returned a negative part size".into())
                })?;
                result.push(RemotePart {
                    part_number: number,
                    etag: part.e_tag().unwrap_or_default().into(),
                    size,
                });
            }
            if output.is_truncated() != Some(true) {
                break;
            }
            marker = output.next_part_number_marker().map(str::to_owned);
            if marker.is_none() {
                return Err(StoreError::Permanent(
                    "S3 truncated ListParts without a continuation marker".into(),
                ));
            }
        }
        Ok(result)
    }

    async fn upload_part(
        &self,
        session: &UploadSession,
        part_number: u32,
        bytes: Vec<u8>,
    ) -> Result<RemotePart, StoreError> {
        let size = bytes.len() as u64;
        let output = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(&session.object_key)
            .upload_id(&session.upload_id)
            .part_number(part_number as i32)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map_err(map_sdk_error)?;
        let etag = output
            .e_tag()
            .ok_or_else(|| StoreError::Permanent("S3 omitted uploaded part ETag".into()))?;
        Ok(RemotePart {
            part_number,
            etag: etag.into(),
            size,
        })
    }

    async fn complete(
        &self,
        session: &UploadSession,
        parts: &[RemotePart],
    ) -> Result<(), StoreError> {
        let completed = parts
            .iter()
            .map(|part| {
                CompletedPart::builder()
                    .part_number(part.part_number as i32)
                    .e_tag(&part.etag)
                    .build()
            })
            .collect();
        let upload = CompletedMultipartUpload::builder()
            .set_parts(Some(completed))
            .build();
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&session.object_key)
            .upload_id(&session.upload_id)
            .multipart_upload(upload)
            .send()
            .await
            .map(|_| ())
            .map_err(map_sdk_error)
    }

    async fn object_exists(&self, object_key: &str) -> Result<bool, StoreError> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(object_key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if matches!(service_code(&error), Some("NotFound" | "NoSuchKey")) => {
                Ok(false)
            }
            Err(error) => Err(map_sdk_error(error)),
        }
    }

    async fn abort(&self, session: &UploadSession) -> Result<(), StoreError> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(&session.object_key)
            .upload_id(&session.upload_id)
            .send()
            .await
            .map(|_| ())
            .map_err(map_sdk_error)
    }
}

fn service_code<E, R>(error: &SdkError<E, R>) -> Option<&str>
where
    E: ProvideErrorMetadata,
{
    error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
}

fn map_sdk_error<E, R>(error: SdkError<E, R>) -> StoreError
where
    E: ProvideErrorMetadata + Debug,
    R: Debug,
{
    if let Some(code) = service_code(&error) {
        return match code {
            "NoSuchUpload" => StoreError::UploadNotFound,
            "ExpiredToken" | "RequestExpired" => StoreError::AuthorizationExpired,
            "InternalError" | "RequestTimeout" | "ServiceUnavailable" | "SlowDown" => {
                StoreError::Transient(format!("S3 service error {code}"))
            }
            _ => StoreError::Permanent(format!("S3 service error {code}: {error:?}")),
        };
    }
    match error {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_) => {
            StoreError::Transient(format!("{error:?}"))
        }
        _ => StoreError::Permanent(format!("{error:?}")),
    }
}
