from datetime import UTC, datetime, timedelta

from data_registry.config import Settings
from data_registry.security import (
    decrypt_totp_secret,
    encrypt_totp_secret,
    generate_admin_session_token,
    matching_totp_step,
    totp_code,
    valid_admin_session_token,
)


def test_totp_matches_the_rfc_6238_sha1_vector_truncated_to_six_digits() -> None:
    secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
    assert totp_code(secret, 1) == "287082"
    assert matching_totp_step(secret, "287082", timestamp=59) == 1


def test_totp_secret_encryption_and_admin_session_expiration(settings: Settings) -> None:
    secret = "JBSWY3DPEHPK3PXP"
    encrypted = encrypt_totp_secret(secret, settings)
    assert secret not in encrypted
    assert decrypt_totp_secret(encrypted, settings) == secret

    now = datetime(2026, 8, 20, 12, 0, tzinfo=UTC)
    token, expires_at = generate_admin_session_token(settings, now=now)
    assert expires_at == now + timedelta(seconds=settings.admin_session_lifetime_seconds)
    assert valid_admin_session_token(token, settings, now=now)
    assert not valid_admin_session_token(token, settings, now=expires_at)
