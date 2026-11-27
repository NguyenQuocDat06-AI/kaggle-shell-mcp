"""Tests for WorkerClient RPC resilience, secret-safe tar sync, and Fail-Closed TUI binary verification."""

from __future__ import annotations

import hashlib
import io
import json
import tarfile
import zipfile
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest

from kaggle_mcp import config, tui_bin
from kaggle_mcp.client import (
    WorkerClient,
    WorkerSessionStoppedError,
    summarize_http_error,
)
from kaggle_mcp.config import ActiveSession, Settings, load_active_session, save_active_session
from kaggle_mcp.sync import pack_tar_gz, unpack_tar_gz


def test_summarize_http_error_sanitizes_cloudflare_530_and_524_html() -> None:
    html_530 = (
        "<!doctype html>\n"
        "<html><head><title>Cloudflare Tunnel error | demo.trycloudflare.com | Cloudflare</title>"
        "</head><body><h1>Origin unavailable</h1></body></html>"
    )
    msg_530 = summarize_http_error("1", 530, html_530)
    assert "\n" not in msg_530
    assert "HTTP 530" in msg_530
    assert "tunnel closed" in msg_530
    assert "<!doctype" not in msg_530.lower()

    msg_524 = summarize_http_error("1", 524, "<html>524 A Timeout Occurred</html>")
    assert "HTTP 524" in msg_524
    assert "100s" in msg_524


def test_refresh_endpoint_auto_clears_stopped_session(
    isolated_state_dir: Path,
    sample_settings: Settings,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    sess = ActiveSession(
        session_id="sess_stop",
        worker_token="tok",
        kernel_ref="alice/kaggle-mcp-worker",
        tunnel_url="https://dead.trycloudflare.com",
        slot="1",
        mode="live",
        accelerator="CPU",
    )
    save_active_session(sess, slot="1")

    monkeypatch.setattr("kaggle_mcp.client.get_kaggle_api", lambda _s: object())
    monkeypatch.setattr(
        "kaggle_mcp.client.fetch_remote_state",
        lambda _a, _s, slot="1": {"session_id": "sess_stop", "status": "stopped"},
    )

    client = WorkerClient(sample_settings, slot="1")
    with pytest.raises(WorkerSessionStoppedError):
        client._refresh_endpoint(sess)
    assert load_active_session("1") is None


def test_client_sends_idempotency_key_and_never_retries_timed_out_post(
    sample_settings: Settings,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    sess = ActiveSession(
        session_id="s1",
        worker_token="tok",
        kernel_ref="alice/kaggle-mcp-worker",
        tunnel_url="https://live.trycloudflare.com",
        slot="1",
    )
    client = WorkerClient(sample_settings, slot="1", session_override=sess)
    seen_headers: list[dict] = []

    def _timeout_request(self: httpx.Client, method: str, url: str, headers: dict | None = None, **kwargs: object):
        seen_headers.append(dict(headers or {}))
        raise httpx.ReadTimeout("timed out after 75s")

    monkeypatch.setattr(httpx.Client, "request", _timeout_request)
    monkeypatch.setattr(client, "_refresh_endpoint", lambda s: s)

    with pytest.raises(RuntimeError, match="timed out after 75s"):
        client._request("POST", "/jobs/spawn", json_body={"command": "python train.py"})

    assert len(seen_headers) == 1
    assert len(seen_headers[0].get("X-Idempotency-Key", "")) == 32


def test_worker_client_fs_404_fallbacks(
    sample_settings: Settings, monkeypatch: pytest.MonkeyPatch
) -> None:
    sess = ActiveSession(
        session_id="s1",
        worker_token="tok",
        kernel_ref="alice/k",
        tunnel_url="https://demo.trycloudflare.com",
        slot="1",
    )
    client = WorkerClient(sample_settings, slot="1", caller="tui", session_override=sess)
    monkeypatch.setattr(client, "_request", lambda *_a, **_kw: (_ for _ in ()).throw(RuntimeError("Worker HTTP 404: Not Found")))

    cmds: list[str] = []

    def _fake_term(cmd: str, timeout_sec: float = 60.0) -> dict:
        cmds.append(cmd)
        if "infolist" in cmd or "'kind'" in cmd:
            return {"exit_code": 0, "output": json.dumps({"kind": "table", "columns": ["a"], "rows": []})}
        if "'matches'" in cmd:
            return {"exit_code": 0, "output": json.dumps({"root": "/kaggle/working", "matches": [], "truncated": False})}
        if "'count'" in cmd and "tarfile" in cmd:
            return {"exit_code": 0, "output": json.dumps({"ok": True, "path": "/kaggle/working/b.tar.gz", "size_bytes": 12, "count": 2})}
        if "du -sb" in cmd:
            return {"exit_code": 0, "output": "1024\t/kaggle/working/data\n"}
        return {"exit_code": 0, "output": "1"}

    monkeypatch.setattr(client, "term_exec", _fake_term)

    assert client.fs_rename("/kaggle/working/a.txt", "/tmp/b.txt")["ok"] is True
    assert client.fs_copy("/kaggle/working/a.txt", "/kaggle/working/c.txt")["ok"] is True
    assert client.fs_du("/kaggle/working")["total_bytes"] == 1024
    assert client.fs_inspect("/kaggle/working/a.csv")["kind"] == "table"
    assert client.fs_extract("/kaggle/working/a.zip")["ok"] is True
    assert client.fs_archive(["/kaggle/working/a.txt"])["ok"] is True
    assert client.fs_find("/kaggle/working", name_glob="*.py")["root"] == "/kaggle/working"
    assert client.proc_kill(12345, "SIGTERM")["pid"] == 12345
    with pytest.raises(PermissionError):
        client.proc_kill(1)
    assert len(cmds) == 8


def test_sync_pack_filters_secrets_and_unpack_enforces_traversal_and_size_caps(
    tmp_path: Path,
) -> None:
    proj = tmp_path / "src_tree" / "my_proj"
    (proj / "models").mkdir(parents=True)
    (proj / "train.py").write_text("print('ok')\n", encoding="utf-8")
    (proj / "models" / "net.py").write_text("x = 1\n", encoding="utf-8")
    (proj / ".env.example").write_text("OPENAI_API_KEY=\n", encoding="utf-8")

    # Ignored build caches, pull folders, and secret files
    (proj / "__pycache__").mkdir()
    (proj / "__pycache__" / "train.cpython-312.pyc").write_bytes(b"\x00\x01")
    (proj / ".git").mkdir()
    (proj / ".git" / "HEAD").write_text("ref: refs/heads/main\n", encoding="utf-8")
    (proj / ".env").write_text("OPENAI_API_KEY=sk-secret\n", encoding="utf-8")
    (proj / ".env.local").write_text("HF_TOKEN=hf_secret\n", encoding="utf-8")
    (proj / "kaggle.json").write_text('{"key":"secret"}', encoding="utf-8")
    (proj / "id_rsa").write_text("PRIVATE KEY", encoding="utf-8")
    (proj / "cert.pem").write_text("CERT", encoding="utf-8")
    (proj / "kaggle_pull").mkdir()
    (proj / "kaggle_pull" / "huge.bin").write_bytes(b"x" * 128)

    archive = pack_tar_gz(proj)
    dest = tmp_path / "remote_working"
    extracted = unpack_tar_gz(archive, dest)

    assert (dest / "my_proj" / "train.py").is_file()
    assert (dest / "my_proj" / "models" / "net.py").is_file()
    assert (dest / "my_proj" / ".env.example").is_file()
    assert "my_proj/train.py" in extracted
    for excluded in ("__pycache__", ".git", ".env", ".env.local", "kaggle.json", "id_rsa", "cert.pem", "kaggle_pull"):
        assert not (dest / "my_proj" / excluded).exists()

    # Single binary file sync & standalone secret refusal
    weights = tmp_path / "weights.bin"
    weights.write_bytes(b"1234567890")
    unpack_tar_gz(pack_tar_gz(weights), tmp_path / "single_out")
    assert (tmp_path / "single_out" / "weights.bin").read_bytes() == b"1234567890"

    with pytest.raises(PermissionError, match="Refused to pack credential file"):
        pack_tar_gz(proj / ".env")
    assert len(pack_tar_gz(proj / ".env", include_secrets=True)) > 0

    # Block `../escape.txt` and sibling-prefix `../sandbox_evil/pwn.txt`
    for bad_name in ("../escape.txt", "../sandbox_evil/pwn.txt"):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tf:
            info = tarfile.TarInfo(name=bad_name)
            info.size = 3
            tf.addfile(info, io.BytesIO(b"bad"))
        with pytest.raises((ValueError, tarfile.TarError)):
            unpack_tar_gz(buf.getvalue(), tmp_path / "sandbox")

    # Enforce `max_bytes` Tar Bomb ceiling
    big_file = tmp_path / "big.bin"
    big_file.write_bytes(b"A" * 2048)
    with pytest.raises(ValueError, match="maximum safe extraction limit"):
        unpack_tar_gz(pack_tar_gz(big_file), tmp_path / "bomb_out", max_bytes=512)


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


def test_tui_bin_platform_mappings_and_fail_closed_sha256(
    isolated_state_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    assert tui_bin.detect_platform_asset("Windows", "AMD64") == (
        "kgsh-tui-windows-x86_64.zip",
        "kgsh-tui.exe",
    )
    assert tui_bin.detect_platform_asset("Linux", "x86_64") == (
        "kgsh-tui-linux-x86_64.tar.gz",
        "kgsh-tui",
    )
    assert tui_bin.detect_platform_asset("Linux", "aarch64") == (
        "kgsh-tui-linux-aarch64.tar.gz",
        "kgsh-tui",
    )
    assert tui_bin.detect_platform_asset("Darwin", "arm64") == (
        "kgsh-tui-macos-arm64.tar.gz",
        "kgsh-tui",
    )
    assert tui_bin.detect_platform_asset("Plan9", "mips") is None

    monkeypatch.setattr(
        tui_bin,
        "detect_platform_asset",
        lambda: ("kgsh-tui-linux-aarch64.tar.gz", "kgsh-tui"),
    )
    archive = _make_targz_bytes("kgsh-tui", b"\x7fELF-fake-binary")
    digest = hashlib.sha256(archive).hexdigest()
    sums = f"{digest}  kgsh-tui-linux-aarch64.tar.gz\n"

    class ValidFallbackClient:
        def __init__(self, **_: object) -> None:
            pass

        def __enter__(self) -> ValidFallbackClient:
            return self

        def __exit__(self, *_: object) -> None:
            return None

        def get(self, url: str) -> SimpleNamespace:
            if "/download/v9.9.9/" in url:
                return SimpleNamespace(status_code=404, content=b"", text="")
            if url.endswith("/releases/latest/download/kgsh-tui-linux-aarch64.tar.gz"):
                return SimpleNamespace(status_code=200, content=archive, text="")
            if url.endswith("/releases/latest/download/SHA256SUMS.txt"):
                return SimpleNamespace(status_code=200, content=sums.encode(), text=sums)
            raise AssertionError(f"Unexpected URL {url}")

    monkeypatch.setattr(tui_bin.httpx, "Client", ValidFallbackClient)
    out = tui_bin.download_tui_binary(version="9.9.9")
    assert out == isolated_state_dir / "bin" / "v9.9.9" / "kgsh-tui"
    assert out.read_bytes() == b"\x7fELF-fake-binary"

    # Corrupt digest, missing entry, or 404 SHA256SUMS.txt MUST raise RuntimeError (Fail-Closed)
    with pytest.raises(RuntimeError, match="Checksum mismatch"):
        tui_bin._verify_sha256(archive, "kgsh-tui-linux-aarch64.tar.gz", "0" * 64 + "  kgsh-tui-linux-aarch64.tar.gz\n")
    with pytest.raises(RuntimeError, match="missing from SHA256SUMS.txt"):
        tui_bin._verify_sha256(archive, "kgsh-tui-linux-aarch64.tar.gz", digest + "  kgsh-tui-windows-x86_64.zip\n")

    class MissingSumsClient(ValidFallbackClient):
        def get(self, url: str) -> SimpleNamespace:
            if url.endswith(".tar.gz"):
                return SimpleNamespace(status_code=200, content=archive, text="")
            return SimpleNamespace(status_code=404, content=b"", text="")

    monkeypatch.setattr(tui_bin.httpx, "Client", MissingSumsClient)
    with pytest.raises(RuntimeError, match="SHA256SUMS.txt"):
        tui_bin.download_tui_binary(version="1.0.1")

    # Zip extraction and cached lookup without network
    zbytes = _make_zip_bytes("bin/kgsh-tui.exe", b"MZ-exe-bytes")
    assert tui_bin._extract_binary_from_archive(zbytes, "a.zip", "kgsh-tui.exe") == b"MZ-exe-bytes"

    monkeypatch.setattr(config, "PROJECT_ROOT", isolated_state_dir / "nonexistent_proj")
    monkeypatch.setattr(tui_bin.shutil, "which", lambda _name: None)
    monkeypatch.setattr(
        tui_bin,
        "detect_platform_asset",
        lambda: ("kgsh-tui-windows-x86_64.zip", "kgsh-tui.exe"),
    )
    cached = isolated_state_dir / "bin" / "v1.0.1" / "kgsh-tui.exe"
    cached.parent.mkdir(parents=True, exist_ok=True)
    cached.write_bytes(b"cached")
    assert tui_bin.find_existing_tui_binary(version="1.0.1") == cached
    assert tui_bin.ensure_tui_binary(auto_download=False, version="1.0.1") == cached
