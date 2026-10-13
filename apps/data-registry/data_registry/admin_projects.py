"""Administrative read models; object bytes remain on the S3 data plane."""

import json
import uuid
from typing import Literal

from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import StreamingResponse
from pydantic import BaseModel
from sqlalchemy import String, case, cast, func, or_, select
from sqlalchemy.orm import Session

from .audit import record_event
from .dependencies import AdminActor, Database, Storage
from .models import AuditEvent, Project, Transfer, TransferFile, TransferObject, TransportPart
from .schemas import AdminTransferSummary, AuditEventResponse, ProjectResponse

router = APIRouter(prefix="/api/v1/admin", tags=["administration"])
READY = {"uploaded", "verified"}
LIVE = {"draft", "uploading", "finalizing", "verifying"}


class ProjectSummary(BaseModel):
    project: ProjectResponse
    total_transfers: int
    active_transfers: int
    completed_transfers: int
    failed_transfers: int
    file_count: int
    original_bytes: int
    uploaded_file_count: int
    uploaded_original_bytes: int
    verified_file_count: int
    verified_original_bytes: int
    recorded_transport_bytes: int
    live_transfers: list[AdminTransferSummary]


class BrowserEntry(BaseModel):
    kind: Literal["folder", "file"]
    name: str
    path: str
    transfer_id: str
    file_id: uuid.UUID | None = None
    original_size: int | None = None
    status: str | None = None
    available: bool = False
    verified: bool = False


class BrowserResponse(BaseModel):
    path: str
    entries: list[BrowserEntry]
    total: int
    offset: int
    limit: int


class FileDownload(BaseModel):
    transfer_id: str
    file_id: uuid.UUID
    object_id: uuid.UUID | None
    relative_path: str
    original_size: int
    digest: str
    hash_algorithm: str
    kind: str
    compression: str
    member_index: int | None
    transport_bytes: int | None
    pack_member_count: int
    pack_original_bytes: int
    verified: bool
    url: str
    expires_in_seconds: int


class TransferLogs(BaseModel):
    transfer_id: str
    scope: str = (
        "Recorded Registry lifecycle events and submitted Courier diagnostics; "
        "unsubmitted device logs and historical diagnostics are unavailable."
    )
    events: list[AuditEventResponse]
    total: int
    offset: int
    limit: int


def project_by_code(database: Session, code: str) -> Project:
    project = database.scalar(select(Project).where(Project.project_code == code))
    if project is None:
        raise HTTPException(404, "project not found")
    return project


def transfer_by_id(database: Session, transfer_id: str) -> Transfer:
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None:
        raise HTTPException(404, "transfer not found")
    return transfer


def file_available(item: TransferFile) -> bool:
    transport = item.transport_object
    return transport.status in READY if transport else item.status in READY


@router.get("/projects/{project_code}/summary", response_model=ProjectSummary)
def project_summary(project_code: str, database: Database, _: AdminActor) -> ProjectSummary:
    from .api import transfer_summary

    project = project_by_code(database, project_code)
    counts = database.execute(
        select(
            func.count(Transfer.id),
            func.sum(case((Transfer.status.in_(LIVE), 1), else_=0)),
            func.sum(case((Transfer.status == "complete", 1), else_=0)),
            func.sum(case((Transfer.status == "failed", 1), else_=0)),
            func.sum(Transfer.file_count),
            func.sum(Transfer.original_bytes),
        ).where(Transfer.project_id == project.id)
    ).one()
    available = or_(
        TransferObject.status.in_(READY),
        (TransferFile.transport_object_id.is_(None)) & TransferFile.status.in_(READY),
    )
    verified = TransferFile.status == "verified"
    files = database.execute(
        select(
            func.sum(case((available, 1), else_=0)),
            func.sum(case((available, TransferFile.original_size), else_=0)),
            func.sum(case((verified, 1), else_=0)),
            func.sum(case((verified, TransferFile.original_size), else_=0)),
        )
        .select_from(TransferFile)
        .join(Transfer)
        .outerjoin(TransferObject, TransferFile.transport_object_id == TransferObject.id)
        .where(Transfer.project_id == project.id)
    ).one()
    recorded = (
        database.scalar(
            select(func.sum(TransportPart.size))
            .join(TransferObject)
            .join(Transfer)
            .where(Transfer.project_id == project.id)
        )
        or 0
    )
    live = database.scalars(
        select(Transfer)
        .where(
            Transfer.project_id == project.id,
            Transfer.status.in_(LIVE),
        )
        .order_by(Transfer.created_at.desc())
    )
    return ProjectSummary(
        project=ProjectResponse.model_validate(project),
        total_transfers=counts[0],
        active_transfers=counts[1] or 0,
        completed_transfers=counts[2] or 0,
        failed_transfers=counts[3] or 0,
        file_count=counts[4] or 0,
        original_bytes=counts[5] or 0,
        uploaded_file_count=files[0] or 0,
        uploaded_original_bytes=files[1] or 0,
        verified_file_count=files[2] or 0,
        verified_original_bytes=files[3] or 0,
        recorded_transport_bytes=recorded,
        live_transfers=[transfer_summary(item) for item in live],
    )


@router.get("/projects/{project_code}/files", response_model=BrowserResponse)
def project_files(
    project_code: str,
    database: Database,
    _: AdminActor,
    transfer_id: str | None = None,
    path: str = Query(default="", max_length=4096),
    offset: int = Query(default=0, ge=0),
    limit: int = Query(default=200, ge=1, le=1000),
) -> BrowserResponse:
    project = project_by_code(database, project_code)
    if path and (
        "\\" in path or "\x00" in path or any(p in {"", ".", ".."} for p in path.split("/"))
    ):
        raise HTTPException(422, "path must be a normalized relative directory")
    query = (
        select(
            Transfer.public_id,
            TransferFile.id,
            TransferFile.relative_path,
            TransferFile.original_size,
            TransferFile.status,
            TransferObject.status.label("object_status"),
        )
        .select_from(TransferFile)
        .join(Transfer, TransferFile.transfer_id == Transfer.id)
        .outerjoin(TransferObject, TransferFile.transport_object_id == TransferObject.id)
        .where(Transfer.project_id == project.id)
    )
    if transfer_id:
        transfer = transfer_by_id(database, transfer_id)
        if transfer.project_id != project.id:
            raise HTTPException(404, "transfer not found in project")
        query = query.where(Transfer.public_id == transfer_id)
    prefix = path + "/" if path else ""
    if prefix:
        query = query.where(TransferFile.relative_path.startswith(prefix, autoescape=True))
    entries: dict[tuple[str, str], BrowserEntry] = {}
    for public_id, file_id, relative_path, size, status, object_status in database.execute(query):
        remainder = relative_path[len(prefix) :]
        name, separator, _tail = remainder.partition("/")
        entry_path = prefix + name
        key = (public_id, entry_path)
        if separator:
            entries[key] = BrowserEntry(
                kind="folder", name=name, path=entry_path, transfer_id=public_id
            )
        else:
            entries[key] = BrowserEntry(
                kind="file",
                name=name,
                path=relative_path,
                transfer_id=public_id,
                file_id=file_id,
                original_size=size,
                status=status,
                available=(object_status if object_status is not None else status) in READY,
                verified=status == "verified",
            )
    ordered = sorted(
        entries.values(), key=lambda item: (item.kind != "folder", item.name, item.transfer_id)
    )
    return BrowserResponse(
        path=path,
        entries=ordered[offset : offset + limit],
        total=len(ordered),
        offset=offset,
        limit=limit,
    )


@router.post("/transfers/{transfer_id}/files/{file_id}/download", response_model=FileDownload)
def download_file(
    transfer_id: str,
    file_id: uuid.UUID,
    database: Database,
    actor: AdminActor,
    storage: Storage,
) -> FileDownload:
    transfer = transfer_by_id(database, transfer_id)
    item = database.scalar(
        select(TransferFile).where(
            TransferFile.id == file_id,
            TransferFile.transfer_id == transfer.id,
        )
    )
    if item is None:
        raise HTTPException(404, "file not found in transfer")
    if not file_available(item):
        raise HTTPException(409, "file transport object is still uploading")
    obj = item.transport_object
    pack_member_count, pack_original_bytes = 1, item.original_size
    if obj is not None and obj.kind == "pack":
        pack_member_count, pack_original_bytes = database.execute(
            select(func.count(TransferFile.id), func.sum(TransferFile.original_size)).where(
                TransferFile.transfer_id == transfer.id,
                TransferFile.transport_object_id == obj.id,
            )
        ).one()
        if pack_original_bytes is None or pack_member_count < 1:
            raise HTTPException(409, "pack has no immutable manifest members")
    # Failed independent verification must never be advertised as a verified preview.
    verified = item.status == "verified" and item.verification_error is None
    try:
        transport_bytes = storage.object_size(item.object_key)
    except Exception as error:
        raise HTTPException(502, "object storage is unavailable for this file") from error
    result = FileDownload(
        transfer_id=transfer_id,
        file_id=item.id,
        object_id=obj.id if obj else None,
        relative_path=item.relative_path,
        original_size=item.original_size,
        digest=item.original_sha256,
        hash_algorithm=item.hash_algorithm,
        kind=obj.kind if obj else "file",
        compression=obj.compression if obj else item.compression,
        member_index=item.member_index,
        transport_bytes=transport_bytes,
        pack_member_count=pack_member_count,
        pack_original_bytes=pack_original_bytes,
        verified=verified,
        url=storage.authorize_download(item.object_key),
        expires_in_seconds=storage.url_lifetime,
    )
    record_event(
        database,
        actor=actor,
        action="transfer.admin_file_download_authorized",
        object_type="transfer",
        object_id=transfer_id,
        metadata={"file_id": str(item.id), "verified": verified},
    )
    database.commit()
    return result


def log_query(database: Session, transfer_id: str, search: str):
    transfer = transfer_by_id(database, transfer_id)
    # Include legacy file/object events as well as transfer-wide events.
    object_ids = select(cast(TransferObject.id, String)).where(
        TransferObject.transfer_id == transfer.id
    )
    file_ids = select(cast(TransferFile.id, String)).where(TransferFile.transfer_id == transfer.id)
    query = select(AuditEvent).where(
        or_(
            (AuditEvent.object_type == "transfer") & (AuditEvent.object_id == transfer_id),
            (AuditEvent.object_type == "transfer_object") & AuditEvent.object_id.in_(object_ids),
            (AuditEvent.object_type == "transfer_file") & AuditEvent.object_id.in_(file_ids),
        )
    )
    if search:
        pattern = "%" + search.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%"
        query = query.where(
            or_(
                AuditEvent.action.ilike(pattern, escape="\\"),
                AuditEvent.actor.ilike(pattern, escape="\\"),
                cast(AuditEvent.event_metadata, String).ilike(pattern, escape="\\"),
            )
        )
    return query


@router.get("/transfers/{transfer_id}/logs", response_model=TransferLogs)
def transfer_logs(
    transfer_id: str,
    database: Database,
    _: AdminActor,
    search: str = Query(default="", max_length=500),
    offset: int = Query(default=0, ge=0),
    limit: int = Query(default=200, ge=1, le=1000),
) -> TransferLogs:
    query = log_query(database, transfer_id, search)
    total = database.scalar(select(func.count()).select_from(query.subquery())) or 0
    events = database.scalars(
        query.order_by(AuditEvent.timestamp, AuditEvent.id).offset(offset).limit(limit)
    )
    return TransferLogs(
        transfer_id=transfer_id,
        events=[AuditEventResponse.model_validate(item) for item in events],
        total=total,
        offset=offset,
        limit=limit,
    )


@router.get("/transfers/{transfer_id}/logs/download")
def download_logs(
    transfer_id: str,
    database: Database,
    _: AdminActor,
    search: str = Query(default="", max_length=500),
) -> StreamingResponse:
    query = log_query(database, transfer_id, search).order_by(AuditEvent.timestamp, AuditEvent.id)

    def rows():
        yield (
            json.dumps(
                {
                    "type": "scope",
                    "transfer_id": transfer_id,
                    "scope": TransferLogs.model_fields["scope"].default,
                }
            )
            + "\n"
        )
        for item in database.scalars(query.execution_options(yield_per=500)):
            yield AuditEventResponse.model_validate(item).model_dump_json() + "\n"

    return StreamingResponse(
        rows(),
        media_type="application/x-ndjson",
        headers={
            "Content-Disposition": f'attachment; filename="{transfer_id}-registry-logs.ndjson"',
            "Cache-Control": "no-store",
        },
    )
