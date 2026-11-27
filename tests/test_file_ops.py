"""Unit tests for worker file sandbox, smart inspect, disk usage, and 404 PTY fallbacks."""

from __future__ import annotations

import base64
import json
import struct
import types
import zipfile
from pathlib import Path

import pytest

from kaggle_mcp.client import WorkerClient
from kaggle_mcp.config import ActiveSession, Settings


def _load_worker_module(tmp_path: Path):
    tmpl_path = (
        Path(__file__).resolve().parents[1]
        / "src"
        / "kaggle_mcp"
        / "remote"
        / "worker_template.py"
    )
    src = tmpl_path.read_text(encoding="utf-8")
    cfg = {
        "session_id": "test_sess",
        "worker_token": "tok_test",
        "kaggle_username": "alice",
        "kaggle_secret": "KGAT_dummy",
        "state_dataset_ref": "alice/kaggle-mcp-state",
    }
    b64 = base64.b64encode(json.dumps(cfg).encode("utf-8")).decode("ascii")
    rendered = src.replace("__BOOTSTRAP_CONFIG_B64__", b64)
    # Compile sanity check
    code = compile(rendered, str(tmpl_path), "exec")
    mod = types.ModuleType("worker_runtime_test")
    # Pre-seed WORKING_DIR / LOG_DIR / TMP_DIR safely before executing?
    # Note: top-level mkdir runs once; only execute helpers if posix fcntl/pty exists,
    # otherwise extract pure functions or skip on Windows without fcntl.
    return rendered, code, mod


def test_worker_template_compiles_cleanly(tmp_path: Path) -> None:
    _rendered, code, _mod = _load_worker_module(tmp_path)
    assert code is not None


def _exec_worker_helpers(tmp_path: Path) -> types.ModuleType:
    """Execute pure filesystem/inspect helpers extracted from worker_template.py."""
    tmpl_path = (
        Path(__file__).resolve().parents[1]
        / "src"
        / "kaggle_mcp"
        / "remote"
        / "worker_template.py"
    )
    src = tmpl_path.read_text(encoding="utf-8")
    start_marker = "def safe_extract_tar("
    end_marker = "class FastHTTPServer("
    snippet = src[src.index(start_marker) : src.index(end_marker)]

    mod = types.ModuleType("worker_helpers_test")
    preamble = (
        "import csv, fnmatch, io, json, os, struct, tarfile, time, zipfile\n"
        "from pathlib import Path\n"
        f"WORKING_DIR = Path({str(tmp_path / 'working')!r})\n"
        "WORKING_DIR.mkdir(parents=True, exist_ok=True)\n"
    )
    exec(compile(preamble + snippet, str(tmpl_path), "exec"), mod.__dict__)
    return mod


def test_sandbox_guard_zip_du_and_inspect(tmp_path: Path) -> None:
    mod = _exec_worker_helpers(tmp_path)
    wdir: Path = mod.WORKING_DIR

    ok_path = mod._assert_writable_path(str(wdir / "model" / "weights.pt"))
    assert ok_path == (wdir / "model" / "weights.pt").resolve()

    with pytest.raises(PermissionError, match="sandbox"):
        mod._assert_writable_path("/kaggle/input/comp/train.csv")

    with pytest.raises(PermissionError, match="sandbox root"):
        mod._assert_writable_path(str(wdir))

    # Safe zip extraction blocks path traversal
    bad_zip = wdir / "evil.zip"
    with zipfile.ZipFile(bad_zip, "w") as zf:
        zf.writestr("../../outside.txt", "boom")
    with pytest.raises(ValueError, match="traversal"):
        mod.safe_extract_zip(bad_zip, wdir / "out_evil")

    good_zip = wdir / "ok.zip"
    with zipfile.ZipFile(good_zip, "w") as zf:
        zf.writestr("sub/hello.txt", "world")
    assert mod.safe_extract_zip(good_zip, wdir / "out_ok") == 1
    assert (wdir / "out_ok" / "sub" / "hello.txt").read_text() == "world"

    # Inspect CSV
    csv_p = wdir / "sub.csv"
    csv_p.write_text("id,target\n1,0.5\n2,0.8\n", encoding="utf-8")
    csv_info = mod.inspect_remote_file(csv_p)
    assert csv_info["kind"] == "table"
    assert csv_info["columns"] == ["id", "target"]
    assert csv_info["row_count"] == 2

    # Inspect Archive
    zip_info = mod.inspect_remote_file(good_zip)
    assert zip_info["kind"] == "archive"
    assert zip_info["total_entries"] == 1

    # Inspect Safetensors (header-only)
    st_p = wdir / "model.safetensors"
    hdr_bytes = json.dumps(
        {
            "__metadata__": {"format": "pt"},
            "model.weight": {"dtype": "BF16", "shape": [4, 8], "data_offsets": [0, 64]},
        }
    ).encode("utf-8")
    st_p.write_bytes(struct.pack("<Q", len(hdr_bytes)) + hdr_bytes + b"\x00" * 64)
    st_info = mod.inspect_remote_file(st_p)
    assert st_info["kind"] == "tensors"
    assert st_info["total_tensors"] == 1
    assert st_info["tensors"][0]["dtype"] == "BF16"

    # Compute Disk Usage
    du_info = mod.compute_disk_usage(wdir)
    assert du_info["total_bytes"] > 0
    assert len(du_info["children"]) >= 3


def test_worker_client_new_fs_404_fallbacks(monkeypatch) -> None:
    settings = Settings(kaggle_username="alice", kaggle_secret="KGAT_dummy")
    sess = ActiveSession(
        session_id="s1",
        worker_token="tok",
        kernel_ref="alice/k",
        tunnel_url="https://demo.trycloudflare.com",
        slot="1",
    )
    client = WorkerClient(settings, slot="1", caller="tui", session_override=sess)

    def _raise_404(*_a, **_kw):
        raise RuntimeError("Worker HTTP 404: Not Found")

    monkeypatch.setattr(client, "_request", _raise_404)
    cmds: list[str] = []

    def _fake_term(cmd: str, timeout_sec: float = 60.0):
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
    du = client.fs_du("/kaggle/working")
    assert du["total_bytes"] == 1024
    assert client.fs_inspect("/kaggle/working/a.csv")["kind"] == "table"
    assert client.fs_extract("/kaggle/working/a.zip")["ok"] is True
    assert client.fs_archive(["/kaggle/working/a.txt"])["ok"] is True
    assert client.fs_find("/kaggle/working", name_glob="*.py")["root"] == "/kaggle/working"
    assert client.proc_kill(12345, "SIGTERM")["pid"] == 12345
    with pytest.raises(PermissionError):
        client.proc_kill(1)
    assert len(cmds) == 8
