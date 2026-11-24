"""Tests for filtered tar.gz directory/file packing and path-traversal protection."""

from __future__ import annotations

import io
import tarfile
from pathlib import Path

import pytest

from kaggle_mcp.sync import pack_tar_gz, unpack_tar_gz


def test_directory_sync_preserves_top_folder_and_skips_ignored(tmp_path: Path) -> None:
    proj = tmp_path / "src_tree" / "my_proj"
    (proj / "models").mkdir(parents=True)
    (proj / "train.py").write_text("print('ok')\n", encoding="utf-8")
    (proj / "models" / "net.py").write_text("x = 1\n", encoding="utf-8")

    # Ignored artifacts
    (proj / "__pycache__").mkdir()
    (proj / "__pycache__" / "train.cpython-312.pyc").write_bytes(b"\x00\x01")
    (proj / ".git").mkdir()
    (proj / ".git" / "HEAD").write_text("ref: refs/heads/main\n", encoding="utf-8")

    archive = pack_tar_gz(proj)
    dest = tmp_path / "remote_working"
    extracted = unpack_tar_gz(archive, dest)

    assert (dest / "my_proj" / "train.py").is_file()
    assert (dest / "my_proj" / "models" / "net.py").is_file()
    assert not (dest / "my_proj" / "__pycache__").exists()
    assert not (dest / "my_proj" / ".git").exists()
    assert "my_proj/train.py" in extracted


def test_single_file_sync(tmp_path: Path) -> None:
    f = tmp_path / "weights.bin"
    f.write_bytes(b"1234567890")
    dest = tmp_path / "out"
    unpack_tar_gz(pack_tar_gz(f), dest)
    assert (dest / "weights.bin").read_bytes() == b"1234567890"


def test_unpack_blocks_path_traversal(tmp_path: Path) -> None:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        info = tarfile.TarInfo(name="../escape.txt")
        payload = b"bad"
        info.size = len(payload)
        tf.addfile(info, io.BytesIO(payload))

    with pytest.raises((ValueError, tarfile.TarError)):
        unpack_tar_gz(buf.getvalue(), tmp_path / "sandbox")
