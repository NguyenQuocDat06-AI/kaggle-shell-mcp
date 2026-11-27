"""Local orchestration activity & real-time MCP packet flow spans (~/.kaggle_mcp/)."""

from __future__ import annotations

import contextlib
import json
import threading
import time
import uuid
from collections.abc import Iterator
from typing import Any

from . import config

_ACT_LOCK = threading.Lock()
_FLOW_LOCK = threading.Lock()
_MAX_BYTES = 512 * 1024
_KEEP_LINES = 200
_FLOW_MAX_BYTES = 128 * 1024
_FLOW_KEEP_LINES = 120


def record_local_activity(
    slot: str,
    caller: str,
    kind: str,
    summary: str,
    detail: str = "",
) -> None:
    """Append a local lifecycle event (e.g. booting, tunnel ready, batch submitted, down)."""
    try:
        state_dir = config.STATE_DIR
        state_dir.mkdir(parents=True, exist_ok=True)
        act_file = state_dir / "activity.jsonl"
        entry = {
            "ts": round(time.time(), 3),
            "slot": str(slot or "1"),
            "caller": str(caller or "mcp"),
            "kind": kind,
            "summary": summary,
            "detail": detail[:600] if detail else "",
        }
        line = json.dumps(entry, ensure_ascii=False) + "\n"
        with _ACT_LOCK:
            with open(act_file, "a", encoding="utf-8") as fh:
                fh.write(line)
            if act_file.stat().st_size > _MAX_BYTES:
                lines = act_file.read_text(encoding="utf-8", errors="replace").splitlines()
                trimmed = "\n".join(lines[-_KEEP_LINES:]) + "\n"
                act_file.write_text(trimmed, encoding="utf-8")
    except Exception:
        pass


def read_local_activities(
    slot: str | None = None,
    since_ts: float = 0.0,
    limit: int = 80,
) -> list[dict[str, Any]]:
    """Read recent local lifecycle events, optionally filtered by slot and timestamp."""
    act_file = config.STATE_DIR / "activity.jsonl"
    if not act_file.is_file():
        return []
    try:
        with _ACT_LOCK:
            raw_lines = act_file.read_text(encoding="utf-8", errors="replace").splitlines()
    except Exception:
        return []

    out: list[dict[str, Any]] = []
    for ln in raw_lines[-max(limit * 3, 200) :]:
        if not ln.strip():
            continue
        try:
            item = json.loads(ln)
        except Exception:
            continue
        if float(item.get("ts", 0.0)) <= since_ts:
            continue
        if slot and slot != "all" and item.get("slot") not in {slot, "all"}:
            continue
        out.append(item)
    return out[-limit:]


def record_mcp_flow_event(
    *,
    span_id: str,
    phase: str,
    tool: str,
    slot: str = "1",
    caller: str = "mcp",
    summary: str = "",
    duration_sec: float | None = None,
    error: str = "",
) -> None:
    """Append a non-blocking MCP pipeline packet event (`start` / `done` / `error`)."""
    try:
        state_dir = config.STATE_DIR
        state_dir.mkdir(parents=True, exist_ok=True)
        flow_file = state_dir / "mcp_flow.jsonl"
        entry: dict[str, Any] = {
            "ts": round(time.time(), 4),
            "span_id": span_id,
            "phase": phase,
            "caller": caller,
            "tool": tool,
            "slot": str(slot or "1"),
            "summary": summary[:160],
        }
        if duration_sec is not None:
            entry["duration_sec"] = round(duration_sec, 3)
        if error:
            entry["error"] = error[:200]
        line = json.dumps(entry, ensure_ascii=False) + "\n"
        with _FLOW_LOCK:
            with open(flow_file, "a", encoding="utf-8") as fh:
                fh.write(line)
            if flow_file.stat().st_size > _FLOW_MAX_BYTES:
                lines = flow_file.read_text(encoding="utf-8", errors="replace").splitlines()
                flow_file.write_text("\n".join(lines[-_FLOW_KEEP_LINES:]) + "\n", encoding="utf-8")
    except Exception:
        pass


def mcp_flow_mtime_ns() -> int:
    """Fast local stat check (~0.01ms) so TUI detects new MCP packets without reading disk when idle."""
    flow_file = config.STATE_DIR / "mcp_flow.jsonl"
    try:
        return flow_file.stat().st_mtime_ns
    except OSError:
        return 0


def read_mcp_flow_events(since_ts: float = 0.0, limit: int = 40) -> list[dict[str, Any]]:
    """Read recent MCP packet flow transitions (`start`, `done`, `error`)."""
    flow_file = config.STATE_DIR / "mcp_flow.jsonl"
    if not flow_file.is_file():
        return []
    try:
        with _FLOW_LOCK:
            raw_lines = flow_file.read_text(encoding="utf-8", errors="replace").splitlines()
    except Exception:
        return []

    out: list[dict[str, Any]] = []
    for ln in raw_lines[-max(limit * 2, 100) :]:
        if not ln.strip():
            continue
        try:
            item = json.loads(ln)
        except Exception:
            continue
        if float(item.get("ts", 0.0)) <= since_ts:
            continue
        out.append(item)
    return out[-limit:]


@contextlib.contextmanager
def mcp_call_span(
    tool: str,
    *,
    slot: str = "1",
    summary: str = "",
    caller: str = "mcp",
) -> Iterator[None]:
    """Passive context manager emitting real-time TX/RX flow events without altering tool behavior."""
    span_id = uuid.uuid4().hex[:10]
    t0 = time.monotonic()
    record_mcp_flow_event(
        span_id=span_id,
        phase="start",
        tool=tool,
        slot=slot,
        caller=caller,
        summary=summary,
    )
    try:
        yield
    except Exception as exc:
        record_mcp_flow_event(
            span_id=span_id,
            phase="error",
            tool=tool,
            slot=slot,
            caller=caller,
            summary=summary,
            duration_sec=time.monotonic() - t0,
            error=f"{type(exc).__name__}: {exc}",
        )
        raise
    else:
        record_mcp_flow_event(
            span_id=span_id,
            phase="done",
            tool=tool,
            slot=slot,
            caller=caller,
            summary=summary,
            duration_sec=time.monotonic() - t0,
        )
