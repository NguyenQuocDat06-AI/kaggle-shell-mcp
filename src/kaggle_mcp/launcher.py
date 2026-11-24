"""Kaggle API orchestration: kernel bootstrap push, private dataset handshake, output pull."""

from __future__ import annotations

import base64
import contextlib
import json
import secrets
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import httpx

from .config import (
    DRAIN_AT_SEC,
    MAX_LIFETIME_SEC,
    STATE_DIR,
    ActiveSession,
    Settings,
    configure_kaggle_env,
    load_active_session,
    save_active_session,
)


@contextlib.contextmanager
def _protect_stdout():
    """Redirect stdout to stderr during Kaggle SDK calls so MCP stdio stays clean."""
    with contextlib.redirect_stdout(sys.stderr):
        yield


def get_kaggle_api(settings: Settings):
    configure_kaggle_env(settings.kaggle_username, settings.kaggle_secret)
    with _protect_stdout():
        from kaggle.api.kaggle_api_extended import KaggleApi

        api = KaggleApi()
        api.authenticate()
        return api


def get_kernel_status(api: Any, kernel_ref: str) -> dict[str, str]:
    """Return normalized {'status': ..., 'failure_message': ...} for a Kaggle kernel."""
    try:
        with _protect_stdout():
            resp = api.kernels_status(kernel_ref)
        raw_status = getattr(resp, "status", resp)
        name = getattr(raw_status, "name", str(raw_status)).split(".")[-1].upper()
        failure = getattr(resp, "failure_message", "") or ""
        return {"status": name, "failure_message": str(failure)}
    except Exception as exc:
        msg = str(exc)
        low = msg.lower()
        if "404" in msg or "notfound" in low or "not found" in low or "cannot access kernel" in low:
            return {"status": "NOT_CREATED_YET", "failure_message": ""}
        return {"status": "UNKNOWN", "failure_message": msg}


def ensure_state_dataset(api: Any, settings: Settings) -> None:
    """Ensure private handshake dataset <user>/kaggle-mcp-state exists."""
    try:
        with _protect_stdout():
            api.dataset_list_files(settings.state_dataset_ref)
        return
    except Exception:
        pass

    with tempfile.TemporaryDirectory(prefix="kaggle_mcp_init_ds_") as tmp:
        tmp_p = Path(tmp)
        meta = {
            "title": settings.state_dataset_slug,
            "id": settings.state_dataset_ref,
            "licenses": [{"name": "CC0-1.0"}],
        }
        (tmp_p / "dataset-metadata.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
        init_state = {"session_id": "init", "status": "initialized", "updated_at": time.time()}
        (tmp_p / "state.json").write_text(json.dumps(init_state, indent=2), encoding="utf-8")

        with _protect_stdout():
            api.dataset_create_new(
                folder=str(tmp_p),
                public=False,
                quiet=True,
                convert_to_csv=False,
                dir_mode="skip",
            )
    # Give Kaggle backend a brief moment to register the new private dataset slug
    time.sleep(4.0)


def fetch_remote_state(api: Any, settings: Settings) -> dict[str, Any] | None:
    """Download and parse state.json from the private handshake dataset."""
    try:
        with tempfile.TemporaryDirectory(prefix="kaggle_mcp_state_") as tmp:
            with _protect_stdout():
                api.dataset_download_files(
                    settings.state_dataset_ref,
                    path=tmp,
                    force=True,
                    quiet=True,
                    unzip=True,
                )
            state_file = Path(tmp) / "state.json"
            if state_file.is_file():
                return json.loads(state_file.read_text(encoding="utf-8"))
    except Exception:
        return None
    return None


def probe_worker_ping(base_url: str, expected_session_id: str = "") -> dict[str, Any] | None:
    if not base_url:
        return None
    try:
        with httpx.Client(timeout=6.0, follow_redirects=True) as client:
            r = client.get(f"{base_url.rstrip('/')}/ping")
            if r.status_code == 200:
                data = r.json()
                if not expected_session_id or data.get("session_id") == expected_session_id:
                    return data
    except Exception:
        return None
    return None


def build_bootstrap_bundle(
    settings: Settings,
    session_id: str,
    worker_token: str,
    gpu: bool,
    idle_timeout_min: int,
    mount_datasets: list[str] | None,
    chain_previous_output: bool,
) -> Path:
    bundle_dir = STATE_DIR / "build_bundle"
    bundle_dir.mkdir(parents=True, exist_ok=True)

    template_path = Path(__file__).resolve().parent / "remote" / "worker_template.py"
    template_code = template_path.read_text(encoding="utf-8")

    worker_cfg = {
        "session_id": session_id,
        "worker_token": worker_token,
        "kaggle_username": settings.kaggle_username,
        "kaggle_secret": settings.kaggle_secret,
        "state_dataset_ref": settings.state_dataset_ref,
        "ts_authkey": settings.ts_authkey,
        "ts_hostname": settings.ts_hostname,
        "idle_timeout_sec": max(0, int(idle_timeout_min) * 60),
        "max_lifetime_sec": MAX_LIFETIME_SEC,
        "drain_at_sec": DRAIN_AT_SEC,
        "rpc_port": 8765,
    }
    b64_cfg = base64.b64encode(json.dumps(worker_cfg).encode("utf-8")).decode("ascii")
    rendered_worker = template_code.replace("__BOOTSTRAP_CONFIG_B64__", b64_cfg)
    (bundle_dir / "bootstrap_worker.py").write_text(rendered_worker, encoding="utf-8")

    kernel_meta: dict[str, Any] = {
        "id": settings.kernel_ref,
        "title": settings.kernel_slug,
        "code_file": "bootstrap_worker.py",
        "language": "python",
        "kernel_type": "script",
        "is_private": True,
        "enable_gpu": bool(gpu),
        "enable_internet": True,
        "dataset_sources": list(mount_datasets or []),
        "competition_sources": [],
        "kernel_sources": [settings.kernel_ref] if chain_previous_output else [],
    }
    if gpu:
        kernel_meta["machine_shape"] = "NvidiaTeslaT4"

    (bundle_dir / "kernel-metadata.json").write_text(
        json.dumps(kernel_meta, indent=2),
        encoding="utf-8",
    )
    return bundle_dir


def launch_worker(
    settings: Settings,
    gpu: bool = True,
    idle_timeout_min: int = 0,
    mount_datasets: list[str] | None = None,
    chain_previous_output: bool = False,
    wait_ready_sec: int = 240,
    force_new: bool = False,
) -> dict[str, Any]:
    """Start or reuse a 12h headless Kaggle worker and wait until its RPC tunnel is reachable."""
    api = get_kaggle_api(settings)

    # Reuse live worker if already running and responsive
    existing = load_active_session()
    if existing and not force_new:
        ping = probe_worker_ping(existing.base_url, existing.session_id)
        if ping:
            return {
                "reused_existing": True,
                "session_id": existing.session_id,
                "kernel_ref": existing.kernel_ref,
                "endpoint": existing.base_url,
                "uptime_sec": ping.get("uptime_sec"),
            }

    ensure_state_dataset(api, settings)

    session_id = secrets.token_hex(12)
    worker_token = secrets.token_hex(32)
    bundle_dir = build_bootstrap_bundle(
        settings=settings,
        session_id=session_id,
        worker_token=worker_token,
        gpu=gpu,
        idle_timeout_min=idle_timeout_min,
        mount_datasets=mount_datasets,
        chain_previous_output=chain_previous_output,
    )

    with _protect_stdout():
        push_resp = api.kernels_push(
            str(bundle_dir),
            acc="NvidiaTeslaT4" if gpu else None,
        )

    push_err = getattr(push_resp, "error", None)
    if push_err:
        raise RuntimeError(f"Kaggle kernels_push failed: {push_err}")

    deadline = time.monotonic() + max(30, int(wait_ready_sec))
    last_kstatus = "PUSHED"

    while time.monotonic() < deadline:
        time.sleep(6.0)
        kinfo = get_kernel_status(api, settings.kernel_ref)
        last_kstatus = kinfo["status"]

        if last_kstatus in {"ERROR", "CANCELLED"}:
            raise RuntimeError(
                f"Kaggle kernel entered {last_kstatus}: {kinfo.get('failure_message') or 'check Kaggle logs'}"
            )

        rstate = fetch_remote_state(api, settings)
        if not rstate or rstate.get("session_id") != session_id:
            continue

        ts_url = (rstate.get("ts_url") or "").strip()
        tunnel_url = (rstate.get("tunnel_url") or "").strip()
        candidate_urls = [u for u in (ts_url, tunnel_url) if u]

        for url in candidate_urls:
            ping = probe_worker_ping(url, session_id)
            if ping:
                active = ActiveSession(
                    session_id=session_id,
                    worker_token=worker_token,
                    kernel_ref=settings.kernel_ref,
                    state_dataset_ref=settings.state_dataset_ref,
                    tunnel_url=tunnel_url,
                    ts_url=ts_url if url == ts_url else "",
                    created_at=time.time(),
                    idle_timeout_min=idle_timeout_min,
                )
                save_active_session(active)
                return {
                    "reused_existing": False,
                    "session_id": session_id,
                    "kernel_ref": settings.kernel_ref,
                    "kernel_status": last_kstatus,
                    "endpoint": active.base_url,
                    "gpus": rstate.get("gpus", []),
                    "max_session_hours": round(MAX_LIFETIME_SEC / 3600, 2),
                    "idle_timeout_min": idle_timeout_min,
                }

    # Save pending credentials so kaggle_status can attach once queue finishes
    pending = ActiveSession(
        session_id=session_id,
        worker_token=worker_token,
        kernel_ref=settings.kernel_ref,
        state_dataset_ref=settings.state_dataset_ref,
        created_at=time.time(),
        idle_timeout_min=idle_timeout_min,
    )
    save_active_session(pending)
    return {
        "reused_existing": False,
        "ready": False,
        "session_id": session_id,
        "kernel_ref": settings.kernel_ref,
        "kernel_status": last_kstatus,
        "message": (
            f"Kernel đã push (status={last_kstatus}) nhưng chưa nhận được tunnel sau {wait_ready_sec}s "
            "(có thể đang xếp hàng cấp GPU). Gọi kaggle_status sau ít giây để tự kết nối."
        ),
    }


def pull_kernel_outputs(settings: Settings, local_dest: Path) -> list[str]:
    api = get_kaggle_api(settings)
    local_dest = local_dest.expanduser().resolve()
    local_dest.mkdir(parents=True, exist_ok=True)
    with _protect_stdout():
        files, _ = api.kernels_output(
            settings.kernel_ref,
            path=str(local_dest),
            force=True,
            quiet=True,
        )
    return list(files or [])
