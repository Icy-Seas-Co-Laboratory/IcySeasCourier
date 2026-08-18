import hashlib
from datetime import UTC, datetime, timedelta

from conftest import FakeObjectStorage
from fastapi.testclient import TestClient
from pydantic import SecretStr, ValidationError
from sqlalchemy.orm import Session, sessionmaker

from data_registry.config import Settings
from data_registry.verification import claim_transfer, verify_claim

ADMIN = {"X-Admin-Key": "test-admin-key"}


def create_project(client: TestClient, code: str = "P26014") -> dict:
    response = client.post(
        "/api/v1/admin/projects",
        headers=ADMIN,
        json={"project_code": code, "name": "Chukchi Ecosystem Analysis"},
    )
    assert response.status_code == 201
    return response.json()


def create_invitation(client: TestClient, code: str = "P26014") -> dict:
    response = client.post(
        "/api/v1/admin/invitations",
        headers=ADMIN,
        json={
            "project_codes": [code],
            "expires_at": (datetime.now(UTC) + timedelta(hours=1)).isoformat(),
            "maximum_uses": 1,
            "created_by": "registry-test@icyseas.co",
        },
    )
    assert response.status_code == 201
    return response.json()


def exchange(client: TestClient, invitation_code: str) -> dict:
    response = client.post(
        "/api/v1/auth/invitations/exchange",
        json={
            "invitation_code": invitation_code,
            "client_identifier": "test-courier",
            "courier_version": "0.1.0",
        },
    )
    assert response.status_code == 200
    return response.json()


def test_admin_key_is_required(client: TestClient) -> None:
    response = client.get("/api/v1/admin/projects")
    assert response.status_code == 401


def test_admin_console_and_operational_read_models(client: TestClient) -> None:
    console = client.get("/admin/")
    assert console.status_code == 200
    assert "Operations Console" in console.text
    assert "[hidden] { display: none !important; }" in console.text
    assert 'id="login-submit"' in console.text

    create_project(client)
    invitation = create_invitation(client)

    overview = client.get("/api/v1/admin/overview", headers=ADMIN)
    assert overview.status_code == 200
    assert overview.json()["projects"] == 1
    assert overview.json()["active_invitations"] == 1

    invitations = client.get("/api/v1/admin/invitations", headers=ADMIN)
    assert invitations.status_code == 200
    assert invitations.json()[0]["id"] == invitation["id"]
    assert invitations.json()[0]["project_codes"] == ["P26014"]
    assert "invitation_code" not in invitations.json()[0]

    audit = client.get("/api/v1/admin/audit-events", headers=ADMIN)
    assert audit.status_code == 200
    assert {event["action"] for event in audit.json()} >= {
        "project.created",
        "invitation.created",
    }


def test_admin_transfer_listing_and_retry_guards(client: TestClient) -> None:
    create_project(client)
    invitation = create_invitation(client)
    session = exchange(client, invitation["invitation_code"])
    transfer = client.post(
        "/api/v1/transfers",
        headers={"Authorization": f"Bearer {session['access_token']}"},
        json={
            "project_code": "P26014",
            "source_name": "admin-console-dataset",
            "file_count": 0,
            "original_bytes": 0,
            "courier_version": "0.1.0",
            "idempotency_key": "admin-console-transfer",
        },
    ).json()

    listing = client.get("/api/v1/admin/transfers", headers=ADMIN)
    assert listing.status_code == 200
    assert listing.json()[0]["transfer_id"] == transfer["public_id"]
    assert listing.json()[0]["project_code"] == "P26014"

    detail = client.get(
        f"/api/v1/admin/transfers/{transfer['public_id']}", headers=ADMIN
    )
    assert detail.status_code == 200
    assert detail.json()["source_name"] == "admin-console-dataset"

    retry = client.post(
        f"/api/v1/admin/transfers/{transfer['public_id']}/retry", headers=ADMIN
    )
    assert retry.status_code == 409


def test_invitation_exchange_and_idempotent_transfer_creation(client: TestClient) -> None:
    project = create_project(client)
    invitation = create_invitation(client)
    session = exchange(client, invitation["invitation_code"])
    assert session["projects"][0]["project_code"] == "P26014"
    headers = {"Authorization": f"Bearer {session['access_token']}"}
    payload = {
        "project_code": "P26014",
        "source_name": "2026_Chukchi_Cruise",
        "file_count": 12_489,
        "original_bytes": 1_840_000_000_000,
        "manifest_version": 1,
        "courier_version": "0.1.0",
        "idempotency_key": "local-transfer-123456",
    }
    first = client.post("/api/v1/transfers", headers=headers, json=payload)
    assert first.status_code == 201
    assert first.json()["project_id"] == project["id"]
    repeated = client.post("/api/v1/transfers", headers=headers, json=payload)
    assert repeated.status_code == 200
    assert repeated.json()["public_id"] == first.json()["public_id"]


def test_invitation_use_limit_and_revocation(client: TestClient) -> None:
    create_project(client)
    invitation = create_invitation(client)
    exchange(client, invitation["invitation_code"])
    second = client.post(
        "/api/v1/auth/invitations/exchange",
        json={
            "invitation_code": invitation["invitation_code"],
            "client_identifier": "second-client",
            "courier_version": "0.1.0",
        },
    )
    assert second.status_code == 401
    revoked = client.delete(f"/api/v1/admin/invitations/{invitation['id']}", headers=ADMIN)
    assert revoked.status_code == 204


def test_session_refresh_rotates_both_credentials(client: TestClient) -> None:
    create_project(client)
    invitation = create_invitation(client)
    session = exchange(client, invitation["invitation_code"])
    refreshed = client.post(
        "/api/v1/auth/sessions/refresh",
        json={"refresh_token": session["refresh_token"]},
    )
    assert refreshed.status_code == 200
    replacement = refreshed.json()
    assert replacement["access_token"] != session["access_token"]
    assert replacement["refresh_token"] != session["refresh_token"]
    assert replacement["projects"][0]["project_code"] == "P26014"

    old_access = client.post(
        "/api/v1/transfers",
        headers={"Authorization": f"Bearer {session['access_token']}"},
        json={
            "project_code": "P26014",
            "source_name": "expired-session",
            "file_count": 0,
            "original_bytes": 0,
            "courier_version": "0.1.0",
            "idempotency_key": "old-access-token",
        },
    )
    assert old_access.status_code == 401
    reused_refresh = client.post(
        "/api/v1/auth/sessions/refresh",
        json={"refresh_token": session["refresh_token"]},
    )
    assert reused_refresh.status_code == 401


def test_project_scope_and_transfer_size_are_enforced(client: TestClient) -> None:
    create_project(client, "P26014")
    create_project(client, "P26015")
    invitation_response = client.post(
        "/api/v1/admin/invitations",
        headers=ADMIN,
        json={
            "project_codes": ["P26014"],
            "expires_at": (datetime.now(UTC) + timedelta(hours=1)).isoformat(),
            "maximum_transfer_bytes": 100,
            "created_by": "registry-test@icyseas.co",
        },
    )
    session = exchange(client, invitation_response.json()["invitation_code"])
    headers = {"Authorization": f"Bearer {session['access_token']}"}
    base = {
        "source_name": "source",
        "file_count": 1,
        "original_bytes": 10,
        "courier_version": "0.1.0",
        "idempotency_key": "transfer-scope-test",
    }
    forbidden = client.post(
        "/api/v1/transfers", headers=headers, json={**base, "project_code": "P26015"}
    )
    assert forbidden.status_code == 403
    too_large = client.post(
        "/api/v1/transfers",
        headers=headers,
        json={**base, "project_code": "P26014", "original_bytes": 101},
    )
    assert too_large.status_code == 413


def test_health_is_process_liveness_only(client: TestClient) -> None:
    assert client.get("/health").json() == {"status": "ok"}


def test_manifest_is_immutable_and_drives_scoped_multipart_lifecycle(
    client: TestClient,
    settings: Settings,
    database_factory: sessionmaker[Session],
    fake_storage: FakeObjectStorage,
) -> None:
    create_project(client)
    invitation = create_invitation(client)
    session = exchange(client, invitation["invitation_code"])
    headers = {"Authorization": f"Bearer {session['access_token']}"}
    content = b"temperature,salinity\n-1.2,31.4\n"
    transfer_response = client.post(
        "/api/v1/transfers",
        headers=headers,
        json={
            "project_code": "P26014",
            "source_name": "cast-001",
            "file_count": 1,
            "original_bytes": len(content),
            "manifest_version": 1,
            "courier_version": "0.1.0",
            "idempotency_key": "manifest-lifecycle-001",
        },
    )
    transfer_id = transfer_response.json()["public_id"]
    manifest = {
        "schema": "icy-seas-transfer-manifest",
        "version": 1,
        "transfer_id": transfer_id,
        "project": "P26014",
        "created_at": datetime.now(UTC).isoformat(),
        "courier": {
            "version": "0.1.0",
            "platform": "test",
            "transport_encoding_version": 1,
        },
        "source": {"name": "cast-001"},
        "summary": {"file_count": 1, "original_bytes": len(content)},
        "files": [
            {
                "path": "casts/cast-001.csv",
                "size": len(content),
                "mtime": datetime.now(UTC).isoformat(),
                "sha256": hashlib.sha256(content).hexdigest(),
                "transport": {"compression": "none", "encoding_version": 1},
            }
        ],
    }
    submitted = client.put(
        f"/api/v1/transfers/{transfer_id}/manifest", headers=headers, json=manifest
    )
    assert submitted.status_code == 200
    transfer_file = submitted.json()["files"][0]
    assert "casts/cast-001.csv" not in transfer_file["object_key"]

    repeated = client.put(
        f"/api/v1/transfers/{transfer_id}/manifest", headers=headers, json=manifest
    )
    assert repeated.status_code == 200
    changed = {**manifest, "created_at": (datetime.now(UTC) + timedelta(seconds=1)).isoformat()}
    assert (
        client.put(f"/api/v1/transfers/{transfer_id}/manifest", headers=headers, json=changed)
        .status_code
        == 409
    )

    file_id = transfer_file["id"]
    initiated = client.post(
        f"/api/v1/transfers/{transfer_id}/files/{file_id}/multipart", headers=headers
    )
    assert initiated.status_code == 201
    authorized = client.post(
        f"/api/v1/transfers/{transfer_id}/files/{file_id}/multipart/parts/1/authorize",
        headers=headers,
    )
    assert authorized.status_code == 200
    assert authorized.json()["method"] == "PUT"
    completed = client.post(
        f"/api/v1/transfers/{transfer_id}/files/{file_id}/multipart/complete",
        headers=headers,
        json={"parts": [{"part_number": 1, "etag": '"part-etag"', "size": len(content)}]},
    )
    assert completed.json()["status"] == "uploaded"
    fake_storage.objects[transfer_file["object_key"]] = content
    finalized = client.post(f"/api/v1/transfers/{transfer_id}/finalize", headers=headers)
    assert finalized.json()["status"] == "finalizing"
    with database_factory() as database:
        claim = claim_transfer(database, settings)
    assert claim is not None
    with database_factory() as database:
        verify_claim(database, fake_storage, settings, claim)
    verified = client.get(f"/api/v1/transfers/{transfer_id}", headers=headers)
    assert verified.json()["status"] == "complete"
    assert verified.json()["files"][0]["verified_sha256"] == hashlib.sha256(content).hexdigest()


def test_production_rejects_development_secrets() -> None:
    try:
        Settings(
            environment="production",
            admin_api_key=SecretStr("development-only-change-me"),
            token_pepper=SecretStr("development-only-change-me-too"),
        )
    except ValidationError:
        return
    raise AssertionError("production settings accepted development secrets")
