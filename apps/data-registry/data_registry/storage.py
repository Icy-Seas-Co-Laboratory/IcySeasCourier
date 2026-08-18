from functools import lru_cache
from typing import Any

import boto3
from botocore.client import Config
from botocore.exceptions import ClientError

from .config import Settings, get_settings


class ObjectStorage:
    """S3 multipart operations. Dataset bytes never pass through this service."""

    def __init__(self, settings: Settings) -> None:
        common: dict[str, Any] = {
            "aws_access_key_id": settings.s3_access_key_id.get_secret_value(),
            "aws_secret_access_key": settings.s3_secret_access_key.get_secret_value(),
            "region_name": settings.s3_region,
            "config": Config(signature_version="s3v4", s3={"addressing_style": "path"}),
        }
        self.bucket = settings.s3_bucket
        self.url_lifetime = settings.upload_url_lifetime_seconds
        self.internal = boto3.client(
            "s3", endpoint_url=settings.s3_internal_endpoint_url, **common
        )
        self.public = boto3.client("s3", endpoint_url=settings.s3_public_endpoint_url, **common)

    def ensure_bucket(self) -> None:
        try:
            self.internal.head_bucket(Bucket=self.bucket)
        except self.internal.exceptions.ClientError:
            self.internal.create_bucket(Bucket=self.bucket)

    def create_multipart(self, object_key: str) -> str:
        self.ensure_bucket()
        result = self.internal.create_multipart_upload(Bucket=self.bucket, Key=object_key)
        return str(result["UploadId"])

    def list_parts(self, object_key: str, upload_id: str) -> list[dict[str, Any]]:
        result = self.internal.list_parts(
            Bucket=self.bucket, Key=object_key, UploadId=upload_id
        )
        return list(result.get("Parts", []))

    def authorize_part(self, object_key: str, upload_id: str, part_number: int) -> str:
        return self.public.generate_presigned_url(
            "upload_part",
            Params={
                "Bucket": self.bucket,
                "Key": object_key,
                "UploadId": upload_id,
                "PartNumber": part_number,
            },
            ExpiresIn=self.url_lifetime,
            HttpMethod="PUT",
        )

    def complete_multipart(
        self, object_key: str, upload_id: str, parts: list[dict[str, Any]]
    ) -> str:
        result = self.internal.complete_multipart_upload(
            Bucket=self.bucket,
            Key=object_key,
            UploadId=upload_id,
            MultipartUpload={"Parts": parts},
        )
        return str(result.get("ETag", ""))

    def open_object(self, object_key: str):
        result = self.internal.get_object(Bucket=self.bucket, Key=object_key)
        return result["Body"]

    def object_exists(self, object_key: str) -> bool:
        try:
            self.internal.head_object(Bucket=self.bucket, Key=object_key)
            return True
        except ClientError as error:
            if error.response.get("ResponseMetadata", {}).get("HTTPStatusCode") == 404:
                return False
            raise


@lru_cache
def get_object_storage() -> ObjectStorage:
    return ObjectStorage(get_settings())
