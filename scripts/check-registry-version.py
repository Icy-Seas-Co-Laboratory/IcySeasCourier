"""Require a Registry version increase when its deployable code changes."""

import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROJECT = "apps/data-registry/pyproject.toml"
DEPLOYABLE_DIRS = (
    "apps/data-registry/data_registry/",
    "apps/data-registry/alembic/",
    "apps/data-registry/scripts/",
)
DEPLOYABLE_FILES = {
    "apps/data-registry/.dockerignore",
    "apps/data-registry/Dockerfile",
    "apps/data-registry/alembic.ini",
    "apps/data-registry/pyproject.toml",
    "apps/data-registry/uv.lock",
}


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


def version(project: str) -> tuple[int, int, int]:
    value = tomllib.loads(project)["project"]["version"]
    parts = tuple(int(part) for part in value.split("."))
    if len(parts) != 3:
        raise ValueError(f"Registry version must use major.minor.patch: {value}")
    return parts


def main(base: str) -> None:
    if not base or set(base) == {"0"}:
        print("No base commit; skipping Registry version comparison")
        return
    changed = git("diff", "--name-only", base, "HEAD", "--", "apps/data-registry").splitlines()
    if not any(path in DEPLOYABLE_FILES or path.startswith(DEPLOYABLE_DIRS) for path in changed):
        print("No deployable Registry changes")
        return
    previous = version(git("show", f"{base}:{PROJECT}"))
    current = version((ROOT / PROJECT).read_text())
    if current <= previous:
        raise SystemExit(
            f"Registry deployable code changed; bump {PROJECT} above "
            f"{'.'.join(map(str, previous))} (currently {'.'.join(map(str, current))})"
        )
    print(f"Registry version increased: {previous} -> {current}")


if __name__ == "__main__":
    main(sys.argv[1])
