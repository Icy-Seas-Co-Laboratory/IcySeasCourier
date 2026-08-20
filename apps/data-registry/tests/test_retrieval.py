import hashlib
import io
import json
import struct
import uuid
from datetime import UTC, datetime

import zstandard

from data_registry.models import Transfer, TransferFile, TransferObject
from data_registry.retrieve import retrieve_transfer_record


class MemoryStorage:
    def __init__(self, objects: dict[str, bytes]) -> None:
        self.objects = objects

    def open_object(self, object_key: str):
        return io.BytesIO(self.objects[object_key])


def record(object_id: uuid.UUID, object_key: str, path: str, content: bytes, index: int):
    return TransferFile(
        id=uuid.uuid4(),
        transfer_id=uuid.uuid4(),
        relative_path=path,
        original_size=len(content),
        original_sha256=hashlib.sha256(content).hexdigest(),
        hash_algorithm="sha256",
        modified_at=datetime.now(UTC),
        compression="zstd",
        transport_encoding_version=2,
        object_key=object_key,
        transport_object_id=object_id,
        member_index=index,
        status="verified",
    )


def encoded_pack(files: list[tuple[str, bytes]]) -> bytes:
    output = io.BytesIO()
    with zstandard.ZstdCompressor().stream_writer(output, closefd=False) as writer:
        writer.write(b"ISCPACK1")
        for path, content in files:
            header = json.dumps(
                {
                    "path": path,
                    "size": len(content),
                    "digest_algorithm": "sha256",
                    "digest": hashlib.sha256(content).hexdigest(),
                },
                separators=(",", ":"),
            ).encode()
            writer.write(struct.pack("<I", len(header)))
            writer.write(header)
            writer.write(struct.pack("<Q", len(content)))
            writer.write(content)
        writer.write(struct.pack("<I", 0))
    return output.getvalue()


def test_retrieval_reconstructs_packs_and_writes_provenance(tmp_path) -> None:
    files = [("a.txt", b"ice"), ("nested/b.txt", b"ocean")]
    object_id = uuid.uuid4()
    object_key = "incoming/P26014/ISC-TR-TEST/object/payload"
    transport_object = TransferObject(
        id=object_id,
        transfer_id=uuid.uuid4(),
        kind="pack",
        compression="zstd",
        encoding_version=2,
        original_bytes=8,
        object_key=object_key,
        status="verified",
    )
    transport_object.files = [
        record(object_id, object_key, path, content, index)
        for index, (path, content) in enumerate(files)
    ]
    transfer = Transfer(
        public_id="ISC-TR-TEST",
        status="complete",
        manifest={"schema": "icy-seas-transfer-manifest", "version": 3},
        manifest_sha256="0" * 64,
        file_count=2,
        original_bytes=8,
        transport_bytes=len(encoded_pack(files)),
    )
    transfer.transport_objects = [transport_object]
    destination = tmp_path / "retrieved"

    receipt = retrieve_transfer_record(
        transfer, MemoryStorage({object_key: encoded_pack(files)}), destination
    )

    assert (destination / "data/a.txt").read_bytes() == b"ice"
    assert (destination / "data/nested/b.txt").read_bytes() == b"ocean"
    assert json.loads((destination / "courier-metadata/manifest.json").read_text())["version"] == 3
    assert receipt["file_count"] == 2
