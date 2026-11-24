"""Configuration, Kaggle token setup, and local session state persistence."""

from __future__ import annotations

import json
import os
from dataclasses import asdict, dataclass
from pathlib import Path

from dotenv import load_dotenv

PROJECT_ROOT = Path(__file__).resolve().parents[2]
STATE_DIR = PROJECT_ROOT / ".kaggle_mcp"
ACTIVE_SESSION_FILE = STATE_DIR / "active_session.json"

MAX_LIFETIME_SEC = 42000  # 11h 40m safe exit before Kaggle 12h hard reaper
DRAIN_AT_SEC = 41700      # 11h 35m SIGTERM checkpoint window
DEFAULT_IDLE_TIMEOUT_MIN = 0  # 0 = disabled by default (opt-in via idle_timeout_min / --idle)
DEFAULT_KERNEL_SLUG = "kaggle-mcp-worker"
DEFAULT_STATE_DATASET_SLUG = "kaggle-mcp-state"


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
        return f"{self.kaggle_username}/{self.kernel_slug}"

    @property
    def state_dataset_ref(self) -> str:
        return f"{self.kaggle_username}/{self.state_dataset_slug}"


@dataclass
class ActiveSession:
    session_id: str
    worker_token: str
    kernel_ref: str
    state_dataset_ref: str
    tunnel_url: str = ""
    ts_url: str = ""
    created_at: float = 0.0
    idle_timeout_min: int = DEFAULT_IDLE_TIMEOUT_MIN

    @property
    def base_url(self) -> str:
        return (self.ts_url or self.tunnel_url).rstrip("/")


def configure_kaggle_env(username: str, secret: str) -> None:
    """Configure environment variables for both KGAT_ bearer tokens and legacy hex keys."""
    os.environ["KAGGLE_USERNAME"] = username
    if secret.startswith("KGAT_"):
        os.environ["KAGGLE_API_TOKEN"] = secret
        # Prevent KaggleApi from misinterpreting a KGAT_ bearer token as a legacy Basic password
        os.environ.pop("KAGGLE_KEY", None)
    else:
        os.environ["KAGGLE_KEY"] = secret


def load_settings() -> Settings:
    """Load credentials from .env (project root or cwd) and prepare Kaggle auth env."""
    env_candidates = [PROJECT_ROOT / ".env", Path.cwd() / ".env"]
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
        raise RuntimeError(
            "Thiếu KAGGLE_USERNAME hoặc KAGGLE_KEY trong file .env "
            f"(đã kiểm tra: {PROJECT_ROOT / '.env'})."
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


def load_active_session() -> ActiveSession | None:
    if not ACTIVE_SESSION_FILE.is_file():
        return None
    try:
        data = json.loads(ACTIVE_SESSION_FILE.read_text(encoding="utf-8"))
        return ActiveSession(**data)
    except Exception:
        return None


def save_active_session(session: ActiveSession) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    tmp_path = ACTIVE_SESSION_FILE.with_suffix(".tmp")
    tmp_path.write_text(json.dumps(asdict(session), indent=2), encoding="utf-8")
    os.replace(tmp_path, ACTIVE_SESSION_FILE)


def clear_active_session() -> None:
    try:
        if ACTIVE_SESSION_FILE.is_file():
            ACTIVE_SESSION_FILE.unlink()
    except OSError:
        pass
