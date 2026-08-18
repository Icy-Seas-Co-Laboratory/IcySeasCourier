import hashlib
import io
import uuid

import blake3
import pytest
import xxhash

from data_registry.models import TransferFile
from data_registry.verification import (
    VerificationMismatch,
    verify_file,
    verify_file_against_manifest,
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
