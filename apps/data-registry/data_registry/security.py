import base64
import hashlib
import hmac
import secrets

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
