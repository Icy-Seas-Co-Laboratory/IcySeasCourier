import os
import subprocess
import tomllib
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path


def _source_version() -> str:
    project = tomllib.loads((Path(__file__).resolve().parents[1] / "pyproject.toml").read_text())
    return str(project["project"]["version"])


def _git_build() -> str:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short=12", "HEAD"],
            cwd=Path(__file__).resolve().parents[1],
            capture_output=True,
            text=True,
            check=True,
            timeout=2,
        )
    except (OSError, subprocess.SubprocessError):
        return "source"
    return result.stdout.strip() or "source"


try:
    REGISTRY_VERSION = version("icy-seas-data-registry")
except PackageNotFoundError:
    # Keep source checkouts usable before the project is installed as a package.
    REGISTRY_VERSION = _source_version()

try:
    from ._build_id import BUILD_ID as BUILT_BUILD_ID
except ImportError:
    BUILT_BUILD_ID = ""

configured_build = os.environ.get("REGISTRY_BUILD_ID", "").strip()
if configured_build and configured_build != "local":
    REGISTRY_BUILD = configured_build
else:
    REGISTRY_BUILD = BUILT_BUILD_ID or _git_build()
