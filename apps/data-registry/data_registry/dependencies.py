from datetime import UTC, datetime
from typing import Annotated

from fastapi import Depends, Header, HTTPException, status
from sqlalchemy import select
from sqlalchemy.orm import Session

from .config import Settings, get_settings
from .db import get_session
from .models import CourierSession
from .security import hash_token, secrets_equal, valid_admin_session_token
from .storage import ObjectStorage, get_object_storage

Database = Annotated[Session, Depends(get_session)]
Configuration = Annotated[Settings, Depends(get_settings)]
Storage = Annotated[ObjectStorage, Depends(get_object_storage)]


def require_admin_session_token(
    settings: Configuration,
    authorization: Annotated[str | None, Header()] = None,
) -> str:
    if authorization is not None and authorization.startswith("Bearer "):
        token = authorization.removeprefix("Bearer ")
        if valid_admin_session_token(token, settings):
            return token
    raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="admin session required")


def require_admin(
    settings: Configuration,
    authorization: Annotated[str | None, Header()] = None,
    x_admin_key: Annotated[str | None, Header()] = None,
) -> str:
    if authorization is not None and authorization.startswith("Bearer "):
        token = authorization.removeprefix("Bearer ")
        if valid_admin_session_token(token, settings):
            return "admin"
    if settings.environment == "development":
        expected = settings.admin_api_key.get_secret_value()
        if x_admin_key is not None and secrets_equal(x_admin_key, expected):
            return "admin"
    raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="admin session required")


def require_courier_session(
    database: Database,
    settings: Configuration,
    authorization: Annotated[str | None, Header()] = None,
) -> CourierSession:
    if authorization is None or not authorization.startswith("Bearer "):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED, detail="bearer token required"
        )
    token_hash = hash_token(authorization.removeprefix("Bearer "), settings)
    session = database.scalar(select(CourierSession).where(CourierSession.token_hash == token_hash))
    if (
        session is None
        or session.revoked_at is not None
        or session.invitation.revoked_at is not None
    ):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED, detail="session expired or invalid"
        )
    expires_at = session.expires_at
    if expires_at.tzinfo is None:
        expires_at = expires_at.replace(tzinfo=UTC)
    if expires_at <= datetime.now(UTC):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED, detail="session expired or invalid"
        )
    return session


AdminActor = Annotated[str, Depends(require_admin)]
AdminSessionToken = Annotated[str, Depends(require_admin_session_token)]
CourierIdentity = Annotated[CourierSession, Depends(require_courier_session)]
