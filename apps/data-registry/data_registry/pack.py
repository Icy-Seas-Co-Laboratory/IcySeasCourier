from __future__ import annotations

import json
import struct
from collections.abc import Iterator
from dataclasses import dataclass
from typing import BinaryIO

import zstandard

MAGIC = b"ISCPACK1"
MAXIMUM_HEADER_BYTES = 1024 * 1024


class PackFormatError(ValueError):
    pass


@dataclass(frozen=True)
class PackMember:
    path: str
    size: int
    digest_algorithm: str
    digest: str
    reader: BinaryIO


class _LimitedReader:
    def __init__(self, source: BinaryIO, remaining: int):
        self.source = source
        self.remaining = remaining

    def read(self, size: int = -1) -> bytes:
        if self.remaining == 0:
            return b""
        requested = self.remaining if size < 0 else min(size, self.remaining)
        value = self.source.read(requested)
        if not value:
            raise PackFormatError("pack member ended before its declared size")
        self.remaining -= len(value)
        return value


def iter_pack_members(source: BinaryIO) -> Iterator[PackMember]:
    decoder = zstandard.ZstdDecompressor().stream_reader(source)
    try:
        if _read_exact(decoder, len(MAGIC)) != MAGIC:
            raise PackFormatError("invalid Courier pack magic")
        while True:
            header_size = struct.unpack("<I", _read_exact(decoder, 4))[0]
            if header_size == 0:
                return
            if header_size > MAXIMUM_HEADER_BYTES:
                raise PackFormatError("Courier pack member header is too large")
            try:
                header = json.loads(_read_exact(decoder, header_size))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise PackFormatError("invalid Courier pack member metadata") from error
            content_size = struct.unpack("<Q", _read_exact(decoder, 8))[0]
            required = {"path", "size", "digest_algorithm", "digest"}
            if not isinstance(header, dict) or set(header) != required:
                raise PackFormatError("invalid Courier pack member metadata fields")
            if header["size"] != content_size:
                raise PackFormatError("Courier pack member size does not match metadata")
            reader = _LimitedReader(decoder, content_size)
            yield PackMember(
                path=header["path"],
                size=content_size,
                digest_algorithm=header["digest_algorithm"],
                digest=header["digest"],
                reader=reader,  # type: ignore[arg-type]
            )
            if reader.remaining != 0:
                raise PackFormatError("Courier pack member was not fully consumed")
    finally:
        decoder.close()


def _read_exact(source: BinaryIO, size: int) -> bytes:
    result = bytearray()
    while len(result) < size:
        chunk = source.read(size - len(result))
        if not chunk:
            raise PackFormatError("Courier pack ended unexpectedly")
        result.extend(chunk)
    return bytes(result)
