# ADR-007: Deterministic transport packs for small files

## Status

Accepted for incremental implementation.

## Context

Uploading thousands of small scientific files as separate S3 objects creates excessive control-plane and multipart overhead. Compression is also less effective when each small file is encoded independently. Logical files must nevertheless retain their own paths, timestamps, sizes, and integrity digests.

## Decision

Courier may group files no larger than 8 MiB into deterministic packs targeting 128 MiB of original content. Candidates are ordered by normalized relative path. Large files remain standalone transport objects.

Each pack is an independently retryable Zstandard frame using the versioned `ISCPACK1` format. It contains a sequence of metadata headers followed by exact logical-file bytes. Headers record the normalized path, size, digest algorithm, and digest. Encoding streams from the source and does not build an uncompressed archive or dataset-sized temporary file.

The immutable manifest remains a logical-file manifest. Manifest v3 will map each logical file to a server-generated transport object and member position. The ingest worker must stream every pack, reject missing, duplicate, reordered, or unexpected members, and verify each logical file independently.

Existing manifest v1 and v2 transfers remain one-object-per-file and resumable under their original transport plan.

## Consequences

- Small-file datasets require fewer object-store and authorization operations.
- Zstandard can exploit redundancy across related small files.
- A failed pack is retried as one bounded unit, while other confirmed packs remain reusable.
- Pack parsing becomes security-sensitive and therefore uses strict size bounds and exact member consumption.
- The client and Registry need explicit transport-object persistence separate from logical-file records.
