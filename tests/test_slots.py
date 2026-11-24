"""Tests for zero-contention multi-slot isolation, file locking, and parallel dual-GPU launch."""

from __future__ import annotations

import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from kaggle_mcp.config import (
    ActiveSession,
    Settings,
    clear_active_session,
    load_active_session,
    load_all_sessions,
    normalize_slot,
    save_active_session,
)
from kaggle_mcp.launcher import launch_workers_multi


def test_slot_resource_isolation_and_backward_compat() -> None:
    s = Settings(kaggle_username="alice", kaggle_secret="KGAT_tok")
    assert s.kernel_ref_for_slot("1") == "alice/kaggle-mcp-worker"
    assert s.state_dataset_ref_for_slot("1") == "alice/kaggle-mcp-state"

    assert s.kernel_ref_for_slot("2") == "alice/kaggle-mcp-worker-2"
    assert s.state_dataset_ref_for_slot("2") == "alice/kaggle-mcp-state-2"
    assert s.kernel_ref_for_slot("batch-1", mode="batch") == "alice/kaggle-mcp-worker-batch"
    assert s.kernel_ref_for_slot("comp-1", mode="batch") == "alice/kaggle-mcp-worker-batch-comp-1"


def test_invalid_slot_names_rejected() -> None:
    with pytest.raises(ValueError):
        normalize_slot("all")
    with pytest.raises(ValueError):
        normalize_slot("../escape")


def test_concurrent_multi_thread_session_store(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr("kaggle_mcp.config.STATE_DIR", tmp_path)

    def _writer(idx: int) -> None:
        sess = ActiveSession(
            session_id=f"sess_{idx}",
            worker_token=f"tok_{idx}",
            kernel_ref=f"alice/kaggle-mcp-worker-{idx}",
            slot=str(idx),
        )
        save_active_session(sess, slot=str(idx))

    with ThreadPoolExecutor(max_workers=6) as pool:
        list(pool.map(_writer, range(1, 7)))

    all_s = load_all_sessions()
    assert set(all_s.keys()) == {"1", "2", "3", "4", "5", "6"}
    assert load_active_session("2").session_id == "sess_2"  # type: ignore[union-attr]

    clear_active_session("2")
    assert load_active_session("2") is None
    assert load_active_session("1") is not None

    clear_active_session("all")
    assert load_all_sessions() == {}


def test_legacy_active_session_auto_migration(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr("kaggle_mcp.config.STATE_DIR", tmp_path)
    legacy = tmp_path / "active_session.json"
    legacy.write_text(
        json.dumps({"session_id": "legacy_abc", "worker_token": "tok", "kernel_ref": "u/k"}),
        encoding="utf-8",
    )

    migrated = load_active_session("1")
    assert migrated is not None
    assert migrated.session_id == "legacy_abc"
    assert not legacy.exists()
    assert (tmp_path / "sessions.json").is_file()


def test_launch_workers_multi_runs_two_gpu_slots_in_parallel(monkeypatch) -> None:
    calls: list[str] = []

    def _fake_launch(settings, *, slot="1", **kwargs):
        calls.append(slot)
        return {"slot": slot, "kernel_status": "RUNNING", "endpoint": f"https://s{slot}.trycloudflare.com"}

    monkeypatch.setattr("kaggle_mcp.launcher.launch_worker", _fake_launch)
    settings = Settings(kaggle_username="alice", kaggle_secret="KGAT_tok")
    res = launch_workers_multi(settings, slots=["1", "2"], accelerator="t4")

    assert res["multi_slot"] is True
    assert set(res["workers"].keys()) == {"1", "2"}
    assert res["errors"] == {}
