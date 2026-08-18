from __future__ import annotations

import io
import json
import struct

import pytest
import zstandard

from data_registry.pack import MAGIC, PackFormatError, iter_pack_members


def make_pack(entries: list[tuple[dict, bytes]]) -> bytes:
    stream = io.BytesIO()
    stream.write(MAGIC)
    for header, content in entries:
        encoded = json.dumps(header, separators=(",", ":")).encode()
        stream.write(struct.pack("<I", len(encoded)))
        stream.write(encoded)
        stream.write(struct.pack("<Q", len(content)))
        stream.write(content)
    stream.write(struct.pack("<I", 0))
    return zstandard.ZstdCompressor(level=3).compress(stream.getvalue())


def test_streams_pack_members_without_extracting_an_archive() -> None:
    packed = make_pack(
        [
            (
                {
                    "path": "casts/温度.csv",
                    "size": 5,
                    "digest_algorithm": "sha256",
                    "digest": "a" * 64,
                },
                b"ocean",
            ),
            (
                {
                    "path": "empty.txt",
                    "size": 0,
                    "digest_algorithm": "sha256",
                    "digest": "b" * 64,
                },
                b"",
            ),
        ]
    )
    actual = []
    for member in iter_pack_members(io.BytesIO(packed)):
        actual.append((member.path, member.reader.read()))
    assert actual == [("casts/温度.csv", b"ocean"), ("empty.txt", b"")]


def test_rejects_a_member_consumer_that_does_not_finish_the_member() -> None:
    packed = make_pack(
        [
            (
                {
                    "path": "data.bin",
                    "size": 5,
                    "digest_algorithm": "sha256",
                    "digest": "a" * 64,
                },
                b"ocean",
            )
        ]
    )
    with pytest.raises(PackFormatError, match="not fully consumed"):
        for _member in iter_pack_members(io.BytesIO(packed)):
            pass
