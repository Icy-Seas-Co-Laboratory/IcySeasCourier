# ADR-006: Transfer manifests are immutable provenance records

Status: Accepted

A versioned manifest will describe logical files, preserved relative paths, source metadata, and transport encoding. Final manifests are canonically serialized, hashed, and stored immutably.

