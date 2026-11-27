"""Tests for the stdlib remote worker daemon (`remote/worker_template.py`)."""

from __future__ import annotations

import ast
import base64
import json
import struct
import zipfile
from pathlib import Path

import pytest
from conftest import WORKER_TEMPLATE_PATH, load_worker_symbols


def test_worker_template_compiles_cleanly_and_exposes_pty_routes() -> None:
    src = WORKER_TEMPLATE_PATH.read_text(encoding="utf-8")
    cfg = {
        "session_id": "test_sess",
        "worker_token": "tok_test",
        "kaggle_username": "alice",
        "kaggle_secret": "KGAT_dummy",
        "state_dataset_ref": "alice/kaggle-mcp-state",
    }
    b64 = base64.b64encode(json.dumps(cfg).encode("utf-8")).decode("ascii")
    rendered = src.replace("__BOOTSTRAP_CONFIG_B64__", b64)
    assert "__BOOTSTRAP_CONFIG_B64__" not in rendered
    compile(rendered, str(WORKER_TEMPLATE_PATH), "exec")

    tree = ast.parse(src)
    pty_methods: set[str] = set()
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == "PtySession":
            pty_methods = {n.name for n in node.body if isinstance(n, ast.FunctionDef)}
            break
    assert {"exec_command", "live_snapshot", "interrupt"} <= pty_methods
    assert '"pty_live": STATE.pty_mgr.live_snapshot()' in src
    assert '"pty_sessions_live": STATE.pty_mgr.live_snapshots_by_session()' in src
    assert '"/term/interrupt"' in src


def test_sandbox_guard_archive_inspect_and_disk_usage(tmp_path: Path) -> None:
    wdir = tmp_path / "working"
    rdir = tmp_path / "runtime"
    wdir.mkdir(parents=True, exist_ok=True)
    rdir.mkdir(parents=True, exist_ok=True)

    ns = load_worker_symbols(
        "_assert_no_symlink_parents",
        "safe_extract_tar",
        "safe_extract_tar_file",
        "safe_extract_zip",
        "_assert_writable_path",
        "compute_disk_usage",
        "inspect_remote_file",
        extra_globals={"WORKING_DIR": wdir, "TMP_DIR": rdir},
    )

    ok_path = ns["_assert_writable_path"](str(wdir / "model" / "weights.pt"))
    assert ok_path == (wdir / "model" / "weights.pt").resolve()

    with pytest.raises(PermissionError, match="protected worker runtime dir"):
        ns["_assert_writable_path"](str(rdir / "cloudflared"))
    with pytest.raises(PermissionError, match="sandbox"):
        ns["_assert_writable_path"]("/kaggle/input/comp/train.csv")
    with pytest.raises(PermissionError, match="sandbox root"):
        ns["_assert_writable_path"](str(wdir))

    # Safe zip extraction blocks path traversal
    bad_zip = wdir / "evil.zip"
    with zipfile.ZipFile(bad_zip, "w") as zf:
        zf.writestr("../../outside.txt", "boom")
    with pytest.raises(ValueError, match="traversal"):
        ns["safe_extract_zip"](bad_zip, wdir / "out_evil")

    good_zip = wdir / "ok.zip"
    with zipfile.ZipFile(good_zip, "w") as zf:
        zf.writestr("sub/hello.txt", "world")
    assert ns["safe_extract_zip"](good_zip, wdir / "out_ok") == 1
    assert (wdir / "out_ok" / "sub" / "hello.txt").read_text() == "world"

    # Inspect CSV, Archive, and Safetensors header
    csv_p = wdir / "sub.csv"
    csv_p.write_text("id,target\n1,0.5\n2,0.8\n", encoding="utf-8")
    csv_info = ns["inspect_remote_file"](csv_p)
    assert csv_info["kind"] == "table"
    assert csv_info["columns"] == ["id", "target"]
    assert csv_info["row_count"] == 2

    zip_info = ns["inspect_remote_file"](good_zip)
    assert zip_info["kind"] == "archive" and zip_info["total_entries"] == 1

    st_p = wdir / "model.safetensors"
    hdr_bytes = json.dumps(
        {
            "__metadata__": {"format": "pt"},
            "model.weight": {"dtype": "BF16", "shape": [4, 8], "data_offsets": [0, 64]},
        }
    ).encode("utf-8")
    st_p.write_bytes(struct.pack("<Q", len(hdr_bytes)) + hdr_bytes + b"\x00" * 64)
    st_info = ns["inspect_remote_file"](st_p)
    assert st_info["kind"] == "tensors"
    assert st_info["total_tensors"] == 1
    assert st_info["tensors"][0]["dtype"] == "BF16"

    du_info = ns["compute_disk_usage"](wdir)
    assert du_info["total_bytes"] > 0 and len(du_info["children"]) >= 3


def test_worker_env_scrubbing_pty_isolation_event_bus_and_idempotency(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class DummyPtySession:
        def __init__(self, name: str) -> None:
            self.name = name
            self.interrupted = 0

        def interrupt(self) -> bool:
            self.interrupted += 1
            return self.name == "agent"

        def live_snapshot(self) -> dict:
            return {"running": self.name == "agent", "session": self.name}

        def close(self) -> None:
            pass

    ns = load_worker_symbols(
        "_SCRUBBED_SECRET_ENV_KEYS",
        "_scrubbed_child_env",
        "PtyManager",
        "EventBus",
        "IdempotencyRegistry",
        "clean_tqdm",
        extra_globals={"PtySession": DummyPtySession},
    )

    # Child env scrubs secrets
    monkeypatch.setenv("KAGGLE_API_TOKEN", "KGAT_top_secret")
    monkeypatch.setenv("KAGGLE_KEY", "legacy_secret")
    monkeypatch.setenv("TS_AUTHKEY", "tskey-secret")
    monkeypatch.setenv("KEEP_ME", "visible")
    clean_env = ns["_scrubbed_child_env"]()
    assert "KAGGLE_API_TOKEN" not in clean_env
    assert "KAGGLE_KEY" not in clean_env
    assert "TS_AUTHKEY" not in clean_env
    assert clean_env["KEEP_ME"] == "visible"

    # Interrupting idle "tui" session never broadcasts SIGINT to busy "agent" session
    mgr = ns["PtyManager"]()
    tui_pty = mgr.get("tui")
    agent_pty = mgr.get("agent")
    assert mgr.interrupt("tui") is False
    assert tui_pty.interrupted == 1
    assert agent_pty.interrupted == 0
    assert set(mgr.live_snapshots_by_session().keys()) == {"tui", "agent"}

    # EventBus monotonic sequence & ring bound
    bus = ns["EventBus"](maxlen=3)
    bus.push("mcp", "term_exec", "$ ls", "file1.py", exit_code=0, duration_sec=0.02)
    bus.push("tui", "term_exec", "$ pwd", "/kaggle/working", exit_code=0, duration_sec=0.01)
    seq, items = bus.since(0)
    assert seq == 2 and [e["caller"] for e in items] == ["mcp", "tui"]
    bus.push("mcp", "job_spawn", "train 1")
    bus.push("mcp", "job_spawn", "train 2")
    seq2, delta = bus.since(2)
    assert seq2 == 4 and [e["summary"] for e in delta] == ["train 1", "train 2"]

    # IdempotencyRegistry & clean_tqdm
    idem = ns["IdempotencyRegistry"](maxlen=2, ttl_sec=10.0)
    assert idem.get("k1") is None
    idem.put("k1", 200, {"job_id": "job_123"})
    assert idem.get("k1") == (200, {"job_id": "job_123"})

    noisy = "Epoch 1\r\n10%|#   |\r50%|##### |\r100%|##########| 10/10 [00:02]\r\nloss=0.12"
    assert ns["clean_tqdm"](noisy) == "Epoch 1\n100%|##########| 10/10 [00:02]\nloss=0.12"
