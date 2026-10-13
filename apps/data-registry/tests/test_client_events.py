import uuid
from datetime import UTC, datetime

from sqlalchemy import func, select
from test_control_plane import ADMIN, create_invitation, create_project, exchange

from data_registry.models import AuditEvent, TransferClientEvent


def fixture_transfer(client):
    create_project(client)
    invitation = create_invitation(client)
    session = exchange(client, invitation["invitation_code"])
    headers = {"Authorization": f"Bearer {session['access_token']}"}
    transfer = client.post(
        "/api/v1/transfers",
        headers=headers,
        json={
            "project_code": "P26014",
            "source_name": "client-logs",
            "file_count": 0,
            "original_bytes": 0,
            "courier_version": "0.3.5",
            "idempotency_key": "client-logs",
        },
    ).json()
    return transfer["public_id"], headers


def event():
    return {
        "id": str(uuid.uuid4()),
        "occurred_at": datetime.now(UTC).isoformat(),
        "kind": "part.attempt_failed",
        "details": {
            "object_id": str(uuid.uuid4()),
            "part_number": 1,
            "attempt": 2,
            "error_class": "transient",
        },
    }


def test_client_events_replay_without_duplicate_logs(client, database_factory):
    transfer_id, headers = fixture_transfer(client)
    endpoint = f"/api/v1/transfers/{transfer_id}/events"
    item = event()
    for _ in range(2):
        response = client.post(endpoint, headers=headers, json={"events": [item]})
        assert response.status_code == 200
        assert response.json()["acknowledged"] == [item["id"]]
    with database_factory() as database:
        assert database.scalar(select(func.count()).select_from(TransferClientEvent)) == 1
        logs = database.scalars(
            select(AuditEvent).where(AuditEvent.action == "client.part.attempt_failed")
        ).all()
        assert len(logs) == 1
        assert logs[0].object_id == transfer_id
        assert logs[0].event_metadata["error_class"] == "transient"
        assert logs[0].event_metadata["event_id"] == item["id"]
    changed = {**item, "details": {**item["details"], "attempt": 3}}
    assert client.post(endpoint, headers=headers, json={"events": [changed]}).status_code == 409
    # A conflicting batch rolls back its earlier new event as well.
    assert (
        client.post(endpoint, headers=headers, json={"events": [event(), changed]}).status_code
        == 409
    )
    with database_factory() as database:
        assert database.scalar(select(func.count()).select_from(TransferClientEvent)) == 1


def test_client_events_reject_secrets_and_cross_session_ingestion(client):
    transfer_id, headers = fixture_transfer(client)
    endpoint = f"/api/v1/transfers/{transfer_id}/events"
    item = event()
    assert client.post(endpoint, json={"events": [item]}).status_code == 401
    item["details"]["url"] = "https://storage.example?secret=value"
    assert client.post(endpoint, headers=headers, json={"events": [item]}).status_code == 422
    item = event()
    item["details"]["error_class"] = "iscs_secret"
    assert client.post(endpoint, headers=headers, json={"events": [item]}).status_code == 422
    assert (
        client.post(endpoint, headers=headers, json={"events": [event()] * 101}).status_code == 422
    )
    other = exchange(client, create_invitation(client)["invitation_code"])
    assert (
        client.post(
            endpoint,
            headers={"Authorization": f"Bearer {other['access_token']}"},
            json={"events": [event()]},
        ).status_code
        == 403
    )
    download = client.post(
        "/api/v1/admin/invitations",
        headers=ADMIN,
        json={
            "project_codes": ["P26014"],
            "purpose": "download",
            "created_by": "test",
            "expires_at": "2099-01-01T00:00:00Z",
        },
    ).json()
    reader = exchange(client, download["invitation_code"])
    assert (
        client.post(
            endpoint,
            headers={"Authorization": f"Bearer {reader['access_token']}"},
            json={"events": [event()]},
        ).status_code
        == 403
    )
