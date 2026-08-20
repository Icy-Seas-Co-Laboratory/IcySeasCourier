"""Create a local demonstration project and single-use Courier invitation."""

import json
import os
import urllib.error
import urllib.request
from datetime import UTC, datetime, timedelta

BASE_URL = os.getenv("E2E_REGISTRY_URL", "http://127.0.0.1:8020")
ADMIN_KEY = os.getenv("REGISTRY_ADMIN_API_KEY", "development-only-change-me")
PROJECT_CODE = os.getenv("DEMO_PROJECT_CODE", "P26014")


def post(path: str, payload: dict) -> dict:
    request = urllib.request.Request(
        BASE_URL + path,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json", "X-Admin-Key": ADMIN_KEY},
        method="POST",
    )
    with urllib.request.urlopen(request) as response:
        return json.load(response)


try:
    post(
        "/api/v1/admin/projects",
        {"project_code": PROJECT_CODE, "name": "Courier demonstration project"},
    )
except urllib.error.HTTPError as error:
    if error.code != 409:
        raise

invitation = post(
    "/api/v1/admin/invitations",
    {
        "project_codes": [PROJECT_CODE],
        "expires_at": (datetime.now(UTC) + timedelta(hours=24)).isoformat(),
        "maximum_uses": 1,
        "created_by": "local-demo@icyseas.co",
    },
)
print(invitation["invitation_code"])
