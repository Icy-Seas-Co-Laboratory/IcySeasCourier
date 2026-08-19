import uuid
from datetime import datetime
from typing import Any

from sqlalchemy import (
    JSON,
    BigInteger,
    Column,
    DateTime,
    ForeignKey,
    Integer,
    String,
    Table,
    Text,
    UniqueConstraint,
)
from sqlalchemy.orm import Mapped, mapped_column, relationship
from sqlalchemy.sql import func

from .db import Base

invitation_projects = Table(
    "invitation_projects",
    Base.metadata,
    Column(
        "invitation_id",
        ForeignKey("upload_invitations.id", ondelete="CASCADE"),
        primary_key=True,
    ),
    Column("project_id", ForeignKey("projects.id", ondelete="CASCADE"), primary_key=True),
)


class TimestampMixin:
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now(), onupdate=func.now()
    )


class User(TimestampMixin, Base):
    __tablename__ = "users"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    email: Mapped[str] = mapped_column(String(320), unique=True)
    display_name: Mapped[str] = mapped_column(String(200))
    active: Mapped[bool] = mapped_column(default=True)
    role: Mapped[str] = mapped_column(String(30), default="admin")


class Project(TimestampMixin, Base):
    __tablename__ = "projects"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    project_code: Mapped[str] = mapped_column(String(6), unique=True, index=True)
    name: Mapped[str] = mapped_column(String(300))
    description: Mapped[str | None] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(30), default="active")


class UploadInvitation(Base):
    __tablename__ = "upload_invitations"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    token_hash: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))
    maximum_transfer_bytes: Mapped[int | None] = mapped_column(BigInteger)
    maximum_uses: Mapped[int | None] = mapped_column(Integer)
    use_count: Mapped[int] = mapped_column(Integer, default=0)
    created_by: Mapped[str] = mapped_column(String(320))
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    projects: Mapped[list[Project]] = relationship(secondary=invitation_projects, lazy="selectin")


class CourierSession(Base):
    __tablename__ = "courier_sessions"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    token_hash: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    refresh_token_hash: Mapped[str | None] = mapped_column(String(64), unique=True, index=True)
    invitation_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("upload_invitations.id"))
    client_identifier: Mapped[str] = mapped_column(String(200))
    courier_version: Mapped[str] = mapped_column(String(40))
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))
    refresh_expires_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    invitation: Mapped[UploadInvitation] = relationship(lazy="joined")


class Transfer(Base):
    __tablename__ = "transfers"
    __table_args__ = (UniqueConstraint("client_session_id", "idempotency_key"),)
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    public_id: Mapped[str] = mapped_column(String(40), unique=True, index=True)
    project_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("projects.id"))
    invitation_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("upload_invitations.id"))
    client_session_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("courier_sessions.id"))
    idempotency_key: Mapped[str] = mapped_column(String(100))
    courier_version: Mapped[str] = mapped_column(String(40))
    manifest_version: Mapped[int] = mapped_column(Integer)
    source_name: Mapped[str] = mapped_column(String(512))
    file_count: Mapped[int]
    original_bytes: Mapped[int] = mapped_column(BigInteger)
    transport_bytes: Mapped[int | None] = mapped_column(BigInteger)
    status: Mapped[str] = mapped_column(String(30), default="draft")
    manifest: Mapped[dict[str, Any] | None] = mapped_column(JSON)
    manifest_sha256: Mapped[str | None] = mapped_column(String(64))
    manifest_submitted_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    verified_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    verification_started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    verification_attempt_count: Mapped[int] = mapped_column(Integer, default=0)
    verification_error: Mapped[str | None] = mapped_column(Text)
    hash_algorithm: Mapped[str] = mapped_column(String(20), default="sha256")
    project: Mapped[Project] = relationship(lazy="joined")
    files: Mapped[list["TransferFile"]] = relationship(
        back_populates="transfer", cascade="all, delete-orphan", lazy="selectin"
    )
    transport_objects: Mapped[list["TransferObject"]] = relationship(
        back_populates="transfer", cascade="all, delete-orphan", lazy="selectin"
    )


class TransferObject(Base):
    __tablename__ = "transfer_objects"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True)
    transfer_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("transfers.id", ondelete="CASCADE"))
    kind: Mapped[str] = mapped_column(String(20))
    compression: Mapped[str] = mapped_column(String(20))
    encoding_version: Mapped[int] = mapped_column(Integer)
    original_bytes: Mapped[int] = mapped_column(BigInteger)
    object_key: Mapped[str] = mapped_column(Text, unique=True)
    multipart_upload_id: Mapped[str | None] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(30), default="pending")
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    transfer: Mapped[Transfer] = relationship(back_populates="transport_objects")
    files: Mapped[list["TransferFile"]] = relationship(back_populates="transport_object")
    parts: Mapped[list["TransportPart"]] = relationship(
        back_populates="transport_object", cascade="all, delete-orphan", lazy="selectin"
    )


class TransferFile(Base):
    __tablename__ = "transfer_files"
    __table_args__ = (UniqueConstraint("transfer_id", "relative_path"),)
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    transfer_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("transfers.id", ondelete="CASCADE"))
    relative_path: Mapped[str] = mapped_column(Text)
    original_size: Mapped[int] = mapped_column(BigInteger)
    original_sha256: Mapped[str] = mapped_column(String(64))
    modified_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))
    compression: Mapped[str] = mapped_column(String(20))
    transport_encoding_version: Mapped[int] = mapped_column(Integer)
    object_key: Mapped[str] = mapped_column(Text)
    multipart_upload_id: Mapped[str | None] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(30), default="pending")
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    verified_size: Mapped[int | None] = mapped_column(BigInteger)
    verified_sha256: Mapped[str | None] = mapped_column(String(64))
    verified_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    verification_error: Mapped[str | None] = mapped_column(Text)
    hash_algorithm: Mapped[str] = mapped_column(String(20), default="sha256")
    transport_object_id: Mapped[uuid.UUID | None] = mapped_column(
        ForeignKey("transfer_objects.id", ondelete="CASCADE"), index=True
    )
    member_index: Mapped[int | None] = mapped_column(Integer)
    transfer: Mapped[Transfer] = relationship(back_populates="files")
    transport_object: Mapped[TransferObject | None] = relationship(back_populates="files")
    parts: Mapped[list["TransferPart"]] = relationship(
        back_populates="file", cascade="all, delete-orphan", lazy="selectin"
    )


class TransferPart(Base):
    __tablename__ = "transfer_parts"
    __table_args__ = (UniqueConstraint("file_id", "part_number"),)
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    file_id: Mapped[uuid.UUID] = mapped_column(ForeignKey("transfer_files.id", ondelete="CASCADE"))
    part_number: Mapped[int] = mapped_column(Integer)
    etag: Mapped[str] = mapped_column(Text)
    size: Mapped[int | None] = mapped_column(BigInteger)
    recorded_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )
    file: Mapped[TransferFile] = relationship(back_populates="parts")


class TransportPart(Base):
    __tablename__ = "transport_parts"
    __table_args__ = (UniqueConstraint("transport_object_id", "part_number"),)
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    transport_object_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("transfer_objects.id", ondelete="CASCADE")
    )
    part_number: Mapped[int] = mapped_column(Integer)
    etag: Mapped[str] = mapped_column(Text)
    size: Mapped[int | None] = mapped_column(BigInteger)
    recorded_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )
    transport_object: Mapped[TransferObject] = relationship(back_populates="parts")


class VerificationAttempt(Base):
    __tablename__ = "verification_attempts"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    transfer_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey("transfers.id", ondelete="CASCADE"), index=True
    )
    attempt_number: Mapped[int] = mapped_column(Integer)
    status: Mapped[str] = mapped_column(String(30))
    started_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    verified_file_count: Mapped[int] = mapped_column(Integer, default=0)
    verified_bytes: Mapped[int] = mapped_column(BigInteger, default=0)
    error: Mapped[str | None] = mapped_column(Text)


class AuditEvent(Base):
    __tablename__ = "audit_events"
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    timestamp: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    actor: Mapped[str] = mapped_column(String(320))
    action: Mapped[str] = mapped_column(String(100))
    object_type: Mapped[str] = mapped_column(String(80))
    object_id: Mapped[str] = mapped_column(String(80))
    event_metadata: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict)
