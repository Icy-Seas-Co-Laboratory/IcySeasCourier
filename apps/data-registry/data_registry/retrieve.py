from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import uuid
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import BinaryIO

import blake3
import xxhash
from sqlalchemy import select
from sqlalchemy.orm import Session

from .audit import record_event
from .db import SessionLocal
from .models import Transfer, TransferFile, TransferObject
from .pack import PackFormatError, iter_pack_members
from .storage import ObjectStorage, get_object_storage


class RetrievalError(RuntimeError):
    pass


def _digest(algorithm: str):
    if algorithm == "sha256":
        return hashlib.sha256()
    if algorithm == "xxhash3":
        return xxhash.xxh3_128()
    if algorithm == "blake3":
        return blake3.blake3()
    raise RetrievalError(f"unsupported hash algorithm: {algorithm}")


def _safe_output(root: Path, relative_path: str) -> Path:
    relative = PurePosixPath(relative_path)
    if (
        relative.is_absolute()
        or not relative.parts
        or any(part in {"", ".", ".."} for part in relative.parts)
    ):
        raise RetrievalError(f"unsafe manifest path: {relative_path}")
    return root.joinpath(*relative.parts)


def _write_verified(reader: BinaryIO, record: TransferFile, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    digest = _digest(record.hash_algorithm)
    size = 0
    with destination.open("xb") as output:
        while chunk := reader.read(1024 * 1024):
            output.write(chunk)
            digest.update(chunk)
            size += len(chunk)
    if size != record.original_size or digest.hexdigest() != record.original_sha256:
        destination.unlink(missing_ok=True)
        raise RetrievalError(f"{record.relative_path}: retrieved bytes do not match the manifest")
    timestamp = record.modified_at.timestamp()
    os.utime(destination, (timestamp, timestamp), follow_symlinks=False)


def _retrieve_object(storage: ObjectStorage, item: TransferObject, root: Path) -> int:
    expected = sorted(item.files, key=lambda value: value.member_index or 0)
    body = storage.open_object(item.object_key)
    try:
        if item.kind == "file":
            if len(expected) != 1:
                raise RetrievalError("standalone transport object does not contain one file")
            _write_verified(body, expected[0], _safe_output(root, expected[0].relative_path))
            return 1
        if item.kind != "pack":
            raise RetrievalError(f"unsupported transport object kind: {item.kind}")
        restored = 0
        try:
            for index, member in enumerate(iter_pack_members(body)):
                if index >= len(expected):
                    raise RetrievalError("pack contains an unexpected extra member")
                record = expected[index]
                if (
                    member.path != record.relative_path
                    or member.size != record.original_size
                    or member.digest_algorithm != record.hash_algorithm
                    or member.digest != record.original_sha256
                ):
                    raise RetrievalError(
                        f"{record.relative_path}: pack metadata does not match the manifest"
                    )
                _write_verified(member.reader, record, _safe_output(root, record.relative_path))
                restored += 1
        except PackFormatError as error:
            raise RetrievalError(f"invalid Courier pack: {error}") from error
        if restored != len(expected):
            raise RetrievalError("pack ended before every manifest file was restored")
        return restored
    finally:
        body.close()


def retrieve_transfer_record(transfer: Transfer, storage: ObjectStorage, destination: Path) -> dict:
    if transfer.status != "complete":
        raise RetrievalError("only verified transfers can be retrieved")
    if transfer.manifest is None or transfer.manifest_sha256 is None:
        raise RetrievalError("transfer has no immutable manifest")
    if destination.exists():
        raise RetrievalError(f"destination already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.parent / f".{destination.name}.partial-{uuid.uuid4().hex}"
    data_root = temporary / "data"
    metadata_root = temporary / "courier-metadata"
    temporary.mkdir()
    restored = 0
    try:
        for item in sorted(transfer.transport_objects, key=lambda value: str(value.id)):
            restored += _retrieve_object(storage, item, data_root)
        if restored != transfer.file_count:
            raise RetrievalError(
                f"manifest expected {transfer.file_count} files, but {restored} were restored"
            )
        metadata_root.mkdir()
        (metadata_root / "manifest.json").write_text(
            json.dumps(transfer.manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        receipt = {
            "schema": "icy-seas-courier-retrieval",
            "version": 1,
            "transfer_id": transfer.public_id,
            "manifest_sha256": transfer.manifest_sha256,
            "retrieved_at": datetime.now(UTC).isoformat(),
            "file_count": restored,
            "original_bytes": transfer.original_bytes,
            "transport_bytes": transfer.transport_bytes,
        }
        (metadata_root / "retrieval.json").write_text(
            json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        temporary.replace(destination)
        return receipt
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def retrieve_transfer(
    database: Session, storage: ObjectStorage, transfer_id: str, destination: Path
) -> dict:
    transfer = database.scalar(select(Transfer).where(Transfer.public_id == transfer_id))
    if transfer is None:
        raise RetrievalError(f"transfer not found: {transfer_id}")
    receipt = retrieve_transfer_record(transfer, storage, destination)
    record_event(
        database,
        actor="operator:retrieval-cli",
        action="transfer.retrieved",
        object_type="transfer",
        object_id=transfer.public_id,
        metadata={"destination": str(destination), "file_count": receipt["file_count"]},
    )
    database.commit()
    return receipt


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Reconstruct and verify a completed Courier transfer."
    )
    parser.add_argument("transfer_id", help="Registry transfer ID, such as ISC-TR-ABC123")
    parser.add_argument("destination", type=Path, help="New output directory")
    arguments = parser.parse_args()
    try:
        with SessionLocal() as database:
            receipt = retrieve_transfer(
                database, get_object_storage(), arguments.transfer_id, arguments.destination
            )
    except RetrievalError as error:
        parser.error(str(error))
    print(
        f"Restored {receipt['file_count']} files ({receipt['original_bytes']} original bytes) "
        f"to {arguments.destination}"
    )


if __name__ == "__main__":
    main()
