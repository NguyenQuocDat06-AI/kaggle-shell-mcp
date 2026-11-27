"""Unit tests for BootProfile persistence (`profiles.py`)."""

from __future__ import annotations

from pathlib import Path

import pytest

from kaggle_mcp import config, profiles


def test_profiles_crud_and_atomic_persistence(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(config, "STATE_DIR", tmp_path)

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
    assert (tmp_path / "profiles.json").is_file()

    # Overwrite case-insensitively without duplicate entries
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


def test_profiles_rejects_empty_name_and_handles_corrupt_json(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(config, "STATE_DIR", tmp_path)
    (tmp_path / "profiles.json").write_text("{not valid json", encoding="utf-8")
    assert profiles.load_profiles() == []

    with pytest.raises(ValueError, match="empty"):
        profiles.save_profile(profiles.BootProfile(name="   "))
