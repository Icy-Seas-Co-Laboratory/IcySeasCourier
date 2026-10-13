# Storage operations review

Reviewed 2026-10-12. This is a source review and integration proposal. No server
temperature, disk throughput, SMART, or production process measurements were
available; no runtime storage or deployment configuration was changed.

## lakeFS on the same SeaweedFS instance

Keep Courier's verified incoming representation and add a downstream publication
job. lakeFS can use an S3-compatible backend with a custom endpoint and path-style
addressing. SeaweedFS is a plausible backend, but compatibility with the pinned
SeaweedFS version and selected lakeFS release must be tested, rather than assumed.
Use a dedicated lakeFS bucket or storage namespace and credentials; preserve the
Registry's project authorization and private backend network.
[lakeFS on-premises configuration](https://docs.lakefs.io/admin/install/onprem/)

Courier's manifest already supplies the essential handoff: project, transfer ID,
manifest digest, relative paths, logical sizes/digests, transport membership, and
independent verification evidence. Raw incoming keys represent opaque transport
objects, including compressed packs. Importing that prefix would expose packs,
not the original logical dataset. Existing server retrieval reconstructs and
verifies logical files, but writes a local export; it is a starting point for a
separate publisher, not an existing lakeFS integration.

Recommended publication sequence:

1. Select a Registry `complete` transfer and snapshot its immutable manifest and
   verification evidence. Do not use upload completion or preview availability
   as publication readiness.
2. Use a job keyed by project, transfer ID, manifest digest, and destination. Track
   attempts separately from Courier transfer completion; retries must not publish
   duplicate datasets or change the manifest.
3. Reconstruct safe relative paths from packs and standalone objects. Verify
   sizes and logical digests while streaming to a private lakeFS branch. A direct
   streaming publisher avoids a full local SSD export, though it still reads the
   incoming data and writes the logical representation.
4. Commit only after every expected logical file is validated. Persist repository,
   destination prefix, commit ID, manifest digest, and publication result. Use a
   transfer-specific prefix to avoid collisions between identically named files
   in different uploads. Publish/merge the commit using an explicit policy.
5. Retain incoming objects until publication is confirmed and a retention policy
   permits deletion. Keep download-plan selection aware of the representation;
   never silently point Courier pack downloads at unpacked lakeFS files.

An alternative is to materialize immutable logical files into a separate SeaweedFS
staging prefix, then import that prefix into lakeFS without copying again. Imported
objects remain dependent on their source, so do not mutate or expire them while
lakeFS references them. Import commits the result and replaces entries under its
destination prefix: use a fresh transfer prefix or isolated branch. The source
must remain outside the repository's storage namespace. These constraints require
a deliberate ownership/retention policy.
[lakeFS import guide](https://docs.lakefs.io/guides/import/),
[lakeFS import CLI](https://docs.lakefs.io/reference/cli/oss/v1.82/)

Before implementation, test multipart upload, presigned access, restart/retry,
empty and nested files, packed files, failed digest checks, repeated publication,
project isolation, source retention, and lakeFS commit/rollback on the exact
versions deployed. No immediate upload-protocol change is needed for this design.

## SSD temperature investigation

The reported 1–2 MB/s network rate is not a measurement of backend disk activity.
The following are hypotheses to measure, not established causes:

| Source-reviewed behavior | Potential effect and scope |
| --- | --- |
| `verification.py` streams all transport objects, hashes logical bytes, and decompresses packs after finalization; no throughput throttle exists | Full-speed backend reads and CPU work may overlap another slow upload. Retried verification rereads data. The verifier itself does not create a full local export. |
| `retrieve.py` reconstructs into a local temporary export and atomically publishes it | An invoked export adds both backend reads and local writes; repeated exports can heat a shared SSD independently of upload bandwidth. |
| `verification_lease_seconds` defaults to 300; claims have no heartbeat or completion fencing | Multiple workers can reclaim a healthy verification taking over five minutes, duplicate reads, and race status updates. Default Compose has one worker, so this is principally a scaling/restart correctness concern, not proof of the reported heat source. Keep one worker until claim renewal/fencing is implemented. |
| Worker idle polling defaults to one second; empty claims roll back | Repeated database reads, not an unconditional database write per second. Increase the interval only after measuring its contribution. Compose currently does not expose this setting through an environment substitution. |
| Compose health checks run every 3/5 seconds; admin refresh runs every 15 seconds | Small recurring queries/process launches/access logs; likely secondary unless profiling shows otherwise. |
| Compose has no explicit container log rotation limits | Effective behavior depends on host Docker logging configuration. Error storms or verbose SeaweedFS logging may produce disk writes. Logs require retention/rotation independently of the admin audit view. |
| SeaweedFS, PostgreSQL, logs, and exports may share the same physical drive | Metadata/WAL writes, storage compaction, filesystem behavior, controller power, and nearby CPU heat may dominate. Repository code cannot establish these conditions. |

On the server, record a time series of NVMe SMART temperature sensors, thermal
warning/throttling counters, host per-device I/O, per-process I/O, CPU usage, and
container block I/O. Map SeaweedFS `/data`, PostgreSQL data, Docker logs, and
exports to actual physical drives. Use cumulative-counter deltas, not one snapshot.
Do not include credentials, signed URLs, full Compose environment output, or raw
dataset contents in the diagnostic bundle.

Compare idle, upload-only, verification-only, and export/preview windows using
the same workload. Note transfer states and worker retry counts alongside each
temperature sample. Inspect log growth and SeaweedFS background activity during
those windows. This separates upload bytes from amplified/backend work and thermal
coupling. Then choose measured mitigations: bound verification/publication
concurrency or bandwidth, rotate logs, move exports/metadata where appropriate,
or address drive cooling. Preserve independent verification; disabling it is not
an acceptable performance fix.
