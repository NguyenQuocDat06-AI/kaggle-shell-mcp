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
    "kaggle_pull",
    "kaggle_outputs",
    "kaggle_downloads",
    "wandb",
    "target",
}
IGNORE_SUFFIXES = {".pyc", ".pyo"}
SECRET_FILENAMES = {
    ".env",
    "kaggle.json",
    "access_token",
    ".netrc",
    "id_rsa",
    "id_ed25519",
}
SECRET_SUFFIXES = {".pem", ".key", ".p12", ".pfx"}
SAFE_ENV_TEMPLATES = {".env.example", ".env.sample", ".env.template"}
MAX_EXTRACT_BYTES = 2 * 1024 * 1024 * 1024  # 2 GiB Tar Bomb ceiling


def is_secret_file(path: Path) -> bool:
    """Return True if `path` looks like a local secret/credential file that must not sync by default."""
    name_low = path.name.lower()
    if name_low in SAFE_ENV_TEMPLATES:
        return False
    if name_low in SECRET_FILENAMES:
        return True
    if name_low.startswith(".env."):
        return True
    if path.suffix.lower() in SECRET_SUFFIXES:
        return True
    return False


def _should_skip(path: Path, *, include_secrets: bool = False) -> bool:
    if path.name in IGNORE_DIRS:
        return True
    if path.suffix in IGNORE_SUFFIXES:
        return True
    if not include_secrets and is_secret_file(path):
        return True
    return False


def _ensure_no_symlink_escape(dest_root: Path, rel_path: str) -> Path:
    """Validate every path component under `dest_root` without following intermediate symlinks."""
    norm_parts = Path(rel_path).parts
    for p in norm_parts:
        if p in {"..", "/", "\\"} or ":" in p:
            raise ValueError(f"Blocked path traversal in downloaded archive: {rel_path}")
    cur = dest_root
    for part in norm_parts[:-1]:
        if part in {"", "."}:
            continue
        cur = cur / part
        if cur.is_symlink():
            raise ValueError(f"Blocked extraction through symlink directory: {cur}")
    leaf = cur / norm_parts[-1] if norm_parts else dest_root
    if leaf != dest_root and leaf.is_symlink():
        raise ValueError(f"Blocked overwrite of existing symlink: {leaf}")
    resolved = leaf.resolve()
    if resolved != dest_root and not resolved.is_relative_to(dest_root):
        raise ValueError(f"Blocked path traversal in downloaded archive: {rel_path}")
    return resolved


def pack_tar_gz(local_path: Path, *, include_secrets: bool = False) -> bytes:
    """Pack a local file or directory into a compressed tar.gz byte buffer."""
    local_path = local_path.expanduser().resolve()
    if not local_path.exists():
        raise FileNotFoundError(f"Local path does not exist: {local_path}")
    if local_path.is_file() and not include_secrets and is_secret_file(local_path):
        raise PermissionError(
            f"Refused to pack credential file '{local_path.name}' by default to prevent secret leakage."
        )

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz", compresslevel=4) as tf:
        if local_path.is_file():
            tf.add(str(local_path), arcname=local_path.name)
        else:
            # Keep the top-level folder name so push(proj -> /kaggle/working) yields /kaggle/working/proj/...
            base = Path(local_path.name)
            for root, dirs, files in os.walk(local_path):
                root_p = Path(root)
                dirs[:] = [d for d in dirs if not _should_skip(root_p / d, include_secrets=include_secrets)]
                rel_dir = base / root_p.relative_to(local_path)
                tf.add(str(root_p), arcname=rel_dir.as_posix(), recursive=False)
                for fname in files:
                    fpath = root_p / fname
                    if _should_skip(fpath, include_secrets=include_secrets) or fpath.is_symlink():
                        continue
                    tf.add(str(fpath), arcname=(rel_dir / fname).as_posix(), recursive=False)
    return buf.getvalue()


def unpack_tar_gz(tar_bytes: bytes, dest_dir: Path, *, max_bytes: int = MAX_EXTRACT_BYTES) -> list[str]:
    """Safely extract tar.gz bytes into dest_dir and return extracted relative names."""
    dest_dir = dest_dir.expanduser().resolve()
    dest_dir.mkdir(parents=True, exist_ok=True)
    extracted: list[str] = []
    total_written = 0

    with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r:gz") as tf:
        for member in tf:
            if member.issym() or member.islnk() or member.ischr() or member.isblk() or member.isfifo():
                continue
            target = _ensure_no_symlink_escape(dest_dir, member.name)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                extracted.append(member.name)
                continue
            if not member.isfile():
                continue
            if member.size > max_bytes or total_written + member.size > max_bytes:
                raise ValueError(f"Archive exceeds maximum safe extraction limit ({max_bytes} bytes).")
            target.parent.mkdir(parents=True, exist_ok=True)
            src = tf.extractfile(member)
            if src is None:
                continue
            with src, open(target, "wb") as out_f:
                while True:
                    chunk = src.read(65536)
                    if not chunk:
                        break
                    total_written += len(chunk)
                    if total_written > max_bytes:
                        out_f.close()
                        target.unlink(missing_ok=True)
                        raise ValueError(f"Archive exceeds maximum safe extraction limit ({max_bytes} bytes).")
                    out_f.write(chunk)
            extracted.append(member.name)
    return extracted
