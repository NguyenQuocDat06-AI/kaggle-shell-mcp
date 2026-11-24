"""Fast filtered tar.gz archive helpers for syncing local <-> Kaggle files."""

from __future__ import annotations

import io
import os
import tarfile
from pathlib import Path

IGNORE_DIRS = {
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    "node_modules",
    ".kaggle_mcp",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
}
IGNORE_SUFFIXES = {".pyc", ".pyo"}


def _should_skip(path: Path) -> bool:
    if path.name in IGNORE_DIRS:
        return True
    if path.suffix in IGNORE_SUFFIXES:
        return True
    return False


def pack_tar_gz(local_path: Path) -> bytes:
    """Pack a local file or directory into a compressed tar.gz byte buffer."""
    local_path = local_path.expanduser().resolve()
    if not local_path.exists():
        raise FileNotFoundError(f"Local path does not exist: {local_path}")

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        if local_path.is_file():
            tf.add(str(local_path), arcname=local_path.name)
        else:
            # Keep the top-level folder name so push(proj -> /kaggle/working) yields /kaggle/working/proj/...
            base = Path(local_path.name)
            for root, dirs, files in os.walk(local_path):
                root_p = Path(root)
                dirs[:] = [d for d in dirs if not _should_skip(root_p / d)]
                rel_dir = base / root_p.relative_to(local_path)
                tf.add(str(root_p), arcname=rel_dir.as_posix(), recursive=False)
                for fname in files:
                    fpath = root_p / fname
                    if _should_skip(fpath):
                        continue
                    tf.add(str(fpath), arcname=(rel_dir / fname).as_posix(), recursive=False)
    return buf.getvalue()


def unpack_tar_gz(tar_bytes: bytes, dest_dir: Path) -> list[str]:
    """Safely extract tar.gz bytes into dest_dir and return extracted relative names."""
    dest_dir = dest_dir.expanduser().resolve()
    dest_dir.mkdir(parents=True, exist_ok=True)
    extracted: list[str] = []

    with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r:gz") as tf:
        for member in tf.getmembers():
            target = (dest_dir / member.name).resolve()
            if not str(target).startswith(str(dest_dir)):
                raise ValueError(f"Blocked path traversal in downloaded archive: {member.name}")
            if member.issym() or member.islnk():
                continue
            try:
                tf.extract(member, path=dest_dir, filter="data")
            except TypeError:
                tf.extract(member, path=dest_dir)
            extracted.append(member.name)
    return extracted
