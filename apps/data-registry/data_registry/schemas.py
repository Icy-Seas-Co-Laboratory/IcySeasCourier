import uuid
from datetime import datetime
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator


class ORMModel(BaseModel):
    model_config = ConfigDict(from_attributes=True)


HashAlgorithm = Literal["sha256", "xxhash3", "blake3"]


class SystemConfigResponse(BaseModel):
    hash_algorithm: HashAlgorithm


class ProjectCreate(BaseModel):
    project_code: str = Field(pattern=r"^P[0-9]{5}$")
    name: str = Field(min_length=1, max_length=300)
    description: str | None = None


class ProjectResponse(ORMModel):
    id: uuid.UUID
    project_code: str
    name: str
    description: str | None
    status: str
    created_at: datetime


class InvitationCreate(BaseModel):
    project_codes: list[str] = Field(min_length=1)
    expires_at: datetime
    maximum_transfer_bytes: int | None = Field(default=None, ge=0)
    maximum_uses: int | None = Field(default=1, ge=1)
    created_by: str = Field(min_length=1, max_length=320)

    @field_validator("project_codes")
    @classmethod
    def unique_projects(cls, values: list[str]) -> list[str]:
        if len(values) != len(set(values)):
            raise ValueError("project codes must be unique")
        return values


class InvitationResponse(BaseModel):
    id: uuid.UUID
    invitation_code: str
    expires_at: datetime
    project_codes: list[str]
    maximum_uses: int | None


class AdminInvitationResponse(BaseModel):
    id: uuid.UUID
    expires_at: datetime
    project_codes: list[str]
    maximum_transfer_bytes: int | None
    maximum_uses: int | None
    use_count: int
    created_by: str
    created_at: datetime
    revoked_at: datetime | None


class AdminTransferSummary(BaseModel):
    transfer_id: str
    project_code: str
    source_name: str
    status: str
    file_count: int
    original_bytes: int
    courier_version: str
    created_at: datetime
    completed_at: datetime | None
    verified_at: datetime | None
    verification_attempt_count: int
    verification_error: str | None
    hash_algorithm: str


class AdminTransferDetail(AdminTransferSummary):
    manifest_sha256: str | None
    verification_started_at: datetime | None
    files: list["VerificationFileResponse"]


class AuditEventResponse(ORMModel):
    id: uuid.UUID
    timestamp: datetime
    actor: str
    action: str
    object_type: str
    object_id: str
    event_metadata: dict


class AdminOverviewResponse(BaseModel):
    projects: int
    active_invitations: int
    total_transfers: int
    active_transfers: int
    failed_transfers: int
    completed_transfers: int
    original_bytes: int
    hash_algorithm: str


class InvitationExchange(BaseModel):
    invitation_code: str = Field(min_length=8, max_length=40)
    client_identifier: str = Field(min_length=1, max_length=200)
    courier_version: str = Field(min_length=1, max_length=40)


class SessionResponse(BaseModel):
    access_token: str
    refresh_token: str
    token_type: str = "bearer"
    expires_at: datetime
    refresh_expires_at: datetime
    projects: list[ProjectResponse]


class SessionRefresh(BaseModel):
    refresh_token: str = Field(min_length=32, max_length=200)


class TransferCreate(BaseModel):
    project_code: str = Field(pattern=r"^P[0-9]{5}$")
    source_name: str = Field(min_length=1, max_length=512)
    file_count: int = Field(ge=0)
    original_bytes: int = Field(ge=0)
    manifest_version: int = Field(default=1, ge=1)
    courier_version: str = Field(min_length=1, max_length=40)
    idempotency_key: str = Field(min_length=8, max_length=100)
    hash_algorithm: HashAlgorithm = "sha256"


class TransferResponse(ORMModel):
    public_id: str
    project_id: uuid.UUID
    source_name: str
    file_count: int
    original_bytes: int
    manifest_version: int
    status: str
    created_at: datetime


class ManifestCourier(BaseModel):
    version: str = Field(min_length=1, max_length=40)
    platform: str = Field(min_length=1, max_length=100)
    transport_encoding_version: Literal[1]


class ManifestSource(BaseModel):
    name: str = Field(min_length=1, max_length=512)


class ManifestSummary(BaseModel):
    file_count: int = Field(ge=0)
    original_bytes: int = Field(ge=0)


class ManifestTransport(BaseModel):
    compression: Literal["none", "zstd"]
    encoding_version: Literal[1]


class ManifestFile(BaseModel):
    path: str = Field(min_length=1, max_length=4096)
    size: int = Field(ge=0)
    mtime: datetime
    sha256: str | None = Field(default=None, pattern=r"^[a-f0-9]{64}$")
    digest: "ManifestDigest | None" = None
    transport: ManifestTransport

    @field_validator("path")
    @classmethod
    def safe_relative_path(cls, value: str) -> str:
        components = value.split("/")
        if (
            value.startswith("/")
            or "\\" in value
            or "\x00" in value
            or any(component in {"", ".", ".."} for component in components)
            or (len(value) >= 2 and value[0].isalpha() and value[1] == ":")
        ):
            raise ValueError("path must be a normalized relative path")
        return value


class ManifestDigest(BaseModel):
    algorithm: HashAlgorithm
    value: str = Field(pattern=r"^[a-f0-9]+$")

    @model_validator(mode="after")
    def validate_length(self) -> "ManifestDigest":
        expected = 32 if self.algorithm == "xxhash3" else 64
        if len(self.value) != expected:
            raise ValueError(f"{self.algorithm} digest must contain {expected} hex characters")
        return self


class TransferManifest(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    schema_name: Literal["icy-seas-transfer-manifest"] = Field(alias="schema")
    version: Literal[1, 2]
    transfer_id: str = Field(min_length=1, max_length=80)
    project: str = Field(pattern=r"^P[0-9]{5}$")
    created_at: datetime
    courier: ManifestCourier
    source: ManifestSource
    summary: ManifestSummary
    files: list[ManifestFile]

    @model_validator(mode="after")
    def validate_summary(self) -> "TransferManifest":
        if len(self.files) != self.summary.file_count:
            raise ValueError("summary file_count does not match files")
        if sum(file.size for file in self.files) != self.summary.original_bytes:
            raise ValueError("summary original_bytes does not match files")
        paths = [file.path for file in self.files]
        if len(paths) != len(set(paths)):
            raise ValueError("file paths must be unique")
        for file in self.files:
            if self.version == 1 and (file.sha256 is None or file.digest is not None):
                raise ValueError("manifest v1 files require sha256")
            if self.version == 2 and (file.digest is None or file.sha256 is not None):
                raise ValueError("manifest v2 files require digest")
        return self


class TransferFileResponse(ORMModel):
    id: uuid.UUID
    relative_path: str
    original_size: int
    original_sha256: str
    object_key: str
    status: str


class ManifestResponse(BaseModel):
    transfer_id: str
    manifest_sha256: str
    submitted_at: datetime
    files: list[TransferFileResponse]


class MultipartResponse(BaseModel):
    file_id: uuid.UUID
    upload_id: str
    status: str


class UploadedPart(BaseModel):
    part_number: int
    etag: str
    size: int | None = None


class PartsResponse(BaseModel):
    file_id: uuid.UUID
    parts: list[UploadedPart]


class ObjectStatusResponse(BaseModel):
    exists: bool


class PartAuthorizationResponse(BaseModel):
    file_id: uuid.UUID
    part_number: int
    method: Literal["PUT"] = "PUT"
    url: str
    expires_in_seconds: int


class CompleteMultipartRequest(BaseModel):
    parts: list[UploadedPart] = Field(min_length=1, max_length=10_000)

    @model_validator(mode="after")
    def validate_parts(self) -> "CompleteMultipartRequest":
        numbers = [part.part_number for part in self.parts]
        if any(number < 1 or number > 10_000 for number in numbers):
            raise ValueError("part numbers must be between 1 and 10000")
        if numbers != sorted(numbers) or len(numbers) != len(set(numbers)):
            raise ValueError("parts must be unique and ordered")
        return self


class CompleteMultipartResponse(BaseModel):
    file_id: uuid.UUID
    status: str
    etag: str


class FinalizeTransferResponse(BaseModel):
    transfer_id: str
    status: str


class VerificationFileResponse(ORMModel):
    id: uuid.UUID
    relative_path: str
    status: str
    original_size: int
    original_sha256: str
    verified_size: int | None
    verified_sha256: str | None
    verified_at: datetime | None
    verification_error: str | None
    hash_algorithm: str


class TransferStatusResponse(BaseModel):
    transfer_id: str
    status: str
    manifest_sha256: str | None
    completed_at: datetime | None
    verification_started_at: datetime | None
    verified_at: datetime | None
    verification_attempt_count: int
    verification_error: str | None
    files: list[VerificationFileResponse]
    hash_algorithm: str


class HealthResponse(BaseModel):
    status: str
