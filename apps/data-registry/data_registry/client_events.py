"""Idempotent ingestion of the client's durable operational event outbox.

Only typed operational fields are accepted. Free-form messages, paths, URLs,
request bodies, and credentials never belong in this endpoint.
"""

import hashlib
import json
import uuid
from datetime import UTC, datetime
from typing import Literal

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, ConfigDict, Field, field_validator
from sqlalchemy import select
from sqlalchemy.exc import IntegrityError

from .api import owned_transfer
from .audit import record_event
from .dependencies import CourierIdentity, Database
from .models import Transfer, TransferClientEvent

router = APIRouter(prefix="/api/v1", tags=["courier"])
State = Literal[
    "draft",
    "inventorying",
    "ready",
    "uploading",
    "paused",
    "interrupted",
    "finalizing",
    "verifying",
    "complete",
    "failed",
    "cancelled",
]


class EventDetails(BaseModel):
    model_config = ConfigDict(extra="forbid")
    object_id: uuid.UUID | None = None
    part_number: int | None = Field(default=None, ge=1, le=10_000)
    attempt: int | None = Field(default=None, ge=1, le=2_147_483_647)
    bytes: int | None = Field(default=None, ge=0, le=9_223_372_036_854_775_807)
    state: State | None = None
    from_state: State | None = None
    error_class: (
        Literal[
            "transient",
            "authorization_expired",
            "upload_not_found",
            "permanent",
            "paused",
            "local",
            "source_changed",
            "reconciliation",
            "invalid_plan",
            "retries_exhausted",
        ]
        | None
    ) = None


class ClientEvent(BaseModel):
    model_config = ConfigDict(extra="forbid")
    id: uuid.UUID
    occurred_at: datetime
    kind: Literal[
        "state.created",
        "state.changed",
        "state.snapshot",
        "part.confirmed",
        "part.failed",
        "part.attempt_failed",
        "object.upload_started",
        "object.upload_succeeded",
        "object.upload_paused",
        "object.upload_failed",
        "object.reconciliation_retry",
        "object.completed",
        "object.completion_failed",
        "object.completion_retry",
    ]
    details: EventDetails

    @field_validator("occurred_at")
    @classmethod
    def timezone_required(cls, value: datetime) -> datetime:
        if value.utcoffset() is None:
            raise ValueError("event time must include a timezone")
        return value.astimezone(UTC)


class EventBatch(BaseModel):
    model_config = ConfigDict(extra="forbid")
    events: list[ClientEvent] = Field(min_length=1, max_length=100)


class EventAcknowledgement(BaseModel):
    acknowledged: list[uuid.UUID]


@router.post("/transfers/{transfer_id}/events", response_model=EventAcknowledgement)
def ingest_events(
    transfer_id: str,
    payload: EventBatch,
    database: Database,
    identity: CourierIdentity,
) -> EventAcknowledgement:
    transfer = owned_transfer(database, identity, transfer_id)
    # Serialize same-transfer ingest retries without duplicating audit records.
    database.scalar(select(Transfer.id).where(Transfer.id == transfer.id).with_for_update())
    acknowledged = []
    for item in payload.events:
        canonical = item.model_dump(mode="json", exclude_none=True)
        digest = hashlib.sha256(
            json.dumps(canonical, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        existing = database.get(TransferClientEvent, item.id)
        if existing is not None:
            if existing.transfer_id != transfer.id or existing.payload_sha256 != digest:
                raise HTTPException(
                    status_code=409, detail="event UUID already has different content"
                )
        else:
            database.add(
                TransferClientEvent(
                    id=item.id,
                    transfer_id=transfer.id,
                    payload_sha256=digest,
                )
            )
            record_event(
                database,
                actor=f"courier:{identity.client_identifier}",
                action=f"client.{item.kind}",
                object_type="transfer",
                object_id=transfer.public_id,
                metadata={
                    "source": "courier-client",
                    "event_id": str(item.id),
                    "occurred_at": item.occurred_at.isoformat(),
                    **canonical["details"],
                },
            )
            try:
                database.flush()
            except IntegrityError as error:
                database.rollback()
                raise HTTPException(status_code=409, detail="event UUID already exists") from error
        acknowledged.append(item.id)
    database.commit()
    return EventAcknowledgement(acknowledged=acknowledged)
