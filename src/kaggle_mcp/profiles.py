"""Boot preset profiles stored locally in STATE_DIR/profiles.json."""

from __future__ import annotations

import json
import os
import threading
from dataclasses import asdict, dataclass, field
from pathlib import Path

from . import config

_PROFILES_LOCK = threading.RLock()


@dataclass
class BootProfile:
    """Reusable worker boot preset for F5 Boot / CLI automation."""

    name: str
    accelerator: str = "NvidiaTeslaT4"
    slots: str = "1"
    datasets: list[str] = field(default_factory=list)
    models: list[str] = field(default_factory=list)
    competition: str = ""
    idle_timeout_min: int = 0
    chain_previous_output: bool = False

    def __post_init__(self) -> None:
        self.name = str(self.name or "").strip()
        self.accelerator = str(self.accelerator or "NvidiaTeslaT4").strip() or "NvidiaTeslaT4"
        self.slots = str(self.slots or "1").strip() or "1"
        self.datasets = [str(x).strip() for x in (self.datasets or []) if str(x).strip()]
        self.models = [str(x).strip() for x in (self.models or []) if str(x).strip()]
        self.competition = str(self.competition or "").strip()
        try:
            self.idle_timeout_min = max(0, int(self.idle_timeout_min))
        except (TypeError, ValueError):
            self.idle_timeout_min = 0
        self.chain_previous_output = bool(self.chain_previous_output)


def profiles_path() -> Path:
    """Return the active profiles.json path derived from config.STATE_DIR."""
    return Path(config.STATE_DIR) / "profiles.json"


def _write_profiles_atomic(items: list[BootProfile]) -> None:
    path = profiles_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    payload = {"profiles": [asdict(p) for p in items]}
    tmp_path.write_text(json.dumps(payload, indent=2, ensure_ascii=False), encoding="utf-8")
    os.replace(tmp_path, path)


def load_profiles() -> list[BootProfile]:
    """Load saved boot profiles from disk; returns empty list on missing or corrupt files."""
    with _PROFILES_LOCK:
        path = profiles_path()
        if not path.is_file():
            return []
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except Exception:
            return []
        rows = raw.get("profiles", []) if isinstance(raw, dict) else (raw if isinstance(raw, list) else [])
        out: list[BootProfile] = []
        seen: set[str] = set()
        for item in rows:
            if not isinstance(item, dict):
                continue
            name = str(item.get("name") or "").strip()
            if not name or name.lower() in seen:
                continue
            seen.add(name.lower())
            out.append(
                BootProfile(
                    name=name,
                    accelerator=item.get("accelerator", "NvidiaTeslaT4"),
                    slots=item.get("slots", "1"),
                    datasets=item.get("datasets", []),
                    models=item.get("models", []),
                    competition=item.get("competition", ""),
                    idle_timeout_min=item.get("idle_timeout_min", 0),
                    chain_previous_output=item.get("chain_previous_output", False),
                )
            )
        return out


def get_profile(name: str) -> BootProfile | None:
    """Look up a saved boot profile by exact (case-insensitive) name."""
    key = (name or "").strip().lower()
    if not key:
        return None
    for prof in load_profiles():
        if prof.name.lower() == key:
            return prof
    return None


def save_profile(profile: BootProfile) -> list[BootProfile]:
    """Create or update a boot profile atomically and return the updated list."""
    clean = BootProfile(
        name=profile.name,
        accelerator=profile.accelerator,
        slots=profile.slots,
        datasets=list(profile.datasets),
        models=list(profile.models),
        competition=profile.competition,
        idle_timeout_min=profile.idle_timeout_min,
        chain_previous_output=profile.chain_previous_output,
    )
    if not clean.name:
        raise ValueError("Profile name cannot be empty.")
    with _PROFILES_LOCK:
        current = load_profiles()
        updated = [p for p in current if p.name.lower() != clean.name.lower()]
        updated.append(clean)
        updated.sort(key=lambda p: p.name.lower())
        _write_profiles_atomic(updated)
        return updated


def delete_profile(name: str) -> list[BootProfile]:
    """Delete a saved boot profile by name (case-insensitive) and return remaining list."""
    key = (name or "").strip().lower()
    with _PROFILES_LOCK:
        current = load_profiles()
        updated = [p for p in current if p.name.lower() != key]
        if len(updated) != len(current):
            _write_profiles_atomic(updated)
        return updated
