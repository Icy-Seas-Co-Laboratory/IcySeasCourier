import base64
import hashlib
import hmac
import secrets
import struct
import time
from datetime import UTC, datetime

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

from .config import Settings


def generate_invitation_code() -> str:
    raw = base64.b32encode(secrets.token_bytes(8)).decode("ascii").rstrip("=")[:12]
    return f"ISC-{raw[:4]}-{raw[4:8]}-{raw[8:12]}"


def generate_session_token() -> str:
    return "iscs_" + secrets.token_urlsafe(32)


def generate_refresh_token() -> str:
    return "iscr_" + secrets.token_urlsafe(48)


def hash_token(token: str, settings: Settings) -> str:
    return hmac.new(
        settings.token_pepper.get_secret_value().encode(),
        token.strip().upper().encode() if token.startswith("ISC-") else token.encode(),
        hashlib.sha256,
    ).hexdigest()


def secrets_equal(left: str, right: str) -> bool:
    return hmac.compare_digest(left.encode(), right.encode())


def generate_totp_secret() -> str:
    return base64.b32encode(secrets.token_bytes(20)).decode("ascii").rstrip("=")


def _totp_key(secret: str) -> bytes:
    normalized = secret.strip().replace(" ", "").upper()
    padding = "=" * (-len(normalized) % 8)
    return base64.b32decode(normalized + padding, casefold=True)


def totp_code(secret: str, step: int) -> str:
    digest = hmac.new(_totp_key(secret), struct.pack(">Q", step), hashlib.sha1).digest()
    offset = digest[-1] & 0x0F
    value = struct.unpack(">I", digest[offset : offset + 4])[0] & 0x7FFFFFFF
    return f"{value % 1_000_000:06d}"


def matching_totp_step(
    secret: str, code: str, *, timestamp: float | None = None, window: int = 1
) -> int | None:
    if len(code) != 6 or not code.isdigit():
        return None
    current_step = int((time.time() if timestamp is None else timestamp) // 30)
    for offset in (0, -1, 1):
        if abs(offset) <= window and secrets_equal(totp_code(secret, current_step + offset), code):
            return current_step + offset
    return None


def _admin_encryption_key(settings: Settings) -> bytes:
    pepper = settings.token_pepper.get_secret_value().encode()
    return hashlib.sha256(b"courier-admin-totp-v1\0" + pepper).digest()


def encrypt_totp_secret(secret: str, settings: Settings) -> str:
    nonce = secrets.token_bytes(12)
    ciphertext = AESGCM(_admin_encryption_key(settings)).encrypt(
        nonce, secret.encode(), b"courier-admin-totp-v1"
    )
    return base64.urlsafe_b64encode(nonce + ciphertext).decode("ascii")


def decrypt_totp_secret(value: str, settings: Settings) -> str:
    payload = base64.urlsafe_b64decode(value.encode("ascii"))
    return (
        AESGCM(_admin_encryption_key(settings))
        .decrypt(payload[:12], payload[12:], b"courier-admin-totp-v1")
        .decode("ascii")
    )


def generate_admin_session_token(
    settings: Settings, *, now: datetime | None = None
) -> tuple[str, datetime]:
    issued_at = now or datetime.now(UTC)
    expires_at = datetime.fromtimestamp(
        issued_at.timestamp() + settings.admin_session_lifetime_seconds, UTC
    )
    payload = f"{int(expires_at.timestamp())}.{secrets.token_urlsafe(24)}"
    signing_key = hmac.new(
        settings.token_pepper.get_secret_value().encode(),
        settings.admin_api_key.get_secret_value().encode(),
        hashlib.sha256,
    ).digest()
    signature = hmac.new(
        signing_key,
        f"courier-admin-session-v1:{payload}".encode(),
        hashlib.sha256,
    ).hexdigest()
    return f"isca_{payload}.{signature}", expires_at


def valid_admin_session_token(
    token: str, settings: Settings, *, now: datetime | None = None
) -> bool:
    if not token.startswith("isca_"):
        return False
    try:
        expires, nonce, signature = token.removeprefix("isca_").split(".", 2)
        expires_at = int(expires)
    except (ValueError, TypeError):
        return False
    payload = f"{expires}.{nonce}"
    signing_key = hmac.new(
        settings.token_pepper.get_secret_value().encode(),
        settings.admin_api_key.get_secret_value().encode(),
        hashlib.sha256,
    ).digest()
    expected = hmac.new(
        signing_key,
        f"courier-admin-session-v1:{payload}".encode(),
        hashlib.sha256,
    ).hexdigest()
    current = now or datetime.now(UTC)
    return secrets_equal(signature, expected) and expires_at > int(current.timestamp())
