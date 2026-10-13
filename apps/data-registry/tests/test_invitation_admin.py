from datetime import UTC, datetime, timedelta

from fastapi.testclient import TestClient
from sqlalchemy import select
from test_control_plane import ADMIN, create_invitation, create_project, exchange

from data_registry.models import UploadInvitation


def test_invitation_review_is_repeatable_and_encrypted(client, database_factory):
    create_project(client)
    created = create_invitation(client)
    endpoint = f"/api/v1/admin/invitations/{created['id']}"
    for _ in range(2):
        response = client.get(endpoint, headers=ADMIN)
        assert response.status_code == 200
        assert response.json()["invitation_code"] == created["invitation_code"]
    assert client.get(endpoint).status_code == 401
    with database_factory() as database:
        invitation = database.scalar(select(UploadInvitation))
        assert invitation.code_ciphertext
        assert created["invitation_code"] not in invitation.code_ciphertext
        invitation.code_ciphertext = None
        database.commit()
    assert client.get(endpoint, headers=ADMIN).json()["invitation_code"] is None
    assert (
        client.get("/api/v1/admin/invitations", headers=ADMIN).json()[0]["invitation_code"] is None
    )


def test_invitation_limits_can_be_adjusted_without_resetting_usage(client):
    create_project(client)
    created = create_invitation(client)
    exchange(client, created["invitation_code"])
    endpoint = f"/api/v1/admin/invitations/{created['id']}"
    expires = (datetime.now(UTC) + timedelta(days=1)).isoformat()
    response = client.patch(
        endpoint,
        headers=ADMIN,
        json={
            "maximum_uses": None,
            "maximum_transfer_bytes": 1024,
            "expires_at": expires,
        },
    )
    assert response.status_code == 200
    assert response.json()["use_count"] == 1
    assert response.json()["maximum_uses"] is None
    assert response.json()["maximum_transfer_bytes"] == 1024
    exchange(client, created["invitation_code"])
    assert client.patch(endpoint, headers=ADMIN, json={"maximum_uses": 1}).status_code == 422
    assert (
        client.patch(endpoint, headers=ADMIN, json={"maximum_transfer_bytes": None}).status_code
        == 200
    )
    assert (
        client.patch(endpoint, headers=ADMIN, json={"project_codes": ["MISSING"]}).status_code
        == 404
    )
    assert (
        client.patch(
            endpoint, headers=ADMIN, json={"expires_at": "2020-01-01T00:00:00"}
        ).status_code
        == 422
    )
    assert client.patch(endpoint, headers=ADMIN, json={"purpose": None}).status_code == 422
    assert client.patch(endpoint, json={"maximum_uses": None}).status_code == 401


def test_invitation_scope_changes_revoke_sessions_and_preserve_read_only(client: TestClient):
    create_project(client)
    create_project(client, "P26015")
    created = create_invitation(client)
    session = exchange(client, created["invitation_code"])
    endpoint = f"/api/v1/admin/invitations/{created['id']}"
    response = client.patch(
        endpoint,
        headers=ADMIN,
        json={
            "project_codes": ["P26015"],
            "purpose": "download",
            "maximum_uses": None,
        },
    )
    assert response.status_code == 200
    assert response.json()["purpose"] == "download"
    assert response.json()["project_codes"] == ["P26015"]
    assert (
        client.get(
            "/api/v1/auth/session", headers={"Authorization": f"Bearer {session['access_token']}"}
        ).status_code
        == 401
    )
    assert (
        client.patch(endpoint, headers=ADMIN, json={"maximum_transfer_bytes": 1}).status_code == 422
    )
    assert exchange(client, created["invitation_code"])["purpose"] == "download"
    assert client.delete(endpoint, headers=ADMIN).status_code == 204
    assert client.patch(endpoint, headers=ADMIN, json={"maximum_uses": None}).status_code == 409
