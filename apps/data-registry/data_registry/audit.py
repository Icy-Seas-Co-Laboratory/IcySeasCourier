from sqlalchemy.orm import Session

from .models import AuditEvent


def record_event(
    database: Session,
    *,
    actor: str,
    action: str,
    object_type: str,
    object_id: str,
    metadata: dict | None = None,
) -> None:
    database.add(
        AuditEvent(
            actor=actor,
            action=action,
            object_type=object_type,
            object_id=object_id,
            event_metadata=metadata or {},
        )
    )
