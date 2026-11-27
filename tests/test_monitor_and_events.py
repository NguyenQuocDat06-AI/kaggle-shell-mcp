"""Tests for remote EventBus ring buffer, local activity.jsonl, and real-time MCP flow spans."""

from __future__ import annotations

import ast
import threading
import time
from collections import deque
from pathlib import Path

import pytest

from kaggle_mcp import config
from kaggle_mcp.activity import (
    mcp_call_span,
    mcp_flow_mtime_ns,
    read_local_activities,
    read_mcp_flow_events,
    record_local_activity,
)


def _load_worker_event_bus_class():
    worker_file = (
        Path(__file__).resolve().parents[1]
        / "src"
        / "kaggle_mcp"
        / "remote"
        / "worker_template.py"
    )
    tree = ast.parse(worker_file.read_text(encoding="utf-8"))
    ns: dict = {"threading": threading, "deque": deque, "time": time}
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == "EventBus":
            mod = ast.Module(body=[node], type_ignores=[])
            exec(compile(mod, str(worker_file), "exec"), ns)
            return ns["EventBus"]
    raise AssertionError("EventBus class not found in worker_template.py")


def test_remote_event_bus_sequence_and_ring_bound() -> None:
    event_bus_cls = _load_worker_event_bus_class()
    bus = event_bus_cls(maxlen=3)
    bus.push("mcp", "term_exec", "$ ls", "file1.py", exit_code=0, duration_sec=0.02)
    bus.push("tui", "term_exec", "$ pwd", "/kaggle/working", exit_code=0, duration_sec=0.01)

    seq, items = bus.since(0)
    assert seq == 2
    assert len(items) == 2
    assert items[0]["caller"] == "mcp"
    assert items[1]["caller"] == "tui"

    bus.push("mcp", "job_spawn", "train 1")
    bus.push("mcp", "job_spawn", "train 2")
    seq2, delta = bus.since(2)
    assert seq2 == 4
    assert [e["summary"] for e in delta] == ["train 1", "train 2"]


def test_local_activity_log_read_and_slot_filter(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(config, "STATE_DIR", tmp_path)
    t0 = time.time() - 1.0
    record_local_activity("1", "mcp", "boot_start", "Booting slot 1")
    record_local_activity("2", "mcp", "boot_start", "Booting slot 2")

    s1_events = read_local_activities(slot="1", since_ts=t0)
    assert len(s1_events) == 1
    assert s1_events[0]["summary"] == "Booting slot 1"


def test_mcp_call_span_records_start_done_and_error_without_swallowing_exceptions(
    tmp_path: Path, monkeypatch
) -> None:
    monkeypatch.setattr(config, "STATE_DIR", tmp_path)
    t0 = time.time() - 1.0

    with mcp_call_span("term_exec", slot="1", summary="nvidia-smi"):
        pass

    with pytest.raises(RuntimeError, match="boom"):
        with mcp_call_span("job_spawn", slot="2", summary="train.py"):
            raise RuntimeError("boom")

    assert mcp_flow_mtime_ns() > 0
    events = read_mcp_flow_events(since_ts=t0)
    assert [e["phase"] for e in events] == ["start", "done", "start", "error"]
    assert events[1]["tool"] == "term_exec"
    assert events[3]["tool"] == "job_spawn"
    assert "boom" in events[3]["error"]


def test_worker_template_has_pty_live_and_interrupt() -> None:
    worker_file = (
        Path(__file__).resolve().parents[1]
        / "src"
        / "kaggle_mcp"
        / "remote"
        / "worker_template.py"
    )
    src = worker_file.read_text(encoding="utf-8")
    tree = ast.parse(src)
    pty_methods: set[str] = set()
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == "PtySession":
            pty_methods = {n.name for n in node.body if isinstance(n, ast.FunctionDef)}
            break
    assert {"exec_command", "live_snapshot", "interrupt"} <= pty_methods
    assert '"pty_live": STATE.pty_mgr.live_snapshot()' in src
    assert '"/term/interrupt"' in src
