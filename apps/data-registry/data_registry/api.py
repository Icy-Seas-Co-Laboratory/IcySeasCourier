import hashlib
import json
import shlex
import uuid
from datetime import UTC, datetime, timedelta
from urllib.parse import quote, urlencode

from fastapi import APIRouter, HTTPException, Response, status
from sqlalchemy import func, select, text
from sqlalchemy.exc import IntegrityError

from .audit import record_event
from .db import engine
from .dependencies import (
    AdminActor,
    AdminSessionToken,
    Configuration,
    CourierIdentity,
    Database,
    Storage,
)
from .models import (
    AdminSecurity,
    AuditEvent,
    CourierSession,
    Project,
    Transfer,
    TransferFile,
    TransferObject,
    TransportPart,
    UploadInvitation,
)
from .schemas import (
    AdminAuthenticationRequest,
    AdminAuthenticationStatus,
    AdminDownloadObject,
    AdminDownloadPlan,
    AdminInvitationResponse,
    AdminOverviewResponse,
    AdminSessionResponse,
    AdminSetupConfirm,
    AdminSetupResponse,
    AdminSetupStart,
    AdminTransferDetail,
    AdminTransferSummary,
    AuditEventResponse,
    CompleteMultipartRequest,
    CompleteMultipartResponse,
    DownloadDatasetSummary,
    DownloadObject,
    DownloadPlan,
    FinalizeTransferResponse,
    HealthResponse,
    InvitationCreate,
    InvitationExchange,
    InvitationResponse,
    ManifestResponse,
    MultipartResponse,
    ObjectStatusResponse,
    PartAuthorizationResponse,
    PartsResponse,
    ProjectCreate,
    ProjectResponse,
    SessionAuthorization,
    SessionRefresh,
    SessionResponse,
    SystemConfigResponse,
    TransferCreate,
    TransferManifest,
    TransferResponse,
    TransferStatusResponse,
    UploadedPart,
    VerificationFileResponse,
)
from .security import (
    decrypt_totp_secret,
    encrypt_totp_secret,
    generate_admin_session_token,
    generate_invitation_code,
    generate_refresh_token,
    generate_session_token,
    generate_totp_secret,
    hash_token,
    matching_totp_step,
    secrets_equal,
)

router = APIRouter()
admin = APIRouter(prefix="/api/v1/admin", tags=["administration"])
client = APIRouter(prefix="/api/v1", tags=["courier"])


def owned_transfer(database: Database, identity: CourierIdentity, transfer_id: str) -> Transfer:
    if identity.invitation.purpose != "upload":
        raise HTTPException(status_code=403, detail="invitation is read-only")
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None:
        raise HTTPException(status_code=404, detail="transfer not found")
    if transfer.client_session_id != identity.id:
        raise HTTPException(status_code=403, detail="transfer belongs to another session")
    return transfer


def owned_file(
    database: Database,
    identity: CourierIdentity,
    transfer_id: str,
    file_id: uuid.UUID,
) -> tuple[Transfer, TransferFile]:
    transfer = owned_transfer(database, identity, transfer_id)
    transfer_file = database.scalar(
        select(TransferFile).where(
            TransferFile.id == file_id, TransferFile.transfer_id == transfer.id
        )
    )
    if transfer_file is None:
        raise HTTPException(status_code=404, detail="transfer file not found")
    return transfer, transfer_file


def owned_object(
    database: Database,
    identity: CourierIdentity,
    transfer_id: str,
    object_id: uuid.UUID,
) -> tuple[Transfer, TransferObject]:
    transfer = owned_transfer(database, identity, transfer_id)
    transport_object = database.scalar(
        select(TransferObject).where(
            TransferObject.id == object_id, TransferObject.transfer_id == transfer.id
        )
    )
    if transport_object is None:
        raise HTTPException(status_code=404, detail="transport object not found")
    return transfer, transport_object


@router.get("/health", response_model=HealthResponse)
def health() -> HealthResponse:
    return HealthResponse(status="ok")


@router.get("/ready", response_model=HealthResponse)
def ready() -> HealthResponse:
    try:
        with engine.connect() as connection:
            connection.execute(text("SELECT 1"))
    except Exception as error:
        raise HTTPException(status_code=503, detail="database unavailable") from error
    return HealthResponse(status="ready")


@client.get("/system/config", response_model=SystemConfigResponse)
def system_config(settings: Configuration) -> SystemConfigResponse:
    return SystemConfigResponse(hash_algorithm=settings.hash_algorithm)


def _admin_security(database: Database, *, for_update: bool = False) -> AdminSecurity | None:
    query = select(AdminSecurity).where(AdminSecurity.id == 1)
    return database.scalar(query.with_for_update() if for_update else query)


def _aware(value: datetime) -> datetime:
    return value if value.tzinfo is not None else value.replace(tzinfo=UTC)


def _check_admin_key(value: str, settings: Configuration) -> None:
    if not secrets_equal(value, settings.admin_api_key.get_secret_value()):
        raise HTTPException(status_code=401, detail="invalid administrator credentials")


def _admin_session(settings: Configuration) -> AdminSessionResponse:
    token, expires_at = generate_admin_session_token(settings)
    return AdminSessionResponse(access_token=token, expires_at=expires_at)


@admin.get("/authentication/status", response_model=AdminAuthenticationStatus)
def admin_authentication_status(database: Database) -> AdminAuthenticationStatus:
    security = _admin_security(database)
    return AdminAuthenticationStatus(
        configured=security is not None and security.configured_at is not None
    )


@admin.post("/authentication/setup", response_model=AdminSetupResponse)
def start_admin_totp_setup(
    payload: AdminSetupStart, database: Database, settings: Configuration
) -> AdminSetupResponse:
    _check_admin_key(payload.admin_key, settings)
    security = _admin_security(database, for_update=True)
    if security is not None and security.configured_at is not None:
        raise HTTPException(status_code=409, detail="administrator TOTP is already configured")

    now = datetime.now(UTC)
    expires_at = now + timedelta(seconds=settings.admin_totp_setup_lifetime_seconds)
    secret = generate_totp_secret()
    if security is None:
        security = AdminSecurity(id=1, encrypted_totp_secret="")
        database.add(security)
    security.encrypted_totp_secret = encrypt_totp_secret(secret, settings)
    security.setup_expires_at = expires_at
    security.last_used_totp_step = None
    database.commit()

    issuer = "Icy Seas Courier"
    label = quote(f"{issuer}:admin")
    provisioning_uri = f"otpauth://totp/{label}?{urlencode({'secret': secret, 'issuer': issuer})}"
    return AdminSetupResponse(
        secret=secret,
        provisioning_uri=provisioning_uri,
        expires_at=expires_at,
    )


@admin.post("/authentication/setup/confirm", response_model=AdminSessionResponse)
def confirm_admin_totp_setup(
    payload: AdminSetupConfirm, database: Database, settings: Configuration
) -> AdminSessionResponse:
    _check_admin_key(payload.admin_key, settings)
    security = _admin_security(database, for_update=True)
    now = datetime.now(UTC)
    if (
        security is None
        or security.configured_at is not None
        or security.setup_expires_at is None
        or _aware(security.setup_expires_at) <= now
    ):
        raise HTTPException(status_code=409, detail="administrator TOTP setup is unavailable")
    secret = decrypt_totp_secret(security.encrypted_totp_secret, settings)
    step = matching_totp_step(secret, payload.totp_code, timestamp=now.timestamp())
    if step is None:
        raise HTTPException(status_code=401, detail="invalid administrator credentials")
    security.configured_at = now
    security.setup_expires_at = None
    security.last_used_totp_step = step
    record_event(
        database,
        actor="admin",
        action="admin.totp.configured",
        object_type="admin_security",
        object_id="primary",
    )
    database.commit()
    return _admin_session(settings)


@admin.post("/authentication/session", response_model=AdminSessionResponse)
def create_admin_session(
    payload: AdminAuthenticationRequest, database: Database, settings: Configuration
) -> AdminSessionResponse:
    _check_admin_key(payload.admin_key, settings)
    security = _admin_security(database, for_update=True)
    if security is None or security.configured_at is None:
        raise HTTPException(status_code=409, detail="administrator TOTP setup is required")
    now = datetime.now(UTC)
    secret = decrypt_totp_secret(security.encrypted_totp_secret, settings)
    step = matching_totp_step(secret, payload.totp_code, timestamp=now.timestamp())
    if step is None or (
        security.last_used_totp_step is not None and step <= security.last_used_totp_step
    ):
        raise HTTPException(status_code=401, detail="invalid or already used one-time code")
    security.last_used_totp_step = step
    record_event(
        database,
        actor="admin",
        action="admin.session.created",
        object_type="admin_security",
        object_id="primary",
    )
    database.commit()
    return _admin_session(settings)


@admin.post("/authentication/renew", response_model=AdminSessionResponse)
def renew_admin_session(_: AdminSessionToken, settings: Configuration) -> AdminSessionResponse:
    return _admin_session(settings)


@admin.post("/projects", response_model=ProjectResponse, status_code=status.HTTP_201_CREATED)
def create_project(payload: ProjectCreate, database: Database, actor: AdminActor) -> Project:
    project = Project(**payload.model_dump())
    database.add(project)
    record_event(
        database,
        actor=actor,
        action="project.created",
        object_type="project",
        object_id=payload.project_code,
    )
    try:
        database.commit()
    except IntegrityError as error:
        database.rollback()
        raise HTTPException(status_code=409, detail="project code already exists") from error
    database.refresh(project)
    return project


@admin.get("/projects", response_model=list[ProjectResponse])
def list_projects(database: Database, _: AdminActor) -> list[Project]:
    return list(database.scalars(select(Project).order_by(Project.project_code)))


@admin.get("/overview", response_model=AdminOverviewResponse)
def admin_overview(
    database: Database, _: AdminActor, settings: Configuration
) -> AdminOverviewResponse:
    now = datetime.now(UTC)
    statuses = dict(
        database.execute(select(Transfer.status, func.count()).group_by(Transfer.status)).all()
    )
    return AdminOverviewResponse(
        projects=database.scalar(select(func.count()).select_from(Project)) or 0,
        active_invitations=database.scalar(
            select(func.count())
            .select_from(UploadInvitation)
            .where(UploadInvitation.revoked_at.is_(None), UploadInvitation.expires_at > now)
        )
        or 0,
        total_transfers=sum(statuses.values()),
        active_transfers=sum(
            statuses.get(value, 0) for value in ("draft", "uploading", "finalizing", "verifying")
        ),
        failed_transfers=statuses.get("failed", 0),
        completed_transfers=statuses.get("complete", 0),
        original_bytes=database.scalar(select(func.coalesce(func.sum(Transfer.original_bytes), 0)))
        or 0,
        transport_bytes=database.scalar(select(func.coalesce(func.sum(TransportPart.size), 0)))
        or 0,
        hash_algorithm=settings.hash_algorithm,
    )


@admin.get("/invitations", response_model=list[AdminInvitationResponse])
def list_invitations(database: Database, _: AdminActor) -> list[AdminInvitationResponse]:
    invitations = database.scalars(
        select(UploadInvitation).order_by(UploadInvitation.created_at.desc())
    ).all()
    return [
        AdminInvitationResponse(
            id=item.id,
            expires_at=item.expires_at,
            project_codes=sorted(project.project_code for project in item.projects),
            maximum_transfer_bytes=item.maximum_transfer_bytes,
            maximum_uses=item.maximum_uses,
            use_count=item.use_count,
            created_by=item.created_by,
            created_at=item.created_at,
            revoked_at=item.revoked_at,
            purpose=item.purpose,
        )
        for item in invitations
    ]


def known_transport_bytes(transfer: Transfer) -> int | None:
    if transfer.transport_bytes is not None:
        return transfer.transport_bytes
    if transfer.manifest_sha256 is None:
        return None
    if not transfer.transport_objects:
        return 0
    if any(item.status not in {"uploaded", "verified"} for item in transfer.transport_objects):
        return None
    sizes = [part.size for item in transfer.transport_objects for part in item.parts]
    if any(size is None for size in sizes):
        return None
    return sum(size for size in sizes if size is not None)


def transfer_summary(transfer: Transfer) -> AdminTransferSummary:
    return AdminTransferSummary(
        transfer_id=transfer.public_id,
        project_code=transfer.project.project_code,
        source_name=transfer.source_name,
        status=transfer.status,
        file_count=transfer.file_count,
        original_bytes=transfer.original_bytes,
        transport_bytes=known_transport_bytes(transfer),
        courier_version=transfer.courier_version,
        created_at=transfer.created_at,
        completed_at=transfer.completed_at,
        verified_at=transfer.verified_at,
        verification_attempt_count=transfer.verification_attempt_count,
        verification_error=transfer.verification_error,
        hash_algorithm=transfer.hash_algorithm,
    )


@admin.get("/transfers", response_model=list[AdminTransferSummary])
def list_admin_transfers(
    database: Database,
    _: AdminActor,
    project_code: str | None = None,
    transfer_status: str | None = None,
    limit: int = 100,
) -> list[AdminTransferSummary]:
    limit = min(max(limit, 1), 500)
    query = select(Transfer).order_by(Transfer.created_at.desc()).limit(limit)
    if project_code:
        query = query.join(Transfer.project).where(Project.project_code == project_code)
    if transfer_status:
        query = query.where(Transfer.status == transfer_status)
    return [transfer_summary(item) for item in database.scalars(query).unique()]


@admin.get("/transfers/{transfer_id}", response_model=AdminTransferDetail)
def admin_transfer_detail(
    transfer_id: str, database: Database, _: AdminActor
) -> AdminTransferDetail:
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None:
        raise HTTPException(status_code=404, detail="transfer not found")
    return AdminTransferDetail(
        **transfer_summary(transfer).model_dump(),
        manifest_sha256=transfer.manifest_sha256,
        verification_started_at=transfer.verification_started_at,
        files=[VerificationFileResponse.model_validate(item) for item in transfer.files],
    )


@admin.post("/transfers/{transfer_id}/downloads", response_model=AdminDownloadPlan)
def create_admin_download_plan(
    transfer_id: str,
    database: Database,
    actor: AdminActor,
    storage: Storage,
) -> AdminDownloadPlan:
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None:
        raise HTTPException(status_code=404, detail="transfer not found")
    if transfer.status != "complete":
        raise HTTPException(status_code=409, detail="only verified transfers can be downloaded")

    directory = f"{transfer.public_id}-transport"
    objects = []
    commands = [f"mkdir -p {shlex.quote(directory)}"]
    for item in sorted(transfer.transport_objects, key=lambda value: str(value.id)):
        extension = "iscpack.zst" if item.kind == "pack" else "payload"
        filename = f"{item.id}.{extension}"
        url = storage.authorize_download(item.object_key)
        transport_bytes = (
            sum(part.size for part in item.parts if part.size is not None)
            if all(part.size is not None for part in item.parts)
            else None
        )
        objects.append(
            AdminDownloadObject(
                object_id=item.id,
                kind=item.kind,
                compression=item.compression,
                original_bytes=item.original_bytes,
                transport_bytes=transport_bytes,
                filename=filename,
                url=url,
            )
        )
        destination = f"{directory}/{filename}"
        commands.append(
            f"curl --fail --location --output {shlex.quote(destination)} {shlex.quote(url)}"
        )
    record_event(
        database,
        actor=actor,
        action="transfer.download_urls_issued",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"object_count": len(objects), "expires_in_seconds": storage.url_lifetime},
    )
    database.commit()
    return AdminDownloadPlan(
        transfer_id=transfer.public_id,
        expires_in_seconds=storage.url_lifetime,
        contains_packs=any(item.kind == "pack" for item in transfer.transport_objects),
        server_retrieve_command=(
            "docker compose exec data-registry python -m data_registry.retrieve "
            f"{shlex.quote(transfer.public_id)} "
            f"{shlex.quote(f'/exports/{transfer.public_id}')}"
        ),
        shell_command="\n".join(commands),
        objects=objects,
    )


@admin.post("/transfers/{transfer_id}/retry", response_model=AdminTransferSummary)
def retry_failed_transfer(
    transfer_id: str, database: Database, actor: AdminActor
) -> AdminTransferSummary:
    transfer = database.scalar(
        select(Transfer).where(Transfer.public_id == transfer_id).with_for_update(of=Transfer)
    )
    if transfer is None:
        raise HTTPException(status_code=404, detail="transfer not found")
    if transfer.status != "failed":
        raise HTTPException(status_code=409, detail="only failed transfers can be retried")
    files_not_ready = any(item.status not in {"uploaded", "verified"} for item in transfer.files)
    if not transfer.files or files_not_ready:
        raise HTTPException(
            status_code=409, detail="transfer objects are not ready for verification"
        )
    transfer.status = "finalizing"
    transfer.verification_error = None
    transfer.verification_started_at = None
    for item in transfer.files:
        item.verification_error = None
    record_event(
        database,
        actor=actor,
        action="transfer.verification_retry_requested",
        object_type="transfer",
        object_id=transfer.public_id,
    )
    database.commit()
    database.refresh(transfer)
    return transfer_summary(transfer)


@admin.delete("/transfers/{transfer_id}", status_code=status.HTTP_204_NO_CONTENT)
def delete_transfer(
    transfer_id: str,
    database: Database,
    storage: Storage,
    actor: AdminActor,
) -> Response:
    """Remove a transfer and its uploaded or in-progress S3 objects."""
    transfer = database.scalar(
        select(Transfer).where(Transfer.public_id == transfer_id).with_for_update(of=Transfer)
    )
    if transfer is None:
        raise HTTPException(status_code=404, detail="transfer not found")

    object_keys = {item.object_key for item in transfer.transport_objects}
    object_keys.update(item.object_key for item in transfer.files)
    for item in transfer.transport_objects:
        if item.multipart_upload_id is not None and item.status not in {"uploaded", "verified"}:
            storage.abort_multipart(item.object_key, item.multipart_upload_id)
        storage.delete_object(item.object_key)

    action = (
        "transfer.completed_deleted"
        if transfer.status == "complete"
        else "transfer.incomplete_purged"
    )
    record_event(
        database,
        actor=actor,
        action=action,
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"status": transfer.status, "object_count": len(object_keys)},
    )
    database.delete(transfer)
    database.commit()
    return Response(status_code=status.HTTP_204_NO_CONTENT)


@admin.get("/audit-events", response_model=list[AuditEventResponse])
def list_audit_events(database: Database, _: AdminActor, limit: int = 100) -> list[AuditEvent]:
    limit = min(max(limit, 1), 500)
    return list(
        database.scalars(select(AuditEvent).order_by(AuditEvent.timestamp.desc()).limit(limit))
    )


@admin.post("/invitations", response_model=InvitationResponse, status_code=status.HTTP_201_CREATED)
def create_invitation(
    payload: InvitationCreate,
    database: Database,
    settings: Configuration,
    actor: AdminActor,
) -> InvitationResponse:
    now = datetime.now(UTC)
    if payload.expires_at <= now:
        raise HTTPException(status_code=422, detail="expiration must be in the future")
    projects = list(
        database.scalars(select(Project).where(Project.project_code.in_(payload.project_codes)))
    )
    if len(projects) != len(payload.project_codes):
        found = {project.project_code for project in projects}
        missing = sorted(set(payload.project_codes) - found)
        raise HTTPException(status_code=404, detail={"missing_projects": missing})
    code = generate_invitation_code()
    invitation = UploadInvitation(
        token_hash=hash_token(code, settings),
        purpose=payload.purpose,
        expires_at=payload.expires_at,
        maximum_transfer_bytes=payload.maximum_transfer_bytes,
        maximum_uses=payload.maximum_uses,
        created_by=payload.created_by,
        projects=projects,
    )
    database.add(invitation)
    database.flush()
    record_event(
        database,
        actor=actor,
        action="invitation.created",
        object_type="upload_invitation",
        object_id=str(invitation.id),
        metadata={"project_codes": payload.project_codes, "purpose": payload.purpose},
    )
    database.commit()
    return InvitationResponse(
        id=invitation.id,
        invitation_code=code,
        expires_at=invitation.expires_at,
        project_codes=sorted(project.project_code for project in projects),
        maximum_uses=invitation.maximum_uses,
        purpose=invitation.purpose,
    )


@admin.delete("/invitations/{invitation_id}", status_code=status.HTTP_204_NO_CONTENT)
def revoke_invitation(
    invitation_id: uuid.UUID,
    database: Database,
    actor: AdminActor,
) -> Response:
    invitation = database.get(UploadInvitation, invitation_id)
    if invitation is None:
        raise HTTPException(status_code=404, detail="invitation not found")
    if invitation.revoked_at is None:
        invitation.revoked_at = datetime.now(UTC)
        record_event(
            database,
            actor=actor,
            action="invitation.revoked",
            object_type="upload_invitation",
            object_id=str(invitation.id),
        )
        database.commit()
    return Response(status_code=status.HTTP_204_NO_CONTENT)


@client.post("/auth/invitations/exchange", response_model=SessionResponse)
def exchange_invitation(
    payload: InvitationExchange, database: Database, settings: Configuration
) -> SessionResponse:
    invitation = database.scalar(
        select(UploadInvitation)
        .where(UploadInvitation.token_hash == hash_token(payload.invitation_code, settings))
        .with_for_update()
    )
    now = datetime.now(UTC)
    expires_at = invitation.expires_at if invitation is not None else now
    if expires_at.tzinfo is None:
        expires_at = expires_at.replace(tzinfo=UTC)
    if invitation is None or invitation.revoked_at is not None or expires_at <= now:
        raise HTTPException(status_code=401, detail="invitation expired or invalid")
    if invitation.maximum_uses is not None and invitation.use_count >= invitation.maximum_uses:
        raise HTTPException(status_code=401, detail="invitation use limit reached")
    raw_token = generate_session_token()
    raw_refresh_token = generate_refresh_token()
    refresh_expires_at = now + timedelta(seconds=settings.refresh_lifetime_seconds)
    courier_session = CourierSession(
        token_hash=hash_token(raw_token, settings),
        refresh_token_hash=hash_token(raw_refresh_token, settings),
        invitation_id=invitation.id,
        client_identifier=payload.client_identifier,
        courier_version=payload.courier_version,
        expires_at=now + timedelta(seconds=settings.session_lifetime_seconds),
        refresh_expires_at=refresh_expires_at,
    )
    invitation.use_count += 1
    database.add(courier_session)
    record_event(
        database,
        actor=f"courier:{payload.client_identifier}",
        action="invitation.exchanged",
        object_type="upload_invitation",
        object_id=str(invitation.id),
    )
    database.commit()
    return SessionResponse(
        access_token=raw_token,
        refresh_token=raw_refresh_token,
        expires_at=courier_session.expires_at,
        refresh_expires_at=refresh_expires_at,
        projects=[ProjectResponse.model_validate(project) for project in invitation.projects],
        purpose=invitation.purpose,
    )


@client.post("/auth/sessions/refresh", response_model=SessionResponse)
def refresh_session(
    payload: SessionRefresh, database: Database, settings: Configuration
) -> SessionResponse:
    courier_session = database.scalar(
        select(CourierSession)
        .where(CourierSession.refresh_token_hash == hash_token(payload.refresh_token, settings))
        .with_for_update(of=CourierSession)
    )
    now = datetime.now(UTC)
    refresh_expires_at = courier_session.refresh_expires_at if courier_session else now
    if refresh_expires_at is not None and refresh_expires_at.tzinfo is None:
        refresh_expires_at = refresh_expires_at.replace(tzinfo=UTC)
    if (
        courier_session is None
        or courier_session.revoked_at is not None
        or courier_session.invitation.revoked_at is not None
        or refresh_expires_at is None
        or refresh_expires_at <= now
    ):
        raise HTTPException(status_code=401, detail="refresh token expired or invalid")
    raw_token = generate_session_token()
    raw_refresh_token = generate_refresh_token()
    courier_session.token_hash = hash_token(raw_token, settings)
    courier_session.refresh_token_hash = hash_token(raw_refresh_token, settings)
    courier_session.expires_at = now + timedelta(seconds=settings.session_lifetime_seconds)
    courier_session.refresh_expires_at = now + timedelta(seconds=settings.refresh_lifetime_seconds)
    record_event(
        database,
        actor=f"courier:{courier_session.client_identifier}",
        action="session.refreshed",
        object_type="courier_session",
        object_id=str(courier_session.id),
    )
    database.commit()
    return SessionResponse(
        access_token=raw_token,
        refresh_token=raw_refresh_token,
        expires_at=courier_session.expires_at,
        refresh_expires_at=courier_session.refresh_expires_at,
        projects=[
            ProjectResponse.model_validate(project)
            for project in courier_session.invitation.projects
        ],
        purpose=courier_session.invitation.purpose,
    )


@client.get("/auth/session", response_model=SessionAuthorization)
def session_authorization(identity: CourierIdentity) -> SessionAuthorization:
    return SessionAuthorization(
        projects=[
            ProjectResponse.model_validate(project) for project in identity.invitation.projects
        ],
        purpose=identity.invitation.purpose,
    )


def downloadable_dataset(transfer: Transfer) -> DownloadDatasetSummary:
    return DownloadDatasetSummary(
        transfer_id=transfer.public_id,
        project_code=transfer.project.project_code,
        source_name=transfer.source_name,
        file_count=transfer.file_count,
        original_bytes=transfer.original_bytes,
        transport_bytes=known_transport_bytes(transfer),
        verified_at=transfer.verified_at,
        hash_algorithm=transfer.hash_algorithm,
    )


def require_download_invitation(identity: CourierIdentity) -> set[uuid.UUID]:
    if identity.invitation.purpose != "download":
        raise HTTPException(status_code=403, detail="invitation does not allow downloads")
    return {project.id for project in identity.invitation.projects}


@client.get("/downloads", response_model=list[DownloadDatasetSummary])
def list_downloadable_datasets(
    database: Database, identity: CourierIdentity
) -> list[DownloadDatasetSummary]:
    project_ids = require_download_invitation(identity)
    transfers = database.scalars(
        select(Transfer)
        .where(Transfer.project_id.in_(project_ids), Transfer.status == "complete")
        .order_by(Transfer.verified_at.desc())
    ).unique()
    return [downloadable_dataset(transfer) for transfer in transfers]


@client.post("/downloads/{transfer_id}", response_model=DownloadPlan)
def create_download_plan(
    transfer_id: str, database: Database, identity: CourierIdentity, storage: Storage
) -> DownloadPlan:
    project_ids = require_download_invitation(identity)
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None or transfer.project_id not in project_ids:
        raise HTTPException(status_code=404, detail="dataset not found")
    if transfer.status != "complete" or transfer.verified_at is None or transfer.manifest is None:
        raise HTTPException(status_code=409, detail="dataset is not verified and downloadable")
    objects = [
        DownloadObject(
            object_id=item.id,
            kind=item.kind,
            compression=item.compression,
            encoding_version=item.encoding_version,
            original_bytes=item.original_bytes,
            transport_bytes=(
                sum(part.size for part in item.parts if part.size is not None)
                if all(part.size is not None for part in item.parts)
                else None
            ),
        )
        for item in sorted(transfer.transport_objects, key=lambda value: str(value.id))
    ]
    record_event(
        database,
        actor=f"courier:{identity.client_identifier}",
        action="transfer.client_download_planned",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"project_code": transfer.project.project_code, "object_count": len(objects)},
    )
    database.commit()
    return DownloadPlan(
        dataset=downloadable_dataset(transfer),
        expires_in_seconds=storage.url_lifetime,
        manifest=transfer.manifest,
        objects=objects,
    )


@client.post(
    "/downloads/{transfer_id}/objects/{object_id}/authorize",
    response_model=DownloadObject,
)
def authorize_download_object(
    transfer_id: str,
    object_id: uuid.UUID,
    database: Database,
    identity: CourierIdentity,
    storage: Storage,
) -> DownloadObject:
    project_ids = require_download_invitation(identity)
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None or transfer.project_id not in project_ids:
        raise HTTPException(status_code=404, detail="dataset not found")
    if transfer.status != "complete" or transfer.verified_at is None:
        raise HTTPException(status_code=409, detail="dataset is not verified and downloadable")
    item = database.scalar(
        select(TransferObject).where(
            TransferObject.transfer_id == transfer.id, TransferObject.id == object_id
        )
    )
    if item is None:
        raise HTTPException(status_code=404, detail="download object not found")
    result = DownloadObject(
        object_id=item.id,
        kind=item.kind,
        compression=item.compression,
        encoding_version=item.encoding_version,
        original_bytes=item.original_bytes,
        transport_bytes=(
            sum(part.size for part in item.parts if part.size is not None)
            if all(part.size is not None for part in item.parts)
            else None
        ),
        url=storage.authorize_download(item.object_key),
    )
    record_event(
        database,
        actor=f"courier:{identity.client_identifier}",
        action="transfer.client_download_object_authorized",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"object_id": str(item.id), "expires_in_seconds": storage.url_lifetime},
    )
    database.commit()
    return result


@client.post("/transfers", response_model=TransferResponse, status_code=status.HTTP_201_CREATED)
def create_transfer(
    payload: TransferCreate,
    database: Database,
    identity: CourierIdentity,
    response: Response,
    settings: Configuration,
) -> Transfer:
    if identity.invitation.purpose != "upload":
        raise HTTPException(status_code=403, detail="download invitations are read-only")
    if payload.file_count > settings.maximum_transfer_files:
        raise HTTPException(status_code=413, detail="transfer exceeds Registry file-count limit")
    allowed = {project.project_code: project for project in identity.invitation.projects}
    project = allowed.get(payload.project_code)
    if project is None:
        raise HTTPException(status_code=403, detail="project not allowed by invitation")
    if (
        identity.invitation.maximum_transfer_bytes is not None
        and payload.original_bytes > identity.invitation.maximum_transfer_bytes
    ):
        raise HTTPException(status_code=413, detail="transfer exceeds invitation size limit")
    existing = database.scalar(
        select(Transfer).where(
            Transfer.client_session_id == identity.id,
            Transfer.idempotency_key == payload.idempotency_key,
        )
    )
    if existing is not None:
        response.status_code = status.HTTP_200_OK
        return existing
    if payload.manifest_version != 3:
        raise HTTPException(status_code=422, detail="unsupported manifest version")
    if payload.hash_algorithm != settings.hash_algorithm:
        raise HTTPException(
            status_code=409,
            detail=f"Registry requires {settings.hash_algorithm} file digests",
        )
    transfer = Transfer(
        public_id=f"ISC-TR-{uuid.uuid4().hex[:12].upper()}",
        project_id=project.id,
        invitation_id=identity.invitation_id,
        client_session_id=identity.id,
        idempotency_key=payload.idempotency_key,
        courier_version=payload.courier_version,
        manifest_version=payload.manifest_version,
        source_name=payload.source_name,
        file_count=payload.file_count,
        original_bytes=payload.original_bytes,
        status="draft",
        hash_algorithm=payload.hash_algorithm,
    )
    database.add(transfer)
    database.flush()
    record_event(
        database,
        actor=f"courier:{identity.client_identifier}",
        action="transfer.created",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"project_code": payload.project_code},
    )
    database.commit()
    database.refresh(transfer)
    return transfer


@client.put("/transfers/{transfer_id}/manifest", response_model=ManifestResponse)
def submit_manifest(
    transfer_id: str,
    payload: TransferManifest,
    database: Database,
    identity: CourierIdentity,
    response: Response,
) -> ManifestResponse:
    transfer = owned_transfer(database, identity, transfer_id)
    canonical = payload.model_dump(mode="json", by_alias=True, exclude_none=True)
    canonical_bytes = json.dumps(
        canonical, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    digest = hashlib.sha256(canonical_bytes).hexdigest()
    if transfer.manifest_sha256 is not None:
        if transfer.manifest_sha256 != digest:
            raise HTTPException(status_code=409, detail="manifest is immutable")
        response.status_code = status.HTTP_200_OK
        return ManifestResponse(
            transfer_id=transfer.public_id,
            manifest_sha256=transfer.manifest_sha256,
            submitted_at=transfer.manifest_submitted_at,
            files=transfer.files,
            transport_objects=transfer.transport_objects,
        )
    if payload.transfer_id != transfer.public_id:
        raise HTTPException(status_code=422, detail="manifest transfer_id does not match")
    if payload.version != transfer.manifest_version:
        raise HTTPException(status_code=422, detail="manifest version does not match transfer")
    if payload.project != transfer.project.project_code:
        raise HTTPException(status_code=422, detail="manifest project does not match")
    if payload.source.name != transfer.source_name:
        raise HTTPException(status_code=422, detail="manifest source does not match")
    if payload.summary.file_count != transfer.file_count:
        raise HTTPException(status_code=422, detail="manifest file count does not match")
    if payload.summary.original_bytes != transfer.original_bytes:
        raise HTTPException(status_code=422, detail="manifest byte count does not match")
    if payload.courier.version != transfer.courier_version:
        raise HTTPException(status_code=422, detail="manifest Courier version does not match")
    algorithms = {item.digest.algorithm for item in payload.files}
    if payload.files and algorithms != {transfer.hash_algorithm}:
        raise HTTPException(status_code=422, detail="manifest hash algorithm does not match")

    object_ids = [item.id for item in payload.transport_objects]
    if database.scalar(select(TransferObject.id).where(TransferObject.id.in_(object_ids))):
        raise HTTPException(status_code=409, detail="transport object ID is already registered")

    now = datetime.now(UTC)
    transfer.manifest = canonical
    transfer.manifest_sha256 = digest
    transfer.manifest_submitted_at = now
    transfer.status = "ready"
    for item in payload.transport_objects:
        transfer.transport_objects.append(
            TransferObject(
                id=item.id,
                kind=item.kind,
                compression=item.compression,
                encoding_version=item.encoding_version,
                original_bytes=item.original_bytes,
                object_key=(
                    f"incoming/{transfer.project.project_code}/{transfer.public_id}/"
                    f"{item.id}/payload"
                ),
                status="pending",
            )
        )
    objects = {item.id: item for item in transfer.transport_objects}
    for item in payload.files:
        file_id = uuid.uuid4()
        transport_object = objects[item.transport.object_id]
        transfer.files.append(
            TransferFile(
                id=file_id,
                relative_path=item.path,
                original_size=item.size,
                original_sha256=item.digest.value,
                hash_algorithm=item.digest.algorithm,
                modified_at=item.mtime,
                compression=transport_object.compression,
                transport_encoding_version=transport_object.encoding_version,
                object_key=transport_object.object_key,
                transport_object_id=transport_object.id,
                member_index=item.transport.member_index,
                status="pending",
            )
        )
    record_event(
        database,
        actor=f"courier:{identity.client_identifier}",
        action="manifest.submitted",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"sha256": digest, "file_count": len(transfer.files)},
    )
    database.commit()
    database.refresh(transfer)
    return ManifestResponse(
        transfer_id=transfer.public_id,
        manifest_sha256=digest,
        submitted_at=now,
        files=transfer.files,
        transport_objects=transfer.transport_objects,
    )


@client.post(
    "/transfers/{transfer_id}/objects/{object_id}/multipart",
    response_model=MultipartResponse,
    status_code=status.HTTP_201_CREATED,
)
def initiate_multipart(
    transfer_id: str,
    object_id: uuid.UUID,
    database: Database,
    identity: CourierIdentity,
    storage: Storage,
    response: Response,
) -> MultipartResponse:
    _, transport_object = owned_object(database, identity, transfer_id, object_id)
    if transport_object.status == "uploaded":
        raise HTTPException(status_code=409, detail="transport object upload is already complete")
    if transport_object.multipart_upload_id is None:
        transport_object.multipart_upload_id = storage.create_multipart(transport_object.object_key)
        transport_object.status = "uploading"
        database.commit()
    else:
        response.status_code = status.HTTP_200_OK
    return MultipartResponse(
        file_id=transport_object.id,
        upload_id=transport_object.multipart_upload_id,
        status=transport_object.status,
    )


@client.get(
    "/transfers/{transfer_id}/objects/{object_id}/multipart/parts", response_model=PartsResponse
)
def list_multipart_parts(
    transfer_id: str,
    object_id: uuid.UUID,
    database: Database,
    identity: CourierIdentity,
    storage: Storage,
) -> PartsResponse:
    _, transport_object = owned_object(database, identity, transfer_id, object_id)
    if transport_object.multipart_upload_id is None:
        raise HTTPException(status_code=409, detail="multipart upload has not been initiated")
    parts = storage.list_parts(transport_object.object_key, transport_object.multipart_upload_id)
    return PartsResponse(
        file_id=object_id,
        parts=[
            UploadedPart(
                part_number=int(part["PartNumber"]),
                etag=str(part["ETag"]),
                size=int(part["Size"]),
            )
            for part in parts
        ],
    )


@client.get(
    "/transfers/{transfer_id}/objects/{object_id}/object",
    response_model=ObjectStatusResponse,
)
def object_status(
    transfer_id: str,
    object_id: uuid.UUID,
    database: Database,
    identity: CourierIdentity,
    storage: Storage,
) -> ObjectStatusResponse:
    _, transport_object = owned_object(database, identity, transfer_id, object_id)
    return ObjectStatusResponse(exists=storage.object_exists(transport_object.object_key))


@client.post(
    "/transfers/{transfer_id}/objects/{object_id}/multipart/parts/{part_number}/authorize",
    response_model=PartAuthorizationResponse,
)
def authorize_part(
    transfer_id: str,
    object_id: uuid.UUID,
    part_number: int,
    database: Database,
    identity: CourierIdentity,
    storage: Storage,
) -> PartAuthorizationResponse:
    if part_number < 1 or part_number > 10_000:
        raise HTTPException(status_code=422, detail="part number must be between 1 and 10000")
    _, transport_object = owned_object(database, identity, transfer_id, object_id)
    if transport_object.multipart_upload_id is None or transport_object.status != "uploading":
        raise HTTPException(status_code=409, detail="transport object is not accepting parts")
    return PartAuthorizationResponse(
        file_id=object_id,
        part_number=part_number,
        url=storage.authorize_part(
            transport_object.object_key, transport_object.multipart_upload_id, part_number
        ),
        expires_in_seconds=storage.url_lifetime,
    )


@client.post(
    "/transfers/{transfer_id}/objects/{object_id}/multipart/complete",
    response_model=CompleteMultipartResponse,
)
def complete_multipart(
    transfer_id: str,
    object_id: uuid.UUID,
    payload: CompleteMultipartRequest,
    database: Database,
    identity: CourierIdentity,
    storage: Storage,
) -> CompleteMultipartResponse:
    _, transport_object = owned_object(database, identity, transfer_id, object_id)
    if transport_object.status == "uploaded":
        return CompleteMultipartResponse(file_id=object_id, status="uploaded", etag="")
    if transport_object.multipart_upload_id is None:
        raise HTTPException(status_code=409, detail="multipart upload has not been initiated")
    parts = [{"PartNumber": part.part_number, "ETag": part.etag} for part in payload.parts]
    etag = storage.complete_multipart(
        transport_object.object_key, transport_object.multipart_upload_id, parts
    )
    for item in payload.parts:
        transport_object.parts.append(
            TransportPart(part_number=item.part_number, etag=item.etag, size=item.size)
        )
    transport_object.status = "uploaded"
    transport_object.completed_at = datetime.now(UTC)
    for transfer_file in transport_object.files:
        transfer_file.status = "uploaded"
    if all(
        item.status in {"uploaded", "verified"}
        for item in transport_object.transfer.transport_objects
    ):
        sizes = [
            part.size for item in transport_object.transfer.transport_objects for part in item.parts
        ]
        if all(size is not None for size in sizes):
            transport_object.transfer.transport_bytes = sum(
                size for size in sizes if size is not None
            )
    database.commit()
    return CompleteMultipartResponse(file_id=object_id, status="uploaded", etag=etag)


@client.post("/transfers/{transfer_id}/finalize", response_model=FinalizeTransferResponse)
def finalize_transfer(
    transfer_id: str, database: Database, identity: CourierIdentity
) -> FinalizeTransferResponse:
    transfer = owned_transfer(database, identity, transfer_id)
    if transfer.status in {"finalizing", "verifying", "complete"}:
        return FinalizeTransferResponse(transfer_id=transfer.public_id, status=transfer.status)
    if transfer.status == "failed":
        raise HTTPException(status_code=409, detail="transfer verification failed")
    if transfer.manifest_version != 3:
        raise HTTPException(status_code=409, detail="legacy transfer versions cannot be finalized")
    if transfer.manifest_sha256 is None:
        raise HTTPException(status_code=409, detail="manifest has not been submitted")
    if any(item.status != "uploaded" for item in transfer.transport_objects):
        raise HTTPException(status_code=409, detail="not all transport objects are uploaded")
    transfer.status = "finalizing"
    transfer.completed_at = datetime.now(UTC)
    record_event(
        database,
        actor=f"courier:{identity.client_identifier}",
        action="transfer.uploaded",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"manifest_sha256": transfer.manifest_sha256},
    )
    database.commit()
    return FinalizeTransferResponse(transfer_id=transfer.public_id, status=transfer.status)


@client.get("/transfers/{transfer_id}", response_model=TransferStatusResponse)
def get_transfer_status(
    transfer_id: str, database: Database, identity: CourierIdentity
) -> TransferStatusResponse:
    transfer = owned_transfer(database, identity, transfer_id)
    return TransferStatusResponse(
        transfer_id=transfer.public_id,
        status=transfer.status,
        manifest_sha256=transfer.manifest_sha256,
        completed_at=transfer.completed_at,
        verification_started_at=transfer.verification_started_at,
        verified_at=transfer.verified_at,
        verification_attempt_count=transfer.verification_attempt_count,
        verification_error=transfer.verification_error,
        files=transfer.files,
        hash_algorithm=transfer.hash_algorithm,
    )


router.include_router(admin)
router.include_router(client)
