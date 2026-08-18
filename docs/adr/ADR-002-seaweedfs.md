# ADR-002: SeaweedFS is the initial object store

Status: Accepted

SeaweedFS supplies the initial self-hosted S3-compatible data plane. Courier will depend on an object-store interface and S3 behavior, not SeaweedFS-specific APIs.

