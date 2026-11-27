from __future__ import annotations

import hashlib
import io
import tarfile
import zipfile
from pathlib import Path
from types import SimpleNamespace

import pytest

from kaggle_mcp import config, tui_bin


def _make_zip_bytes(filename: str, payload: bytes) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(filename, payload)
    return buf.getvalue()


def _make_targz_bytes(filename: str, payload: bytes) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        info = tarfile.TarInfo(name=filename)
        info.size = len(payload)
        tf.addfile(info, io.BytesIO(payload))
    return buf.getvalue()


def test_detect_platform_asset_mappings() -> None:
    assert tui_bin.detect_platform_asset("Windows", "AMD64") == (
        "kgsh-tui-windows-x86_64.zip",
        "kgsh-tui.exe",
    )
    assert tui_bin.detect_platform_asset("Linux", "x86_64") == (
        "kgsh-tui-linux-x86_64.tar.gz",
        "kgsh-tui",
    )
    assert tui_bin.detect_platform_asset("Darwin", "arm64") == (
        "kgsh-tui-macos-arm64.tar.gz",
        "kgsh-tui",
    )
    assert tui_bin.detect_platform_asset("Plan9", "mips") is None


def test_download_tui_binary_verifies_sha256_and_falls_back_to_latest(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(config, "STATE_DIR", tmp_path)
    monkeypatch.setattr(
        tui_bin,
        "detect_platform_asset",
        lambda: ("kgsh-tui-linux-x86_64.tar.gz", "kgsh-tui"),
    )
    archive = _make_targz_bytes("kgsh-tui", b"\x7fELF-fake-binary")
    digest = hashlib.sha256(archive).hexdigest()
    sums = f"{digest}  kgsh-tui-linux-x86_64.tar.gz\n"

    calls: list[str] = []

    class FakeClient:
        def __init__(self, **_: object) -> None:
            pass

        def __enter__(self) -> FakeClient:
            return self

        def __exit__(self, *_: object) -> None:
            return None

        def get(self, url: str) -> SimpleNamespace:
            calls.append(url)
            if "/download/v9.9.9/kgsh-tui-linux-x86_64.tar.gz" in url:
                return SimpleNamespace(status_code=404, content=b"", text="")
            if url.endswith("/releases/latest/download/kgsh-tui-linux-x86_64.tar.gz"):
                return SimpleNamespace(status_code=200, content=archive, text="")
            if url.endswith("/releases/latest/download/SHA256SUMS.txt"):
                return SimpleNamespace(status_code=200, content=sums.encode(), text=sums)
            raise AssertionError(f"Unexpected URL {url}")

    monkeypatch.setattr(tui_bin.httpx, "Client", FakeClient)

    out = tui_bin.download_tui_binary(version="9.9.9")
    assert out == tmp_path / "bin" / "v9.9.9" / "kgsh-tui"
    assert out.read_bytes() == b"\x7fELF-fake-binary"
    assert len(calls) == 3

    # Corrupt checksum must raise RuntimeError
    bad_sums = "0" * 64 + "  kgsh-tui-linux-x86_64.tar.gz\n"
    with pytest.raises(RuntimeError, match="Checksum mismatch"):
        tui_bin._verify_sha256(archive, "kgsh-tui-linux-x86_64.tar.gz", bad_sums)


def test_extract_zip_and_cached_lookup(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    zbytes = _make_zip_bytes("bin/kgsh-tui.exe", b"MZ-exe-bytes")
    assert tui_bin._extract_binary_from_archive(zbytes, "a.zip", "kgsh-tui.exe") == b"MZ-exe-bytes"

    monkeypatch.setattr(config, "STATE_DIR", tmp_path)
    monkeypatch.setattr(config, "PROJECT_ROOT", tmp_path / "nonexistent_proj")
    monkeypatch.setattr(tui_bin.shutil, "which", lambda _name: None)
    monkeypatch.setattr(
        tui_bin,
        "detect_platform_asset",
        lambda: ("kgsh-tui-windows-x86_64.zip", "kgsh-tui.exe"),
    )
    cached = tmp_path / "bin" / "v1.0.0" / "kgsh-tui.exe"
    cached.parent.mkdir(parents=True, exist_ok=True)
    cached.write_bytes(b"cached")
    assert tui_bin.find_existing_tui_binary(version="1.0.0") == cached
    assert tui_bin.ensure_tui_binary(auto_download=False, version="1.0.0") == cached
