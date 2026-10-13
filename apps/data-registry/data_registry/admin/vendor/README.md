# Browser download dependencies

Vendored, version-pinned browser builds (no runtime CDN requests):

- fzstd 0.1.1 UMD: https://github.com/101arrowz/fzstd/tree/v0.1.1
- hash-wasm 4.12.0 SHA-256, BLAKE3, XXH128 UMD builds: https://github.com/Daninet/hash-wasm

MIT licenses are included alongside the builds. Files were fetched from the exact
versioned npm builds via jsDelivr. XXH128 implements Courier's 128-bit XXH3 policy.
The download wrapper restricts Zstd frame windows to 32 MiB, validates frame and
manifest limits, streams extraction, and verifies the selected logical digest
before saving. Browser builds do not execute remotely supplied scripts.

Validate the wrapper with `node --test tests/admin/download.test.cjs` from the
repository root. See the Registry README for browser and S3 CORS requirements.
