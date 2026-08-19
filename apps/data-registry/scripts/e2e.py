"""Exercise the Registry and SeaweedFS data plane as a single vertical slice."""

import hashlib
import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from datetime import UTC, datetime, timedelta

import blake3
import xxhash

BASE_URL = os.getenv("E2E_REGISTRY_URL", "http://127.0.0.1:8000")
ADMIN_KEY = os.getenv("REGISTRY_ADMIN_API_KEY", "development-only-change-me")
S3_CONNECT_HOST = os.getenv("E2E_S3_CONNECT_HOST")


def content_digest(content: bytes, algorithm: str) -> str:
    if algorithm == "sha256":
        return hashlib.sha256(content).hexdigest()
    if algorithm == "xxhash3":
        return xxhash.xxh3_128_hexdigest(content)
    if algorithm == "blake3":
        return blake3.blake3(content).hexdigest()
    raise RuntimeError(f"unsupported Registry hash algorithm: {algorithm}")


def request(
    method: str, path: str, payload: dict | None = None, headers: dict | None = None
) -> tuple[int, dict, dict]:
    body = None if payload is None else json.dumps(payload).encode()
    all_headers = {"Accept": "application/json", **(headers or {})}
    if body is not None:
        all_headers["Content-Type"] = "application/json"
    try:
        with urllib.request.urlopen(
            urllib.request.Request(BASE_URL + path, data=body, headers=all_headers, method=method)
        ) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else {}, dict(response.headers)
    except urllib.error.HTTPError as error:
        raw = error.read()
        detail = raw.decode(errors="replace")
        raise RuntimeError(f"{method} {path} failed ({error.code}): {detail}") from error


def upload_part(url: str, content: bytes) -> str:
    parsed = urllib.parse.urlsplit(url)
    request_url = url
    headers = {}
    if S3_CONNECT_HOST:
        request_url = urllib.parse.urlunsplit(
            (parsed.scheme, S3_CONNECT_HOST, parsed.path, parsed.query, parsed.fragment)
        )
        headers["Host"] = parsed.netloc
    with urllib.request.urlopen(
        urllib.request.Request(request_url, data=content, headers=headers, method="PUT")
    ) as response:
        etag = response.headers.get("ETag")
    if not etag:
        raise RuntimeError("S3 upload did not return an ETag")
    return etag


def main() -> None:
    admin = {"X-Admin-Key": ADMIN_KEY}
    try:
        request(
            "POST",
            "/api/v1/admin/projects",
            {"project_code": "P26014", "name": "Courier development smoke test"},
            admin,
        )
    except RuntimeError as error:
        if "409" not in str(error):
            raise
    _, invitation, _ = request(
        "POST",
        "/api/v1/admin/invitations",
        {
            "project_codes": ["P26014"],
            "expires_at": (datetime.now(UTC) + timedelta(hours=1)).isoformat(),
            "maximum_uses": 1,
            "created_by": "dev-e2e@icyseas.co",
        },
        admin,
    )
    _, session, _ = request(
        "POST",
        "/api/v1/auth/invitations/exchange",
        {
            "invitation_code": invitation["invitation_code"],
            "client_identifier": "dev-e2e",
            "courier_version": "0.1.0",
        },
    )
    bearer = {"Authorization": f"Bearer {session['access_token']}"}
    content = b"temperature,salinity\n-1.2,31.4\n"
    _, system_config, _ = request("GET", "/api/v1/system/config")
    hash_algorithm = system_config["hash_algorithm"]
    _, transfer, _ = request(
        "POST",
        "/api/v1/transfers",
        {
            "project_code": "P26014",
            "source_name": "dev-smoke-cast",
            "file_count": 1,
            "original_bytes": len(content),
            "manifest_version": 3,
            "courier_version": "0.1.0",
            "idempotency_key": f"dev-e2e-{datetime.now(UTC).timestamp()}",
            "hash_algorithm": hash_algorithm,
        },
        bearer,
    )
    transfer_id = transfer["public_id"]
    now = datetime.now(UTC).isoformat()
    object_id = str(uuid.uuid4())
    _, manifest, _ = request(
        "PUT",
        f"/api/v1/transfers/{transfer_id}/manifest",
        {
            "schema": "icy-seas-transfer-manifest",
            "version": 3,
            "transfer_id": transfer_id,
            "project": "P26014",
            "created_at": now,
            "courier": {
                "version": "0.1.0",
                "platform": "dev-e2e",
                "transport_encoding_version": 2,
            },
            "source": {"name": "dev-smoke-cast"},
            "summary": {"file_count": 1, "original_bytes": len(content)},
            "transport_objects": [
                {
                    "id": object_id,
                    "kind": "file",
                    "compression": "none",
                    "encoding_version": 1,
                    "original_bytes": len(content),
                }
            ],
            "files": [
                {
                    "path": "casts/dev-smoke.csv",
                    "size": len(content),
                    "mtime": now,
                    "digest": {
                        "algorithm": hash_algorithm,
                        "value": content_digest(content, hash_algorithm),
                    },
                    "transport": {"object_id": object_id, "member_index": 0},
                }
            ],
        },
        bearer,
    )
    object_id = manifest["transport_objects"][0]["id"]
    request(
        "POST", f"/api/v1/transfers/{transfer_id}/objects/{object_id}/multipart", headers=bearer
    )
    _, authorization, _ = request(
        "POST",
        f"/api/v1/transfers/{transfer_id}/objects/{object_id}/multipart/parts/1/authorize",
        headers=bearer,
    )
    etag = upload_part(authorization["url"], content)
    request(
        "POST",
        f"/api/v1/transfers/{transfer_id}/objects/{object_id}/multipart/complete",
        {"parts": [{"part_number": 1, "etag": etag, "size": len(content)}]},
        bearer,
    )
    _, finalized, _ = request("POST", f"/api/v1/transfers/{transfer_id}/finalize", headers=bearer)
    deadline = time.monotonic() + 30
    while finalized["status"] not in {"complete", "failed"} and time.monotonic() < deadline:
        time.sleep(0.5)
        _, finalized, _ = request("GET", f"/api/v1/transfers/{transfer_id}", headers=bearer)
    if finalized["status"] != "complete":
        raise RuntimeError(f"verification did not complete: {finalized}")
    print(
        json.dumps(
            {
                "result": "ok",
                "transfer_id": transfer_id,
                "manifest_sha256": manifest["manifest_sha256"],
                "status": finalized["status"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
