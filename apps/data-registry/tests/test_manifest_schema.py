import json
from pathlib import Path

import jsonschema

SCHEMA = Path(__file__).parents[3] / "schemas" / "transfer-manifest-v1.schema.json"
SCHEMA_V2 = Path(__file__).parents[3] / "schemas" / "transfer-manifest-v2.schema.json"
SCHEMA_V3 = Path(__file__).parents[3] / "schemas" / "transfer-manifest-v3.schema.json"


def test_manifest_v1_accepts_logical_relative_files() -> None:
    schema = json.loads(SCHEMA.read_text())
    manifest = {
        "schema": "icy-seas-transfer-manifest",
        "version": 1,
        "transfer_id": "ISC-TR-TEST",
        "project": "P26014",
        "created_at": "2026-08-11T22:30:00Z",
        "courier": {
            "version": "0.1.0",
            "platform": "linux-x86_64",
            "transport_encoding_version": 1,
        },
        "source": {"name": "2026_Chukchi_Cruise"},
        "summary": {"file_count": 1, "original_bytes": 5},
        "files": [
            {
                "path": "CTD/cast001.csv",
                "size": 5,
                "mtime": "2026-07-12T03:42:51Z",
                "sha256": "0" * 64,
                "transport": {"compression": "none", "encoding_version": 1},
            }
        ],
    }
    jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker()).validate(
        manifest
    )


def test_manifest_rejects_absolute_client_paths() -> None:
    schema = json.loads(SCHEMA.read_text())
    manifest_file = schema["properties"]["files"]["items"]
    validator = jsonschema.Draft202012Validator(manifest_file)
    errors = list(
        validator.iter_errors(
            {
                "path": "/Users/example/raw.csv",
                "size": 1,
                "mtime": "2026-07-12T03:42:51Z",
                "sha256": "0" * 64,
                "transport": {"compression": "none", "encoding_version": 1},
            }
        )
    )
    assert errors


def test_manifest_v2_accepts_each_explicit_digest_algorithm() -> None:
    schema = json.loads(SCHEMA_V2.read_text())
    for algorithm, value in (
        ("sha256", "0" * 64),
        ("xxhash3", "0" * 32),
        ("blake3", "0" * 64),
    ):
        manifest = {
            "schema": "icy-seas-transfer-manifest",
            "version": 2,
            "transfer_id": "ISC-TR-TEST",
            "project": "P26014",
            "created_at": "2026-08-11T22:30:00Z",
            "courier": {
                "version": "0.1.0",
                "platform": "linux",
                "transport_encoding_version": 1,
            },
            "source": {"name": "dataset"},
            "summary": {"file_count": 1, "original_bytes": 5},
            "files": [{
                "path": "data.csv",
                "size": 5,
                "mtime": "2026-07-12T03:42:51Z",
                "digest": {"algorithm": algorithm, "value": value},
                "transport": {"compression": "none", "encoding_version": 1},
            }],
        }
        jsonschema.Draft202012Validator(schema).validate(manifest)


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
