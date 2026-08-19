import json
from pathlib import Path

import jsonschema

SCHEMA_V3 = Path(__file__).parents[3] / "schemas" / "transfer-manifest-v3.schema.json"


def test_manifest_rejects_absolute_client_paths() -> None:
    schema = json.loads(SCHEMA_V3.read_text())
    manifest_file = schema["properties"]["files"]["items"]
    validator = jsonschema.Draft202012Validator(manifest_file)
    errors = list(
        validator.iter_errors(
            {
                "path": "/Users/example/raw.csv",
                "size": 1,
                "mtime": "2026-07-12T03:42:51Z",
                "digest": {"algorithm": "sha256", "value": "0" * 64},
                "transport": {
                    "object_id": "4d5d7d7f-944f-4eef-a4d8-80d42c608dac",
                    "member_index": 0,
                },
            }
        )
    )
    assert errors


def test_manifest_v3_maps_multiple_logical_files_to_one_pack() -> None:
    schema = json.loads(SCHEMA_V3.read_text())
    object_id = "4d5d7d7f-944f-4eef-a4d8-80d42c608dac"
    manifest = {
        "schema": "icy-seas-transfer-manifest",
        "version": 3,
        "transfer_id": "ISC-TR-000001",
        "project": "P26014",
        "created_at": "2026-08-11T22:30:00Z",
        "courier": {
            "version": "0.1.0",
            "platform": "linux",
            "transport_encoding_version": 2,
        },
        "source": {"name": "cruise"},
        "summary": {"file_count": 2, "original_bytes": 5},
        "transport_objects": [
            {
                "id": object_id,
                "kind": "pack",
                "compression": "zstd",
                "encoding_version": 2,
                "original_bytes": 5,
            }
        ],
        "files": [
            {
                "path": "a.txt",
                "size": 2,
                "mtime": "2026-08-11T22:30:00Z",
                "digest": {"algorithm": "sha256", "value": "a" * 64},
                "transport": {"object_id": object_id, "member_index": 0},
            },
            {
                "path": "b.txt",
                "size": 3,
                "mtime": "2026-08-11T22:30:00Z",
                "digest": {"algorithm": "sha256", "value": "b" * 64},
                "transport": {"object_id": object_id, "member_index": 1},
            },
        ],
    }
    jsonschema.Draft202012Validator(schema).validate(manifest)
