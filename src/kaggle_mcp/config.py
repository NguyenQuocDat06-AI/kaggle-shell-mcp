"""Configuration, Kaggle token setup, and file-locked multi-slot session registry."""

from __future__ import annotations

import contextlib
import json
import os
import re
import threading
import time
from dataclasses import asdict, dataclass, fields
from pathlib import Path

from dotenv import load_dotenv

PROJECT_ROOT = Path(__file__).resolve().parents[2]


def _default_state_dir() -> Path:
    custom = (os.environ.get("KAGGLE_MCP_HOME") or "").strip()
    if custom:
        return Path(custom).expanduser().resolve()
    # Keep state in repo root during editable git development; use ~/.kaggle_mcp when installed from PyPI
    if (PROJECT_ROOT / "pyproject.toml").is_file() and (PROJECT_ROOT / "src" / "kaggle_mcp").is_dir():
        return PROJECT_ROOT / ".kaggle_mcp"
    return Path.home() / ".kaggle_mcp"


STATE_DIR = _default_state_dir()
SESSIONS_FILE = STATE_DIR / "sessions.json"
SESSIONS_LOCK_FILE = STATE_DIR / "sessions.lock"
ACTIVE_SESSION_FILE = STATE_DIR / "active_session.json"  # Legacy v0.1 path (auto-migrated)

MAX_LIFETIME_SEC = 42000  # 11h 40m safe exit before Kaggle 12h hard reaper
DRAIN_AT_SEC = 41700      # 11h 35m SIGTERM checkpoint window
DEFAULT_IDLE_TIMEOUT_MIN = 0  # 0 = disabled by default (opt-in via idle_timeout_min / --idle)
DEFAULT_KERNEL_SLUG = "kaggle-mcp-worker"
DEFAULT_STATE_DATASET_SLUG = "kaggle-mcp-state"

_SLOT_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,19}$")
_PROC_LOCK = threading.RLock()


def normalize_slot(slot: str | None) -> str:
    """Validate and normalize a worker slot identifier (default '1')."""
    s = (slot or "1").strip().lower()
    if not s:
        s = "1"
    if s == "all":
        raise ValueError("Slot name 'all' is reserved for multi-slot queries.")
    if not _SLOT_RE.match(s):
        raise ValueError(
            f"Invalid slot {slot!r}: use 1-20 lowercase alphanumeric or hyphen characters (e.g. '1', '2', 'comp-1')."
        )
    return s


@dataclass
class Settings:
    kaggle_username: str
    kaggle_secret: str
    ts_authkey: str = ""
    ts_hostname: str = "kaggle-mcp-worker"
    kernel_slug: str = DEFAULT_KERNEL_SLUG
    state_dataset_slug: str = DEFAULT_STATE_DATASET_SLUG

    @property
    def kernel_ref(self) -> str:
        return self.kernel_ref_for_slot("1", mode="live")

    @property
    def state_dataset_ref(self) -> str:
        return self.state_dataset_ref_for_slot("1")

    def kernel_slug_for_slot(self, slot: str = "1", mode: str = "live") -> str:
        s = normalize_slot(slot)
        if mode == "batch":
            suffix = s[6:] if s.startswith("batch-") and len(s) > 6 else s
            return f"{self.kernel_slug}-batch" if suffix == "1" else f"{self.kernel_slug}-batch-{suffix}"
        return self.kernel_slug if s == "1" else f"{self.kernel_slug}-{s}"

    def kernel_ref_for_slot(self, slot: str = "1", mode: str = "live") -> str:
        return f"{self.kaggle_username}/{self.kernel_slug_for_slot(slot, mode=mode)}"

    def state_dataset_slug_for_slot(self, slot: str = "1") -> str:
        s = normalize_slot(slot)
        return self.state_dataset_slug if s == "1" else f"{self.state_dataset_slug}-{s}"

    def state_dataset_ref_for_slot(self, slot: str = "1") -> str:
        return f"{self.kaggle_username}/{self.state_dataset_slug_for_slot(slot)}"

    def ts_hostname_for_slot(self, slot: str = "1") -> str:
        s = normalize_slot(slot)
        return self.ts_hostname if s == "1" else f"{self.ts_hostname}-{s}"


@dataclass
class ActiveSession:
    session_id: str
    worker_token: str = ""
    kernel_ref: str = ""
    state_dataset_ref: str = ""
    tunnel_url: str = ""
    ts_url: str = ""
    created_at: float = 0.0
    idle_timeout_min: int = DEFAULT_IDLE_TIMEOUT_MIN
    slot: str = "1"
    mode: str = "live"  # "live" | "batch"
    accelerator: str = "NvidiaTeslaT4"
    version_number: int | None = None
    competition: str = ""
    enable_internet: bool = True

    @property
    def base_url(self) -> str:
        return (self.ts_url or self.tunnel_url).rstrip("/")

    @classmethod
    def from_dict(cls, data: dict) -> ActiveSession:
        known = {f.name for f in fields(cls)}
        filtered = {k: v for k, v in data.items() if k in known}
        return cls(**filtered)


@contextlib.contextmanager
def _locked_state_store(timeout_sec: float = 10.0):
    """Acquire both thread lock and cross-process OS file lock for sessions.json."""
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    lock_file = STATE_DIR / "sessions.lock"
    with _PROC_LOCK:
        fh = open(lock_file, "a+b")
        try:
            fh.seek(0, os.SEEK_END)
            if fh.tell() == 0:
                fh.write(b"\x00")
                fh.flush()
            deadline = time.monotonic() + timeout_sec
            if os.name == "nt":
                import msvcrt

                while True:
                    try:
                        fh.seek(0)
                        msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
                        break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise TimeoutError("Timed out acquiring sessions.lock") from None
                        time.sleep(0.02)
            else:
                import fcntl

                while True:
                    try:
                        fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise TimeoutError("Timed out acquiring sessions.lock") from None
                        time.sleep(0.02)
            yield
        finally:
            try:
                if os.name == "nt":
                    import msvcrt

                    fh.seek(0)
                    msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    import fcntl

                    fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
            fh.close()


def _read_store_unlocked() -> dict[str, ActiveSession]:
    sessions_path = STATE_DIR / "sessions.json"
    legacy_path = STATE_DIR / "active_session.json"
    result: dict[str, ActiveSession] = {}

    if sessions_path.is_file():
        try:
            raw = json.loads(sessions_path.read_text(encoding="utf-8"))
            slots_map = raw.get("slots", raw) if isinstance(raw, dict) else {}
            for slot_key, entry in slots_map.items():
                if isinstance(entry, dict) and entry.get("session_id"):
                    sess = ActiveSession.from_dict(entry)
                    sess.slot = normalize_slot(slot_key)
                    result[sess.slot] = sess
        except Exception:
            result = {}

    if legacy_path.is_file():
        try:
            legacy_raw = json.loads(legacy_path.read_text(encoding="utf-8"))
            if isinstance(legacy_raw, dict) and legacy_raw.get("session_id") and "1" not in result:
                sess = ActiveSession.from_dict(legacy_raw)
                sess.slot = "1"
                result["1"] = sess
                _write_store_unlocked(result)
            legacy_path.unlink(missing_ok=True)
        except Exception:
            pass

    return result


def _write_store_unlocked(slots: dict[str, ActiveSession]) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    sessions_path = STATE_DIR / "sessions.json"
    tmp_path = sessions_path.with_name(f"sessions.{threading.get_ident()}.tmp")
    payload = {"slots": {k: asdict(v) for k, v in sorted(slots.items())}}
    tmp_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    os.replace(tmp_path, sessions_path)


def configure_kaggle_env(username: str, secret: str) -> None:
    """Configure environment variables for both KGAT_ bearer tokens and legacy hex keys."""
    os.environ["KAGGLE_USERNAME"] = username
    if secret.startswith("KGAT_"):
        os.environ["KAGGLE_API_TOKEN"] = secret
        os.environ.pop("KAGGLE_KEY", None)
    else:
        os.environ["KAGGLE_KEY"] = secret


def _read_kaggle_json_fallback() -> tuple[str, str]:
    """Read standard ~/.kaggle/kaggle.json or ~/.kaggle/access_token if env vars are unset."""
    kdir = Path(os.environ.get("KAGGLE_CONFIG_DIR") or (Path.home() / ".kaggle")).expanduser()
    username = ""
    secret = ""

    kjson = kdir / "kaggle.json"
    if kjson.is_file():
        try:
            data = json.loads(kjson.read_text(encoding="utf-8"))
            username = str(data.get("username") or "").strip()
            secret = str(data.get("key") or data.get("token") or "").strip()
        except Exception:
            pass

    atok = kdir / "access_token"
    if not secret and atok.is_file():
        try:
            secret = atok.read_text(encoding="utf-8").strip()
        except Exception:
            pass

    return username, secret


def load_settings() -> Settings:
    """Load credentials from env, .env files, or ~/.kaggle/kaggle.json for PyPI/uvx zero-setup."""
    env_candidates = [
        Path.cwd() / ".env",
        STATE_DIR / ".env",
        PROJECT_ROOT / ".env",
    ]
    for env_path in env_candidates:
        if env_path.is_file():
            load_dotenv(env_path, override=False)

    username = (os.environ.get("KAGGLE_USERNAME") or "").strip()
    secret = (
        os.environ.get("KAGGLE_API_TOKEN")
        or os.environ.get("KAGGLE_KEY")
        or ""
    ).strip()

    if not username or not secret:
        fb_user, fb_secret = _read_kaggle_json_fallback()
        username = username or fb_user
        secret = secret or fb_secret

    if not username or not secret:
        raise RuntimeError(
            "Missing Kaggle credentials. Set KAGGLE_USERNAME & KAGGLE_KEY in MCP env, "
            f"create a .env file ({Path.cwd() / '.env'} or {STATE_DIR / '.env'}), "
            "or place ~/.kaggle/kaggle.json."
        )

    configure_kaggle_env(username, secret)

    return Settings(
        kaggle_username=username,
        kaggle_secret=secret,
        ts_authkey=(os.environ.get("TS_AUTHKEY") or "").strip(),
        ts_hostname=(os.environ.get("TS_HOSTNAME") or "kaggle-mcp-worker").strip(),
        kernel_slug=(os.environ.get("KAGGLE_MCP_KERNEL_SLUG") or DEFAULT_KERNEL_SLUG).strip(),
        state_dataset_slug=(
            os.environ.get("KAGGLE_MCP_STATE_DATASET") or DEFAULT_STATE_DATASET_SLUG
        ).strip(),
    )


def load_all_sessions() -> dict[str, ActiveSession]:
    with _locked_state_store():
        return _read_store_unlocked()


def load_active_session(slot: str = "1") -> ActiveSession | None:
    s = normalize_slot(slot)
    with _locked_state_store():
        return _read_store_unlocked().get(s)


def save_active_session(session: ActiveSession, slot: str | None = None) -> None:
    s = normalize_slot(slot or session.slot)
    session.slot = s
    with _locked_state_store():
        slots = _read_store_unlocked()
        slots[s] = session
        _write_store_unlocked(slots)


def clear_active_session(slot: str = "1") -> None:
    raw = (slot or "1").strip().lower()
    with _locked_state_store():
        if raw == "all":
            _write_store_unlocked({})
            return
        s = normalize_slot(raw)
        slots = _read_store_unlocked()
        if s in slots:
            del slots[s]
            _write_store_unlocked(slots)
