# Shared schemas

`transfer-manifest-v1.schema.json` is the versioned contract for the immutable logical-source manifest submitted by Courier. Absolute client paths and transport object keys are intentionally excluded from file provenance.

Canonical manifest hashes use UTF-8 JSON with recursively sorted object keys and no insignificant whitespace. Arrays retain their original order; Courier sorts file entries by preserved relative path before finalization.

Manifest v3 separates logical files from upload objects. Its `transport_objects` may represent standalone files or bounded `ISCPACK1` packs; each logical file points to exactly one object/member position and retains its own digest and provenance.

Manifest v2 records each logical-file digest as an explicit `{algorithm, value}`
pair and supports `sha256`, `xxhash3` (128-bit XXH3), and `blake3`. Manifest v1
remains readable as the legacy SHA-256-only format.
