# ADR-001: S3 multipart is the transfer protocol

Status: Accepted

Courier uses standard S3 multipart uploads rather than a custom transport. Multipart semantics provide bounded retries, remote part reconciliation, and storage-backend portability. The Registry authorizes transfers but does not proxy file content.

