# ADR-007: Deterministic transport packs for small files

## Status

Implemented.

## Context

Uploading thousands of small scientific files as separate S3 objects creates excessive control-plane and multipart overhead. Compression is also less effective when each small file is encoded independently. Logical files must nevertheless retain their own paths, timestamps, sizes, and integrity digests.

## Decision

Courier may group files no larger than 8 MiB into deterministic packs targeting 64 MiB of original content. Candidates are ordered by normalized relative path. Large files remain standalone transport objects.

Each pack is an independently retryable Zstandard frame using the versioned `ISCPACK1` format. It contains a sequence of metadata headers followed by exact logical-file bytes. Headers record the normalized path, size, digest algorithm, and digest. Encoding streams from the source and does not build an uncompressed archive or dataset-sized temporary file.

The immutable manifest remains a logical-file manifest. Manifest v3 maps each logical file to a client-planned transport object and member position; the Registry assigns the opaque storage key. The ingest worker streams every pack, rejects missing, duplicate, reordered, or unexpected members, and verifies each logical file independently.

Manifest v3 is the sole accepted transfer contract. Earlier manifest versions are intentionally rejected as a breaking change.

## Consequences

- Small-file datasets require fewer object-store and authorization operations.
- Zstandard can exploit redundancy across related small files.
- A failed pack is retried as one bounded unit, while other confirmed packs remain reusable.
- Pack parsing becomes security-sensitive and therefore uses strict size bounds and exact member consumption.
- The client and Registry need explicit transport-object persistence separate from logical-file records.
