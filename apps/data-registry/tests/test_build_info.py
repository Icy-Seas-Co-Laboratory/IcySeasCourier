import shutil
import subprocess
import sys
from pathlib import Path


def test_image_build_fingerprint_changes_with_packaged_source(tmp_path: Path) -> None:
    root = Path(__file__).resolve().parents[1]
    for name in ("pyproject.toml", "uv.lock", "alembic.ini", "Dockerfile", ".dockerignore"):
        shutil.copyfile(root / name, tmp_path / name)
    (tmp_path / "data_registry").mkdir()
    (tmp_path / "alembic").mkdir()
    (tmp_path / "scripts").mkdir()
    shutil.copyfile(root / "scripts" / "stamp_build.py", tmp_path / "scripts" / "stamp_build.py")
    module = tmp_path / "data_registry" / "example.py"
    module.write_text("VALUE = 1\n")

    def stamp() -> str:
        subprocess.run(
            [sys.executable, str(tmp_path / "scripts" / "stamp_build.py")],
            check=True,
            capture_output=True,
        )
        return (tmp_path / "data_registry" / "_build_id.py").read_text()

    first = stamp()
    assert first.startswith("BUILD_ID = 'src-")
    assert stamp() == first
    module.write_text("VALUE = 2\n")
    assert stamp() != first
