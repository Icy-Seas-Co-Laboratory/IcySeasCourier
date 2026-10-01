"""Stamp the image with a deterministic fingerprint of its Registry source."""

import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE_FILES = (
    ROOT / "pyproject.toml",
    ROOT / "uv.lock",
    ROOT / "alembic.ini",
    ROOT / "Dockerfile",
    ROOT / ".dockerignore",
)
SOURCE_DIRS = (ROOT / "data_registry", ROOT / "alembic", ROOT / "scripts")
GENERATED = ROOT / "data_registry" / "_build_id.py"


def source_build_id() -> str:
    digest = hashlib.sha256()
    paths = [*SOURCE_FILES, *(path for directory in SOURCE_DIRS for path in directory.rglob("*"))]
    for path in sorted(paths):
        if not path.is_file() or path == GENERATED or "__pycache__" in path.parts:
            continue
        digest.update(path.relative_to(ROOT).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return f"src-{digest.hexdigest()[:12]}"


if __name__ == "__main__":
    GENERATED.write_text(f"BUILD_ID = {source_build_id()!r}\n")
