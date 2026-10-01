import os
from importlib.metadata import PackageNotFoundError, version

try:
    REGISTRY_VERSION = version("icy-seas-data-registry")
except PackageNotFoundError:
    # Keep source checkouts usable before the project is installed as a package.
    REGISTRY_VERSION = "0.1.0"

REGISTRY_BUILD = os.environ.get("REGISTRY_BUILD_ID", "local")
