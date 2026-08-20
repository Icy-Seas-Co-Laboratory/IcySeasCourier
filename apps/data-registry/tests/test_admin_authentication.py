import time

from fastapi.testclient import TestClient

from data_registry.config import Settings
from data_registry.security import totp_code


def test_first_admin_login_enrolls_totp_and_returns_a_short_lived_session(
    client: TestClient,
) -> None:
    status = client.get("/api/v1/admin/authentication/status")
    assert status.status_code == 200
    assert status.json() == {"configured": False}

    invalid = client.post(
        "/api/v1/admin/authentication/setup",
        json={"admin_key": "wrong-key"},
    )
    assert invalid.status_code == 401

    setup = client.post(
        "/api/v1/admin/authentication/setup",
        json={"admin_key": "test-admin-key"},
    )
    assert setup.status_code == 200
    secret = setup.json()["secret"]
    assert setup.json()["provisioning_uri"].startswith("otpauth://totp/")

    code = totp_code(secret, int(time.time() // 30))
    wrong = f"{(int(code) + 1) % 1_000_000:06d}"
    wrong_code = client.post(
        "/api/v1/admin/authentication/setup/confirm",
        json={"admin_key": "test-admin-key", "totp_code": wrong},
    )
    assert wrong_code.status_code == 401

    confirmed = client.post(
        "/api/v1/admin/authentication/setup/confirm",
        json={"admin_key": "test-admin-key", "totp_code": code},
    )
    assert confirmed.status_code == 200
    token = confirmed.json()["access_token"]
    assert token.startswith("isca_")

    status = client.get("/api/v1/admin/authentication/status")
    assert status.json() == {"configured": True}
    assert (
        client.get(
            "/api/v1/admin/projects",
            headers={"Authorization": f"Bearer {token}"},
        ).status_code
        == 200
    )

    repeated_setup = client.post(
        "/api/v1/admin/authentication/setup",
        json={"admin_key": "test-admin-key"},
    )
    assert repeated_setup.status_code == 409

    replayed_code = client.post(
        "/api/v1/admin/authentication/session",
        json={"admin_key": "test-admin-key", "totp_code": code},
    )
    assert replayed_code.status_code == 401


def test_static_admin_key_is_not_an_api_session_outside_development(
    client: TestClient,
    settings: Settings,
) -> None:
    settings.environment = "beta"
    try:
        response = client.get(
            "/api/v1/admin/projects",
            headers={"X-Admin-Key": "test-admin-key"},
        )
        assert response.status_code == 401
    finally:
        settings.environment = "development"
