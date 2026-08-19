import hashlib
import io
import json
import struct
import uuid

import blake3
import pytest
import xxhash
import zstandard

from data_registry.models import TransferFile, TransferObject
from data_registry.verification import (
    VerificationMismatch,
    verify_file,
    verify_file_against_manifest,
    verify_transport_object,
)


class MemoryStorage:
    def __init__(self, content: bytes) -> None:
        self.content = content

    def open_object(self, _object_key: str):
        return io.BytesIO(self.content)


def transfer_file(content: bytes, algorithm: str = "sha256") -> TransferFile:
    digests = {
        "sha256": hashlib.sha256(content).hexdigest(),
        "xxhash3": xxhash.xxh3_128_hexdigest(content),
        "blake3": blake3.blake3(content).hexdigest(),
    }
    return TransferFile(
        id=uuid.uuid4(),
        transfer_id=uuid.uuid4(),
        relative_path="casts/cast.csv",
        original_size=len(content),
        original_sha256=digests[algorithm],
        hash_algorithm=algorithm,
        compression="none",
        transport_encoding_version=1,
        object_key="incoming/opaque/payload",
        status="uploaded",
    )


def test_verifier_streams_and_hashes_logical_bytes() -> None:
    content = b"temperature,salinity\n-1.2,31.4\n"
    size, digest = verify_file(MemoryStorage(content), transfer_file(content))
    assert size == len(content)
    assert digest == hashlib.sha256(content).hexdigest()


@pytest.mark.parametrize("algorithm", ["sha256", "xxhash3", "blake3"])
def test_verifier_supports_configured_algorithms(algorithm: str) -> None:
    content = b"ocean-data"
    record = transfer_file(content, algorithm)
    _, digest = verify_file_against_manifest(MemoryStorage(content), record)
    assert digest == record.original_sha256


def test_verifier_rejects_unknown_transport_compression() -> None:
    record = transfer_file(b"data")
    record.compression = "unknown"
    with pytest.raises(VerificationMismatch, match="unsupported compression"):
        verify_file(MemoryStorage(b"data"), record)


def test_verifier_rejects_bytes_that_do_not_match_manifest() -> None:
    record = transfer_file(b"expected")
    with pytest.raises(VerificationMismatch, match="SHA-256"):
        verify_file_against_manifest(MemoryStorage(b"corrupt!"), record)


def test_verifier_reconstructs_and_checks_v3_pack_members() -> None:
    contents = [("a.txt", b"ice"), ("nested/b.txt", b"ocean")]
    raw = io.BytesIO()
    with zstandard.ZstdCompressor().stream_writer(raw, closefd=False) as writer:
        writer.write(b"ISCPACK1")
        for path, content in contents:
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

    object_id = uuid.uuid4()
    transport_object = TransferObject(
        id=object_id,
        transfer_id=uuid.uuid4(),
        kind="pack",
        compression="zstd",
        encoding_version=2,
        original_bytes=sum(len(content) for _, content in contents),
        object_key="incoming/pack/payload",
        status="uploaded",
    )
    transport_object.files = []
    for index, (path, content) in enumerate(contents):
        record = transfer_file(content)
        record.relative_path = path
        record.transport_object_id = object_id
        record.member_index = index
        record.object_key = transport_object.object_key
        transport_object.files.append(record)

    results = verify_transport_object(MemoryStorage(raw.getvalue()), transport_object)
    assert [(item.relative_path, size) for item, size, _ in results] == [
        ("a.txt", 3),
        ("nested/b.txt", 5),
    ]
