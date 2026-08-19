import hashlib
import io
import uuid
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

import blake3
import xxhash
import zstandard
from sqlalchemy import or_, select
from sqlalchemy.orm import Session

from .audit import record_event
from .config import Settings
from .models import Transfer, TransferFile, TransferObject, VerificationAttempt
from .pack import PackFormatError, iter_pack_members
from .storage import ObjectStorage


@dataclass(frozen=True)
class VerificationClaim:
    transfer_id: uuid.UUID
    attempt_id: uuid.UUID


class VerificationMismatch(Exception):
    pass


def claim_transfer(database: Session, settings: Settings) -> VerificationClaim | None:
    stale_before = datetime.now(UTC) - timedelta(seconds=settings.verification_lease_seconds)
    transfer = database.scalar(
        select(Transfer)
        .where(
            Transfer.verification_attempt_count < settings.verification_max_attempts,
            or_(
                Transfer.status == "finalizing",
                (Transfer.status == "verifying")
                & (Transfer.verification_started_at < stale_before),
            ),
        )
        .order_by(Transfer.completed_at, Transfer.created_at)
        .with_for_update(skip_locked=True, of=Transfer)
    )
    if transfer is None:
        database.rollback()
        return None
    transfer.status = "verifying"
    transfer.verification_started_at = datetime.now(UTC)
    transfer.verification_attempt_count += 1
    transfer.verification_error = None
    attempt = VerificationAttempt(
        transfer_id=transfer.id,
        attempt_number=transfer.verification_attempt_count,
        status="running",
    )
    database.add(attempt)
    database.flush()
    claim = VerificationClaim(transfer_id=transfer.id, attempt_id=attempt.id)
    database.commit()
    return claim


def verify_claim(
    database: Session,
    storage: ObjectStorage,
    settings: Settings,
    claim: VerificationClaim,
) -> None:
    transfer = database.get(Transfer, claim.transfer_id)
    attempt = database.get(VerificationAttempt, claim.attempt_id)
    if transfer is None or attempt is None:
        raise RuntimeError("verification claim disappeared")
    verified_files = 0
    verified_bytes = 0
    try:
        for transport_object in transfer.transport_objects:
            object_results = verify_transport_object(storage, transport_object)
            for transfer_file, size, digest in object_results:
                transfer_file.verified_size = size
                transfer_file.verified_sha256 = digest
                transfer_file.verified_at = datetime.now(UTC)
                transfer_file.verification_error = None
                transfer_file.status = "verified"
                verified_files += 1
                verified_bytes += size
            transport_object.status = "verified"
        transfer.status = "complete"
        transfer.verified_at = datetime.now(UTC)
        transfer.verification_error = None
        attempt.status = "complete"
        attempt.completed_at = transfer.verified_at
        attempt.verified_file_count = verified_files
        attempt.verified_bytes = verified_bytes
        record_event(
            database,
            actor="worker:ingest-verifier",
            action="transfer.verified",
            object_type="transfer",
            object_id=transfer.public_id,
            metadata={
                "manifest_sha256": transfer.manifest_sha256,
                "file_count": verified_files,
                "original_bytes": verified_bytes,
            },
        )
    except VerificationMismatch as error:
        fail_verification(transfer, attempt, str(error), permanent=True)
    except Exception as error:
        permanent = transfer.verification_attempt_count >= settings.verification_max_attempts
        fail_verification(transfer, attempt, f"object storage error: {error}", permanent)
    database.commit()


def verify_file(storage: ObjectStorage, transfer_file: TransferFile) -> tuple[int, str]:
    body = storage.open_object(transfer_file.object_key)
    reader = body
    if transfer_file.compression == "zstd":
        reader = zstandard.ZstdDecompressor().stream_reader(body)
    elif transfer_file.compression != "none":
        body.close()
        raise VerificationMismatch(
            f"{transfer_file.relative_path}: unsupported compression {transfer_file.compression}"
        )
    algorithm = transfer_file.hash_algorithm or "sha256"
    if algorithm == "sha256":
        digest = hashlib.sha256()
    elif algorithm == "xxhash3":
        digest = xxhash.xxh3_128()
    elif algorithm == "blake3":
        digest = blake3.blake3()
    else:
        raise VerificationMismatch(
            f"{transfer_file.relative_path}: unsupported hash algorithm {algorithm}"
        )
    size = 0
    try:
        while chunk := reader.read(io.DEFAULT_BUFFER_SIZE * 128):
            digest.update(chunk)
            size += len(chunk)
    finally:
        reader.close()
        if reader is not body:
            body.close()
    return size, digest.hexdigest()


def verify_file_against_manifest(
    storage: ObjectStorage, transfer_file: TransferFile
) -> tuple[int, str]:
    size, digest = verify_file(storage, transfer_file)
    if size != transfer_file.original_size:
        raise VerificationMismatch(
            f"{transfer_file.relative_path}: expected "
            f"{transfer_file.original_size} bytes, got {size}"
        )
    if digest != transfer_file.original_sha256:
        label = (
            "SHA-256"
            if (transfer_file.hash_algorithm or "sha256") == "sha256"
            else (transfer_file.hash_algorithm or "sha256").upper()
        )
        raise VerificationMismatch(
            f"{transfer_file.relative_path}: {label} does not match immutable manifest"
        )
    return size, digest


def verify_transport_object(
    storage: ObjectStorage, transport_object: TransferObject
) -> list[tuple[TransferFile, int, str]]:
    files = sorted(transport_object.files, key=lambda item: item.member_index or 0)
    if transport_object.kind == "file":
        if len(files) != 1:
            raise VerificationMismatch("standalone transport object must contain exactly one file")
        size, digest = verify_file_against_manifest(storage, files[0])
        return [(files[0], size, digest)]
    if transport_object.kind != "pack":
        raise VerificationMismatch(f"unsupported transport object kind {transport_object.kind}")

    body = storage.open_object(transport_object.object_key)
    results: list[tuple[TransferFile, int, str]] = []
    try:
        try:
            for index, member in enumerate(iter_pack_members(body)):
                if index >= len(files):
                    raise VerificationMismatch("pack contains an unexpected extra member")
                expected = files[index]
                if (
                    member.path != expected.relative_path
                    or member.size != expected.original_size
                    or member.digest_algorithm != expected.hash_algorithm
                    or member.digest != expected.original_sha256
                ):
                    raise VerificationMismatch(
                        f"{expected.relative_path}: pack member metadata does not match manifest"
                    )
                size, digest = hash_stream(member.reader, expected.hash_algorithm)
                if size != expected.original_size or digest != expected.original_sha256:
                    raise VerificationMismatch(
                        f"{expected.relative_path}: pack content does not match immutable manifest"
                    )
                results.append((expected, size, digest))
        except PackFormatError as error:
            raise VerificationMismatch(f"invalid Courier pack: {error}") from error
    finally:
        body.close()
    if len(results) != len(files):
        raise VerificationMismatch("pack ended before every manifest member was present")
    return results


def hash_stream(reader, algorithm: str) -> tuple[int, str]:
    if algorithm == "sha256":
        digest = hashlib.sha256()
    elif algorithm == "xxhash3":
        digest = xxhash.xxh3_128()
    elif algorithm == "blake3":
        digest = blake3.blake3()
    else:
        raise VerificationMismatch(f"unsupported hash algorithm {algorithm}")
    size = 0
    while chunk := reader.read(io.DEFAULT_BUFFER_SIZE * 128):
        digest.update(chunk)
        size += len(chunk)
    return size, digest.hexdigest()


def fail_verification(
    transfer: Transfer,
    attempt: VerificationAttempt,
    error: str,
    permanent: bool,
) -> None:
    transfer.status = "failed" if permanent else "finalizing"
    transfer.verification_error = error
    attempt.status = "failed" if permanent else "retryable"
    attempt.completed_at = datetime.now(UTC)
    attempt.error = error
    for transfer_file in transfer.files:
        if transfer_file.status != "verified":
            transfer_file.verification_error = error
