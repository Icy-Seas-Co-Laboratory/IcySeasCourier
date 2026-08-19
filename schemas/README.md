# Shared schemas

`transfer-manifest-v3.schema.json` is the only accepted contract for the immutable logical-source manifest submitted by Courier. Absolute client paths and server transport object keys are intentionally excluded from file provenance.

Canonical manifest hashes use UTF-8 JSON with recursively sorted object keys and no insignificant whitespace. Arrays retain their original order; Courier sorts file entries by preserved relative path before finalization.

Manifest v3 separates logical files from upload objects. Its `transport_objects` may represent standalone files or bounded `ISCPACK1` packs; each logical file points to exactly one object/member position and retains its own digest and provenance.

Every logical-file digest is an explicit `{algorithm, value}` pair supporting
`sha256`, `xxhash3` (128-bit XXH3), and `blake3`. Versions 1 and 2 are rejected.
