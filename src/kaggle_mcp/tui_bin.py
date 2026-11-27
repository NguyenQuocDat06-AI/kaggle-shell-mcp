"""Prebuilt Rust TUI (`kgsh-tui`) binary locator and GitHub Release auto-downloader."""

from __future__ import annotations

import hashlib
import io
import os
import platform
import shutil
import stat
import sys
import tarfile
import zipfile
from pathlib import Path

import httpx

from . import __version__, config

DEFAULT_RELEASE_REPO = "NguyenQuocDat06-AI/kaggle-shell-mcp"

PLATFORM_ASSETS: dict[tuple[str, str], tuple[str, str]] = {
    ("windows", "x86_64"): ("kgsh-tui-windows-x86_64.zip", "kgsh-tui.exe"),
    ("windows", "amd64"): ("kgsh-tui-windows-x86_64.zip", "kgsh-tui.exe"),
    ("linux", "x86_64"): ("kgsh-tui-linux-x86_64.tar.gz", "kgsh-tui"),
    ("linux", "amd64"): ("kgsh-tui-linux-x86_64.tar.gz", "kgsh-tui"),
    ("linux", "aarch64"): ("kgsh-tui-linux-aarch64.tar.gz", "kgsh-tui"),
    ("linux", "arm64"): ("kgsh-tui-linux-aarch64.tar.gz", "kgsh-tui"),
    ("darwin", "arm64"): ("kgsh-tui-macos-arm64.tar.gz", "kgsh-tui"),
    ("darwin", "aarch64"): ("kgsh-tui-macos-arm64.tar.gz", "kgsh-tui"),
    ("darwin", "x86_64"): ("kgsh-tui-macos-x86_64.tar.gz", "kgsh-tui"),
    ("darwin", "amd64"): ("kgsh-tui-macos-x86_64.tar.gz", "kgsh-tui"),
}


def detect_platform_asset(
    system: str | None = None,
    machine: str | None = None,
) -> tuple[str, str] | None:
    """Return `(release_asset_name, executable_name)` for the current or given OS/arch."""
    sys_name = (system or platform.system()).strip().lower()
    mach = (machine or platform.machine()).strip().lower()
    return PLATFORM_ASSETS.get((sys_name, mach))


def default_exe_name() -> str:
    pair = detect_platform_asset()
    if pair is not None:
        return pair[1]
    return "kgsh-tui.exe" if sys.platform == "win32" else "kgsh-tui"


def cached_binary_path(version: str = __version__, exe_name: str | None = None) -> Path:
    """Return local cache path `~/.kaggle_mcp/bin/v<version>/<exe_name>`."""
    name = exe_name or default_exe_name()
    return config.STATE_DIR / "bin" / f"v{version}" / name


def find_existing_tui_binary(version: str = __version__) -> Path | None:
    """Locate an already-built/installed/cached `kgsh-tui` executable without hitting network."""
    exe_name = default_exe_name()
    candidates = [
        config.PROJECT_ROOT / "tui" / "target" / "release" / exe_name,
        config.PROJECT_ROOT / "tui" / "target" / "debug" / exe_name,
    ]
    for p in candidates:
        if p.is_file():
            return p

    found = shutil.which(exe_name)
    if found:
        return Path(found)

    cached = cached_binary_path(version=version, exe_name=exe_name)
    if cached.is_file():
        return cached
    return None


def _verify_sha256(data: bytes, asset_name: str, sums_text: str) -> None:
    expected: str | None = None
    for line in sums_text.splitlines():
        parts = line.strip().split()
        if len(parts) >= 2 and Path(parts[-1]).name == asset_name:
            expected = parts[0].lower()
            break
    if expected is None:
        return
    actual = hashlib.sha256(data).hexdigest().lower()
    if actual != expected:
        raise RuntimeError(
            f"Checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        )


def _extract_binary_from_archive(data: bytes, asset_name: str, exe_name: str) -> bytes:
    if asset_name.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as zf:
            for info in zf.infolist():
                if not info.is_dir() and Path(info.filename).name == exe_name:
                    raw = zf.read(info)
                    if not raw:
                        raise RuntimeError(f"Empty executable '{exe_name}' inside {asset_name}")
                    return raw
    elif asset_name.endswith(".tar.gz"):
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tf:
            for member in tf.getmembers():
                if member.isfile() and Path(member.name).name == exe_name:
                    fh = tf.extractfile(member)
                    if fh is not None:
                        raw = fh.read()
                        if raw:
                            return raw
    raise RuntimeError(f"Executable '{exe_name}' not found inside archive {asset_name}")


def download_tui_binary(
    version: str = __version__,
    repo: str | None = None,
    timeout: float = 60.0,
) -> Path:
    """Download prebuilt `kgsh-tui` binary from GitHub Releases into `~/.kaggle_mcp/bin/`."""
    pair = detect_platform_asset()
    if pair is None:
        raise RuntimeError(
            f"Unsupported platform {platform.system()}/{platform.machine()} for prebuilt kgsh-tui. "
            "Build from source with: cargo build --release --manifest-path tui/Cargo.toml"
        )
    asset_name, exe_name = pair
    gh_repo = (repo or os.environ.get("KGSH_RELEASE_REPO") or DEFAULT_RELEASE_REPO).strip()
    bases = [
        f"https://github.com/{gh_repo}/releases/download/v{version}",
        f"https://github.com/{gh_repo}/releases/latest/download",
    ]

    dest = cached_binary_path(version=version, exe_name=exe_name)
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = dest.with_name(f".{exe_name}.{os.getpid()}.tmp")

    with httpx.Client(follow_redirects=True, timeout=timeout) as client:
        archive_bytes: bytes | None = None
        used_base: str | None = None
        last_err = ""
        for base in bases:
            url = f"{base}/{asset_name}"
            print(f"[kgsh] Downloading prebuilt TUI ({asset_name}) from {base}...", file=sys.stderr)
            resp = client.get(url)
            if resp.status_code == 200 and resp.content:
                archive_bytes = resp.content
                used_base = base
                break
            last_err = f"HTTP {resp.status_code} ({url})"

        if archive_bytes is None or used_base is None:
            raise RuntimeError(f"Could not download prebuilt kgsh-tui: {last_err}")

        sums_resp = client.get(f"{used_base}/SHA256SUMS.txt")
        if sums_resp.status_code == 200 and sums_resp.text:
            _verify_sha256(archive_bytes, asset_name, sums_resp.text)

    binary = _extract_binary_from_archive(archive_bytes, asset_name, exe_name)
    try:
        tmp_path.write_bytes(binary)
        if os.name != "nt":
            mode = tmp_path.stat().st_mode
            tmp_path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        os.replace(tmp_path, dest)
    except Exception:
        tmp_path.unlink(missing_ok=True)
        raise

    print(f"[kgsh] Installed TUI binary -> {dest}", file=sys.stderr)
    return dest


def ensure_tui_binary(auto_download: bool = True, version: str = __version__) -> Path:
    """Resolve `kgsh-tui` locally or auto-download the matching GitHub Release binary."""
    existing = find_existing_tui_binary(version=version)
    if existing is not None:
        return existing

    disable_flag = (os.environ.get("KGSH_NO_AUTO_DOWNLOAD") or "").strip().lower()
    if not auto_download or disable_flag in {"1", "true", "yes"}:
        raise RuntimeError(
            "kgsh-tui binary not found. Build from source with: "
            "cargo build --release --manifest-path tui/Cargo.toml"
        )

    try:
        return download_tui_binary(version=version)
    except Exception as exc:
        raise RuntimeError(
            f"kgsh-tui binary not found and auto-download failed: {exc}\n"
            "You can build locally instead: cargo build --release --manifest-path tui/Cargo.toml"
        ) from exc
