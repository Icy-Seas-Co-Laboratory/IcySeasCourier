import hashlib
import json
import uuid
from datetime import UTC, datetime

from sqlalchemy import select
from test_control_plane import ADMIN, create_invitation, create_project, exchange

from data_registry.audit import record_event
from data_registry.models import Transfer, TransferFile, TransferObject


def prepare(client, database_factory, fake_storage, kind="file"):
    create_project(client)
    session = exchange(client, create_invitation(client)["invitation_code"])
    response = client.post(
        "/api/v1/transfers",
        headers={"Authorization": f"Bearer {session['access_token']}"},
        json={
            "project_code": "P26014",
            "source_name": "sample",
            "file_count": 2,
            "original_bytes": 6,
            "courier_version": "test",
            "idempotency_key": "admin-files-test",
        },
    )
    transfer_id = response.json()["public_id"]
    with database_factory() as database:
        transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
        transfer.status = "uploading"
        obj = TransferObject(
            id=uuid.uuid4(),
            transfer_id=transfer.id,
            kind=kind,
            compression="zstd" if kind == "pack" else "none",
            encoding_version=2 if kind == "pack" else 1,
            original_bytes=3,
            object_key="opaque/available",
            status="uploaded",
        )
        pending = TransferObject(
            id=uuid.uuid4(),
            transfer_id=transfer.id,
            kind="file",
            compression="none",
            encoding_version=1,
            original_bytes=3,
            object_key="opaque/pending",
            status="uploading",
        )
        database.add_all([obj, pending])
        database.flush()
        files = [
            TransferFile(
                transfer_id=transfer.id,
                relative_path=path,
                original_size=3,
                original_sha256=hashlib.sha256(b"abc").hexdigest(),
                modified_at=datetime.now(UTC),
                compression=transport.compression,
                transport_encoding_version=transport.encoding_version,
                object_key=transport.object_key,
                status=transport.status,
                transport_object_id=transport.id,
                member_index=0,
            )
            for path, transport in [("folder/ready.txt", obj), ("folder/pending.txt", pending)]
        ]
        database.add_all(files)
        record_event(
            database,
            actor="test",
            action="transfer.test_event",
            object_type="transfer",
            object_id=transfer_id,
            metadata={"message": "searchable 100%"},
        )
        for i in range(510):
            record_event(
                database,
                actor="test",
                action="transfer.bulk_event",
                object_type="transfer",
                object_id=transfer_id,
                metadata={"sequence": i},
            )
        record_event(
            database,
            actor="test",
            action="transfer.unrelated",
            object_type="transfer",
            object_id="another-transfer",
        )
        database.commit()
        file_id, pending_id = str(files[0].id), str(files[1].id)
    fake_storage.objects["opaque/available"] = b"abc"
    return transfer_id, file_id, pending_id


def test_project_browser_and_live_object_download(client, database_factory, fake_storage):
    transfer_id, file_id, pending_id = prepare(client, database_factory, fake_storage)
    summary = client.get("/api/v1/admin/projects/P26014/summary", headers=ADMIN)
    assert summary.status_code == 200
    assert summary.json()["active_transfers"] == 1
    assert summary.json()["uploaded_file_count"] == 1
    assert summary.json()["verified_file_count"] == 0
    assert summary.json()["live_transfers"][0]["transfer_id"] == transfer_id
    root = client.get("/api/v1/admin/projects/P26014/files", headers=ADMIN).json()
    assert root["entries"][0]["kind"] == "folder"
    listing = client.get(
        "/api/v1/admin/projects/P26014/files", headers=ADMIN, params={"path": "folder", "limit": 1}
    ).json()
    assert listing["total"] == 2
    assert len(listing["entries"]) == 1
    download = client.post(
        f"/api/v1/admin/transfers/{transfer_id}/files/{file_id}/download", headers=ADMIN
    )
    assert download.status_code == 200
    assert download.json()["verified"] is False
    assert download.json()["transport_bytes"] == 3
    assert download.json()["pack_member_count"] == 1
    assert download.json()["pack_original_bytes"] == 3
    assert download.json()["url"].startswith("https://objects.test/")
    assert (
        client.post(
            f"/api/v1/admin/transfers/{transfer_id}/files/{pending_id}/download", headers=ADMIN
        ).status_code
        == 409
    )
    assert (
        client.get(
            "/api/v1/admin/projects/P26014/files", headers=ADMIN, params={"path": "../folder"}
        ).status_code
        == 422
    )
    assert (
        client.post(
            f"/api/v1/admin/transfers/{transfer_id}/files/{uuid.uuid4()}/download", headers=ADMIN
        ).status_code
        == 404
    )
    create_project(client, "P26015")
    assert (
        client.get(
            "/api/v1/admin/projects/P26015/files",
            headers=ADMIN,
            params={"transfer_id": transfer_id},
        ).status_code
        == 404
    )


def test_pack_download_metadata_and_auth(client, database_factory, fake_storage):
    transfer_id, file_id, _ = prepare(client, database_factory, fake_storage, "pack")
    url = f"/api/v1/admin/transfers/{transfer_id}/files/{file_id}/download"
    assert client.post(url).status_code == 401
    result = client.post(url, headers=ADMIN).json()
    assert (result["kind"], result["compression"], result["member_index"]) == ("pack", "zstd", 0)
    assert result["relative_path"] == "folder/ready.txt"
    assert result["digest"] == hashlib.sha256(b"abc").hexdigest()
    assert result["pack_member_count"] == 1
    assert result["pack_original_bytes"] == 3
    with database_factory() as database:
        first = database.get(TransferFile, uuid.UUID(file_id))
        database.add(
            TransferFile(
                transfer_id=first.transfer_id,
                relative_path="folder/other.txt",
                original_size=7,
                original_sha256=hashlib.sha256(b"another").hexdigest(),
                modified_at=datetime.now(UTC),
                compression="zstd",
                transport_encoding_version=2,
                object_key=first.object_key,
                status="uploaded",
                transport_object_id=first.transport_object_id,
                member_index=1,
            )
        )
        database.commit()
    result = client.post(url, headers=ADMIN).json()
    assert result["pack_member_count"] == 2
    assert result["pack_original_bytes"] == 10


def test_transfer_logs_search_pagination_and_complete_export(
    client, database_factory, fake_storage
):
    transfer_id, _, _ = prepare(client, database_factory, fake_storage)
    base = f"/api/v1/admin/transfers/{transfer_id}/logs"
    assert client.get(base).status_code == 401
    page = client.get(base, headers=ADMIN, params={"limit": 1, "offset": 1}).json()
    assert page["total"] > 500
    assert len(page["events"]) == 1
    search = client.get(base, headers=ADMIN, params={"search": "100%"}).json()
    assert search["total"] == 1
    export = client.get(base + "/download", headers=ADMIN)
    assert export.status_code == 200
    lines = [json.loads(line) for line in export.text.splitlines()]
    assert lines[0]["type"] == "scope"
    assert len(lines) == page["total"] + 1
    assert all(item.get("action") != "transfer.unrelated" for item in lines)
    assert (
        client.get(base + "/download", headers=ADMIN, params={"search": "100%"}).text.count("\n")
        == 2
    )
    assert client.get("/api/v1/admin/transfers/missing/logs", headers=ADMIN).status_code == 404


def test_admin_csp_allows_only_configured_storage_origin(client, settings):
    response = client.get("/admin/")
    csp = response.headers["Content-Security-Policy"]
    assert "'wasm-unsafe-eval'" in csp
    assert "'unsafe-eval'" not in csp
    assert "connect-src 'self' http://127.0.0.1:8333" in csp
    assert "connect-src *" not in csp
