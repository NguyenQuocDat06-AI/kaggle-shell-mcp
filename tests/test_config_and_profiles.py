"""Tests for configuration, credentials, multi-slot locking, boot profiles, accelerators, and quota."""

from __future__ import annotations

import json
import os
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
from pathlib import Path
from types import SimpleNamespace

import pytest

from kaggle_mcp import profiles
from kaggle_mcp.accelerators import (
    MAX_GPU_LIFETIME_SEC,
    MAX_TPU_LIFETIME_SEC,
    resolve_accelerator,
)
from kaggle_mcp.config import (
    ActiveSession,
    Settings,
    _read_kaggle_json_fallback,
    clear_active_session,
    configure_kaggle_env,
    load_active_session,
    load_all_sessions,
    normalize_slot,
    save_active_session,
)
from kaggle_mcp.quota import _td_seconds, parse_quota_bucket


def test_configure_kaggle_env_distinguishes_kgat_and_legacy(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KAGGLE_KEY", "old_value")
    monkeypatch.delenv("KAGGLE_API_TOKEN", raising=False)

    configure_kaggle_env("alice", "KGAT_secret123")
    assert os.environ["KAGGLE_USERNAME"] == "alice"
    assert os.environ["KAGGLE_API_TOKEN"] == "KGAT_secret123"
    assert "KAGGLE_KEY" not in os.environ

    configure_kaggle_env("bob", "0123456789abcdef0123456789abcdef")
    assert os.environ["KAGGLE_USERNAME"] == "bob"
    assert os.environ["KAGGLE_KEY"] == "0123456789abcdef0123456789abcdef"


def test_kaggle_json_fallback_for_pypi_installs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("KAGGLE_CONFIG_DIR", str(tmp_path))
    (tmp_path / "kaggle.json").write_text(
        json.dumps({"username": "pypi_user", "key": "KGAT_from_json"}),
        encoding="utf-8",
    )
    user, secret = _read_kaggle_json_fallback()
    assert user == "pypi_user"
    assert secret == "KGAT_from_json"


def test_slot_resource_isolation_and_ephemeral_classification(sample_settings: Settings) -> None:
    assert sample_settings.kernel_ref_for_slot("1") == "alice/kaggle-mcp-worker"
    assert sample_settings.state_dataset_ref_for_slot("1") == "alice/kaggle-mcp-state"
    assert sample_settings.kernel_ref_for_slot("2") == "alice/kaggle-mcp-worker-2"
    assert sample_settings.state_dataset_ref_for_slot("2") == "alice/kaggle-mcp-state-2"
    assert sample_settings.kernel_ref_for_slot("batch-1", mode="batch") == "alice/kaggle-mcp-worker-batch"
    assert sample_settings.kernel_ref_for_slot("comp-1", mode="batch") == "alice/kaggle-mcp-worker-batch-comp-1"

    bref = sample_settings.batch_run_ref("batch-1", "a1b2c3")
    assert bref == "alice/kgsh-batch-1-a1b2c3"
    assert sample_settings.is_ephemeral_managed_kernel(bref) is True
    assert sample_settings.is_ephemeral_managed_kernel("alice/kaggle-mcp-worker-2") is True
    assert sample_settings.is_ephemeral_managed_kernel("alice/my-handcrafted-notebook") is False


def test_invalid_slot_names_rejected() -> None:
    with pytest.raises(ValueError):
        normalize_slot("all")
    with pytest.raises(ValueError):
        normalize_slot("../escape")


def test_concurrent_multi_thread_session_store(isolated_state_dir: Path) -> None:
    def _writer(idx: int) -> None:
        sess = ActiveSession(
            session_id=f"sess_{idx}",
            worker_token=f"tok_{idx}",
            kernel_ref=f"alice/kaggle-mcp-worker-{idx}",
            slot=str(idx),
            status="ready",
        )
        save_active_session(sess, slot=str(idx))

    with ThreadPoolExecutor(max_workers=6) as pool:
        list(pool.map(_writer, range(1, 7)))

    all_s = load_all_sessions()
    assert set(all_s.keys()) == {"1", "2", "3", "4", "5", "6"}
    s2 = load_active_session("2")
    assert s2 is not None and s2.session_id == "sess_2"

    clear_active_session("2")
    assert load_active_session("2") is None
    assert load_active_session("1") is not None

    clear_active_session("all")
    assert load_all_sessions() == {}


def test_legacy_active_session_auto_migration(isolated_state_dir: Path) -> None:
    legacy = isolated_state_dir / "active_session.json"
    legacy.write_text(
        json.dumps({"session_id": "legacy_abc", "worker_token": "tok", "kernel_ref": "u/k"}),
        encoding="utf-8",
    )

    migrated = load_active_session("1")
    assert migrated is not None
    assert migrated.session_id == "legacy_abc"
    assert not legacy.exists()
    assert (isolated_state_dir / "sessions.json").is_file()


def test_profiles_crud_and_atomic_persistence(isolated_state_dir: Path) -> None:
    assert profiles.load_profiles() == []
    assert profiles.get_profile("missing") is None

    p1 = profiles.BootProfile(
        name="t4-lora",
        accelerator="NvidiaTeslaT4",
        slots="1,2",
        datasets=["alice/ds-a", " bob/ds-b "],
        models=["qwen-lm/qwen2.5/transformers/7b"],
        competition="arc-prize-2026",
        idle_timeout_min=30,
        chain_previous_output=True,
    )
    saved = profiles.save_profile(p1)
    assert len(saved) == 1
    assert saved[0].datasets == ["alice/ds-a", "bob/ds-b"]
    assert (isolated_state_dir / "profiles.json").is_file()

    p1_updated = profiles.BootProfile(name="T4-LORA", accelerator="NvidiaL4", idle_timeout_min=45)
    saved2 = profiles.save_profile(p1_updated)
    assert len(saved2) == 1
    loaded = profiles.get_profile("t4-lora")
    assert loaded is not None
    assert loaded.accelerator == "NvidiaL4"
    assert loaded.idle_timeout_min == 45

    p2 = profiles.BootProfile(name="cpu-debug", accelerator="cpu")
    profiles.save_profile(p2)
    assert [p.name for p in profiles.load_profiles()] == ["cpu-debug", "T4-LORA"]

    remaining = profiles.delete_profile("t4-lora")
    assert [p.name for p in remaining] == ["cpu-debug"]


def test_profiles_rejects_empty_name_and_handles_corrupt_json(isolated_state_dir: Path) -> None:
    (isolated_state_dir / "profiles.json").write_text("{not valid json", encoding="utf-8")
    assert profiles.load_profiles() == []

    with pytest.raises(ValueError, match="empty"):
        profiles.save_profile(profiles.BootProfile(name="   "))


def test_accelerator_aliases_and_drain_windows() -> None:
    t4 = resolve_accelerator()
    assert t4.enable_gpu is True
    assert t4.enable_tpu is False
    assert t4.machine_shape == "NvidiaTeslaT4"
    assert t4.max_lifetime_sec == MAX_GPU_LIFETIME_SEC

    l4x4 = resolve_accelerator("l4x4")
    assert l4x4.enable_gpu is True and l4x4.machine_shape == "NvidiaL4x4"

    rtx = resolve_accelerator("rtx-pro-6000")
    assert rtx.enable_gpu is True and rtx.machine_shape == "NvidiaRtxPro6000"

    tpu = resolve_accelerator("tpuv5e8")
    assert tpu.enable_gpu is False and tpu.enable_tpu is True
    assert tpu.machine_shape == "TpuV5E8"
    assert tpu.max_lifetime_sec == MAX_TPU_LIFETIME_SEC

    cpu = resolve_accelerator("cpu")
    assert cpu.enable_gpu is False and cpu.enable_tpu is False and cpu.machine_shape is None

    future = resolve_accelerator("NvidiaB200")
    assert future.enable_gpu is True and future.machine_shape == "NvidiaB200"


def test_timedelta_30h_gpu_quota_and_over_quota_clamping() -> None:
    bucket = SimpleNamespace(
        time_used=timedelta(hours=7, minutes=30),
        time_reserved=timedelta(0),
        total_time_allowed=timedelta(days=1, seconds=21600),
    )
    parsed = parse_quota_bucket(bucket)
    assert parsed["total_hours"] == 30.0
    assert parsed["used_hours"] == 7.5
    assert parsed["remaining_hours"] == 22.5
    assert parsed["used_pct"] == 25.0
    assert parsed["over_quota"] is False

    over = parse_quota_bucket(
        SimpleNamespace(
            time_used=timedelta(hours=35),
            time_reserved=timedelta(0),
            total_time_allowed=timedelta(hours=30),
        )
    )
    assert over["remaining_hours"] == 0.0
    assert over["used_pct"] == 100.0
    assert over["over_quota"] is True
    assert _td_seconds("81802.260441.0s") == 81802.260441
