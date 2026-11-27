"""Kaggle API orchestration: multi-slot kernel bootstrap, accelerator & model mounts, output pull."""

from __future__ import annotations

import base64
import contextlib
import json
import secrets
import shutil
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

from .accelerators import resolve_accelerator
from .activity import record_local_activity
from .config import (
    STATE_DIR,
    ActiveSession,
    Settings,
    clear_active_session,
    configure_kaggle_env,
    ensure_secure_state_dir,
    load_active_session,
    normalize_slot,
    save_active_session,
)

TERMINAL_KERNEL_STATUSES = {
    "COMPLETE",
    "ERROR",
    "CANCELLED",
    "CANCEL_ACKNOWLEDGED",
    "CANCEL_REQUESTED",
}


def strip_kernel_version(kernel_ref: str) -> str:
    """Convert `owner/slug/12` -> `owner/slug` for APIs like `kernels_delete` & `kernels_output`."""
    parts = [p for p in str(kernel_ref or "").strip().split("/") if p]
    if len(parts) >= 2:
        return f"{parts[0]}/{parts[1]}"
    return str(kernel_ref or "").strip()


@contextlib.contextmanager
def protect_stdout():
    """Redirect stdout to stderr during Kaggle SDK calls so MCP stdio stays clean."""
    with contextlib.redirect_stdout(sys.stderr):
        yield


def get_kaggle_api(settings: Settings):
    configure_kaggle_env(settings.kaggle_username, settings.kaggle_secret)
    with protect_stdout():
        from kaggle.api.kaggle_api_extended import KaggleApi

        api = KaggleApi()
        api.authenticate()
        return api


def normalize_str_list(val: str | list[str] | None) -> list[str]:
    if not val:
        return []
    if isinstance(val, str):
        return [s.strip() for s in val.split(",") if s.strip()]
    return [str(s).strip() for s in val if str(s).strip()]


def validate_push_response(push_resp: Any) -> int | None:
    """Validate ApiSaveKernelResponse for errors or invalid dataset/competition/model sources."""
    push_err = getattr(push_resp, "error", None)
    if push_err:
        raise RuntimeError(f"Kaggle kernels_push failed: {push_err}")

    bad_comp = getattr(push_resp, "invalid_competition_sources", None) or []
    bad_ds = getattr(push_resp, "invalid_dataset_sources", None) or []
    bad_mdl = getattr(push_resp, "invalid_model_sources", None) or []
    bad_krn = getattr(push_resp, "invalid_kernel_sources", None) or []

    problems: list[str] = []
    if bad_comp:
        problems.append(
            f"invalid_competition_sources={bad_comp} (check slug & accept competition rules on kaggle.com)"
        )
    if bad_ds:
        problems.append(f"invalid_dataset_sources={bad_ds}")
    if bad_mdl:
        problems.append(f"invalid_model_sources={bad_mdl}")
    if bad_krn:
        problems.append(f"invalid_kernel_sources={bad_krn}")

    if problems:
        raise RuntimeError("Kaggle rejected attached sources: " + "; ".join(problems))

    ver = getattr(push_resp, "version_number", None)
    return int(ver) if ver is not None else None


def get_kernel_status(api: Any, kernel_ref: str) -> dict[str, str]:
    """Return normalized {'status': ..., 'failure_message': ...} for a Kaggle kernel."""
    try:
        with protect_stdout():
            resp = api.kernels_status(strip_kernel_version(kernel_ref))
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


def wait_for_kernel_terminal(api: Any, kernel_ref: str, timeout_sec: float = 60.0) -> str:
    """Poll Kaggle API until kernel reaches a terminal state (e.g. COMPLETE after /shutdown)."""
    deadline = time.monotonic() + max(1.0, float(timeout_sec))
    last_status = "UNKNOWN"
    base_ref = strip_kernel_version(kernel_ref)
    while time.monotonic() < deadline:
        info = get_kernel_status(api, base_ref)
        last_status = info["status"]
        if last_status in TERMINAL_KERNEL_STATUSES or last_status == "NOT_CREATED_YET":
            return last_status
        time.sleep(3.0)
    return last_status


def ensure_state_dataset(api: Any, settings: Settings, slot: str = "1") -> str:
    """Ensure private handshake dataset <user>/kaggle-mcp-state[-<slot>] exists."""
    ds_ref = settings.state_dataset_ref_for_slot(slot)
    ds_slug = settings.state_dataset_slug_for_slot(slot)
    try:
        with protect_stdout():
            api.dataset_list_files(ds_ref)
        return ds_ref
    except Exception:
        pass

    with tempfile.TemporaryDirectory(prefix=f"kaggle_mcp_init_ds_{normalize_slot(slot)}_") as tmp:
        tmp_p = Path(tmp)
        meta = {
            "title": ds_slug,
            "id": ds_ref,
            "licenses": [{"name": "CC0-1.0"}],
        }
        (tmp_p / "dataset-metadata.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
        init_state = {"session_id": "init", "slot": normalize_slot(slot), "status": "initialized", "updated_at": time.time()}
        (tmp_p / "state.json").write_text(json.dumps(init_state, indent=2), encoding="utf-8")

        with protect_stdout():
            api.dataset_create_new(
                folder=str(tmp_p),
                public=False,
                quiet=True,
                convert_to_csv=False,
                dir_mode="skip",
            )
    time.sleep(4.0)
    return ds_ref


def fetch_remote_state(api: Any, settings: Settings, slot: str = "1") -> dict[str, Any] | None:
    """Download and parse state.json from the slot's private handshake dataset."""
    ds_ref = settings.state_dataset_ref_for_slot(slot)
    try:
        with tempfile.TemporaryDirectory(prefix=f"kaggle_mcp_state_{normalize_slot(slot)}_") as tmp:
            with protect_stdout():
                api.dataset_download_files(
                    ds_ref,
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
        from .client import _get_pooled_client

        clean_base = base_url.rstrip("/")
        client = _get_pooled_client(clean_base)
        r = client.get(f"{clean_base}/ping", timeout=6.0)
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
    gpu: bool = True,
    idle_timeout_min: int = 0,
    mount_datasets: list[str] | None = None,
    chain_previous_output: bool = False,
    *,
    slot: str = "1",
    accelerator: str | None = None,
    competition: str | list[str] | None = None,
    mount_models: list[str] | None = None,
) -> Path:
    s = normalize_slot(slot)
    acc_spec = resolve_accelerator(accelerator, gpu=gpu)
    kernel_ref = settings.kernel_ref_for_slot(s, mode="live")
    kernel_slug = settings.kernel_slug_for_slot(s, mode="live")
    state_ds_ref = settings.state_dataset_ref_for_slot(s)

    ensure_secure_state_dir(STATE_DIR)
    bundle_dir = STATE_DIR / f"build_bundle_{s}_{session_id[:6]}"
    bundle_dir.mkdir(parents=True, exist_ok=True)

    template_path = Path(__file__).resolve().parent / "remote" / "worker_template.py"
    template_code = template_path.read_text(encoding="utf-8")

    worker_cfg = {
        "session_id": session_id,
        "slot": s,
        "worker_token": worker_token,
        "kaggle_username": settings.kaggle_username,
        "kaggle_secret": settings.kaggle_secret,
        "state_dataset_ref": state_ds_ref,
        "ts_authkey": settings.ts_authkey,
        "ts_hostname": settings.ts_hostname_for_slot(s),
        "idle_timeout_sec": max(0, int(idle_timeout_min) * 60),
        "max_lifetime_sec": acc_spec.max_lifetime_sec,
        "drain_at_sec": acc_spec.drain_at_sec,
        "rpc_port": 8765,
    }
    b64_cfg = base64.b64encode(json.dumps(worker_cfg).encode("utf-8")).decode("ascii")
    rendered_worker = template_code.replace("__BOOTSTRAP_CONFIG_B64__", b64_cfg)
    (bundle_dir / "bootstrap_worker.py").write_text(rendered_worker, encoding="utf-8")

    comp_list = normalize_str_list(competition)
    ds_list = normalize_str_list(mount_datasets)
    mdl_list = normalize_str_list(mount_models)

    kernel_meta: dict[str, Any] = {
        "id": kernel_ref,
        "title": kernel_slug,
        "code_file": "bootstrap_worker.py",
        "language": "python",
        "kernel_type": "script",
        "is_private": True,
        "enable_gpu": acc_spec.enable_gpu,
        "enable_tpu": acc_spec.enable_tpu,
        "enable_internet": True,
        "dataset_sources": ds_list,
        "competition_sources": comp_list,
        "kernel_sources": [kernel_ref] if chain_previous_output else [],
        "model_sources": mdl_list,
    }
    if acc_spec.machine_shape:
        kernel_meta["machine_shape"] = acc_spec.machine_shape

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
    *,
    slot: str = "1",
    accelerator: str | None = None,
    competition: str | list[str] | None = None,
    mount_models: list[str] | None = None,
) -> dict[str, Any]:
    """Start or reuse a headless Kaggle worker in `slot` (supports parallel multi-slot GPU runs)."""
    s = normalize_slot(slot)
    acc_spec = resolve_accelerator(accelerator, gpu=gpu)
    kernel_ref = settings.kernel_ref_for_slot(s, mode="live")
    state_ds_ref = settings.state_dataset_ref_for_slot(s)
    comp_list = normalize_str_list(competition)
    api = get_kaggle_api(settings)

    session_id = ""
    worker_token = ""
    version_number: int | None = None
    created_at = time.time()
    resumed_provisioning = False

    existing = load_active_session(s)
    if existing and existing.mode == "live" and not force_new:
        if existing.base_url:
            ping = probe_worker_ping(existing.base_url, existing.session_id)
            if ping:
                return {
                    "slot": s,
                    "reused_existing": True,
                    "session_id": existing.session_id,
                    "kernel_ref": existing.kernel_ref,
                    "accelerator": existing.accelerator,
                    "endpoint": existing.base_url,
                    "uptime_sec": ping.get("uptime_sec"),
                }
        elif existing.worker_token and (time.time() - existing.created_at) < 900:
            # Resume waiting for an already-pushed provisioning kernel instead of pushing a duplicate version
            kinfo = get_kernel_status(api, existing.kernel_ref or kernel_ref)
            if kinfo["status"] in {"QUEUED", "RUNNING"}:
                session_id = existing.session_id
                worker_token = existing.worker_token
                version_number = existing.version_number
                created_at = existing.created_at
                resumed_provisioning = True

    if not resumed_provisioning:
        ensure_state_dataset(api, settings, slot=s)

        session_id = secrets.token_hex(12)
        worker_token = secrets.token_hex(32)
        created_at = time.time()
        bundle_dir = build_bootstrap_bundle(
            settings=settings,
            session_id=session_id,
            worker_token=worker_token,
            gpu=gpu,
            idle_timeout_min=idle_timeout_min,
            mount_datasets=mount_datasets,
            chain_previous_output=chain_previous_output,
            slot=s,
            accelerator=accelerator,
            competition=comp_list,
            mount_models=mount_models,
        )
        try:
            record_local_activity(
                s,
                "mcp",
                "boot_start",
                f"Provisioning slot '{s}' ({acc_spec.label}) -> {kernel_ref}",
            )
            with protect_stdout():
                push_resp = api.kernels_push(
                    str(bundle_dir),
                    acc=acc_spec.machine_shape,
                )
            version_number = validate_push_response(push_resp)
        finally:
            # Never leave rendered bootstrap_worker.py containing credentials on local disk
            shutil.rmtree(bundle_dir, ignore_errors=True)

        # Persist worker_token immediately after push so SSH drops or 60s MCP timeouts never orphan the VM
        provisioning_sess = ActiveSession(
            session_id=session_id,
            worker_token=worker_token,
            kernel_ref=kernel_ref,
            state_dataset_ref=state_ds_ref,
            created_at=created_at,
            idle_timeout_min=idle_timeout_min,
            slot=s,
            mode="live",
            accelerator=acc_spec.label,
            version_number=version_number,
            competition=",".join(comp_list),
            enable_internet=True,
            status="provisioning",
        )
        save_active_session(provisioning_sess, slot=s)

    deadline = time.monotonic() + max(30, int(wait_ready_sec))
    last_kstatus = "PUSHED"

    while time.monotonic() < deadline:
        time.sleep(6.0)
        kinfo = get_kernel_status(api, kernel_ref)
        last_kstatus = kinfo["status"]

        if last_kstatus in {"ERROR", "CANCELLED", "CANCEL_ACKNOWLEDGED"}:
            clear_active_session(s)
            record_local_activity(
                s,
                "mcp",
                "boot_error",
                f"Kernel {kernel_ref} entered {last_kstatus}",
                detail=kinfo.get("failure_message", ""),
            )
            raise RuntimeError(
                f"Kaggle kernel ({kernel_ref}) entered {last_kstatus}: "
                f"{kinfo.get('failure_message') or 'check Kaggle logs (note: some competition GPUs force enable_internet=false; use kaggle_batch_run for offline GPUs)'}"
            )

        rstate = fetch_remote_state(api, settings, slot=s)
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
                    kernel_ref=kernel_ref,
                    state_dataset_ref=state_ds_ref,
                    tunnel_url=tunnel_url,
                    ts_url=ts_url if url == ts_url else "",
                    created_at=created_at,
                    idle_timeout_min=idle_timeout_min,
                    slot=s,
                    mode="live",
                    accelerator=acc_spec.label,
                    version_number=version_number,
                    competition=",".join(comp_list),
                    enable_internet=True,
                    status="ready",
                )
                save_active_session(active, slot=s)
                record_local_activity(
                    s,
                    "mcp",
                    "boot_ready",
                    f"Slot '{s}' LIVE ({acc_spec.label})",
                    detail=active.base_url,
                )
                return {
                    "slot": s,
                    "reused_existing": resumed_provisioning,
                    "session_id": session_id,
                    "kernel_ref": kernel_ref,
                    "version_number": version_number,
                    "accelerator": acc_spec.label,
                    "kernel_status": last_kstatus,
                    "endpoint": active.base_url,
                    "gpus": rstate.get("gpus", []),
                    "max_session_hours": round(acc_spec.max_lifetime_sec / 3600, 2),
                    "idle_timeout_min": idle_timeout_min,
                }

    return {
        "slot": s,
        "reused_existing": resumed_provisioning,
        "ready": False,
        "session_id": session_id,
        "kernel_ref": kernel_ref,
        "version_number": version_number,
        "accelerator": acc_spec.label,
        "kernel_status": last_kstatus,
        "message": (
            f"Kernel ({kernel_ref}) đã push (status={last_kstatus}) nhưng chưa nhận được tunnel sau {wait_ready_sec}s "
            "(đang xếp hàng cấp phát phần cứng). Gọi kaggle_status hoặc kaggle_up lại sau ít giây để tự kết nối tiếp."
        ),
    }


def launch_workers_multi(
    settings: Settings,
    slots: list[str],
    **kwargs: Any,
) -> dict[str, Any]:
    """Launch multiple Kaggle worker slots concurrently (e.g. slots=['1', '2'] for 2 parallel GPU VMs)."""
    norm_slots = [normalize_slot(s) for s in slots]
    if len(norm_slots) == 1:
        return launch_worker(settings, slot=norm_slots[0], **kwargs)

    results: dict[str, Any] = {}
    errors: dict[str, str] = {}

    def _run_one(target_slot: str) -> tuple[str, dict[str, Any]]:
        return target_slot, launch_worker(settings, slot=target_slot, **kwargs)

    with ThreadPoolExecutor(max_workers=min(4, len(norm_slots))) as pool:
        futures = {pool.submit(_run_one, s): s for s in norm_slots}
        for fut, s in futures.items():
            try:
                _, res = fut.result()
                results[s] = res
            except Exception as exc:
                errors[s] = f"{type(exc).__name__}: {exc}"

    return {
        "multi_slot": True,
        "requested_slots": norm_slots,
        "workers": results,
        "errors": errors,
        "note": (
            "Đang chạy song song nhiều phiên GPU: mỗi phiên trừ giờ độc lập vào quỹ 30h GPU/tuần."
            if len(results) > 1
            else ""
        ),
    }


def pull_kernel_outputs(
    settings: Settings,
    local_dest: Path,
    slot: str = "1",
    kernel_ref: str = "",
    wait_complete_sec: float = 0.0,
) -> list[str]:
    api = get_kaggle_api(settings)
    s = normalize_slot(slot)
    sess = load_active_session(s)
    raw_ref = kernel_ref or (sess.kernel_ref if sess and sess.kernel_ref else settings.kernel_ref_for_slot(s))
    target_ref = strip_kernel_version(raw_ref)

    if wait_complete_sec > 0:
        wait_for_kernel_terminal(api, target_ref, timeout_sec=wait_complete_sec)

    local_dest = local_dest.expanduser().resolve()
    local_dest.mkdir(parents=True, exist_ok=True)
    with protect_stdout():
        files, _ = api.kernels_output(
            target_ref,
            path=str(local_dest),
            force=True,
            quiet=True,
        )
    return list(files or [])


def shutdown_slot(
    settings: Settings,
    slot: str = "1",
    *,
    pull_outputs: bool = False,
    local_output_dir: str = "",
    single_slot_dest: bool = False,
    force: bool = False,
    caller: str = "mcp",
) -> dict[str, Any]:
    """Verified 3-tier shutdown preventing silent GPU quota leaks when tunnel is down or mode=='batch'."""
    from .client import WorkerClient

    s = normalize_slot(slot)
    sess = load_active_session(s)
    raw_kref = sess.kernel_ref if (sess and sess.kernel_ref) else settings.kernel_ref_for_slot(s, mode="live")
    base_kref = strip_kernel_version(raw_kref)
    out: dict[str, Any] = {
        "slot": s,
        "kernel_ref": base_kref,
        "stopped": False,
        "shutdown_sent": False,
    }

    dest = (
        Path(local_output_dir or "./kaggle_outputs")
        if single_slot_dest
        else (Path(local_output_dir) / f"slot_{s}" if local_output_dir else Path(f"./kaggle_outputs/slot_{s}"))
    )

    # Tier 1: Graceful in-band RPC `/shutdown` for live workers
    if not sess or sess.mode == "live":
        try:
            client = WorkerClient(settings, slot=s, caller=caller)
            out["worker_response"] = client.shutdown()
            out["shutdown_sent"] = True
            out["stopped"] = True
        except Exception as exc:
            out["shutdown_note"] = str(exc)

    if out["shutdown_sent"]:
        if pull_outputs:
            out["pulled_files"] = pull_kernel_outputs(
                settings,
                dest,
                slot=s,
                kernel_ref=base_kref,
                wait_complete_sec=60.0,
            )
        clear_active_session(s)
        record_local_activity(s, caller, "shutdown", f"Gracefully shut down slot '{s}'")
        return out

    # Tier 2: Check actual Kaggle Kernel status out-of-band
    api = get_kaggle_api(settings)
    kinfo = get_kernel_status(api, base_kref)
    kstatus = kinfo["status"]
    out["kaggle_api_status"] = kstatus

    if kstatus in TERMINAL_KERNEL_STATUSES or kstatus == "NOT_CREATED_YET":
        out["stopped"] = True
        if pull_outputs and kstatus == "COMPLETE":
            out["pulled_files"] = pull_kernel_outputs(settings, dest, slot=s, kernel_ref=base_kref)
        clear_active_session(s)
        record_local_activity(s, caller, "shutdown", f"Cleaned ended slot '{s}' ({kstatus})")
        return out

    # Tier 3: Kernel is still QUEUED / RUNNING (or batch run) -> out-of-band cancel via ephemeral kernel delete
    can_hard_delete = settings.is_ephemeral_managed_kernel(base_kref) and (
        force or not pull_outputs or kstatus == "QUEUED"
    )
    if can_hard_delete:
        try:
            with protect_stdout():
                api.kernels_delete(base_kref, no_confirm=True)
            out["stopped"] = True
            out["hard_cancelled"] = True
            clear_active_session(s)
            record_local_activity(
                s,
                caller,
                "shutdown",
                f"Hard-cancelled active kernel {base_kref} ({kstatus}) via Kaggle API",
            )
            return out
        except Exception as exc:
            out["cancel_error"] = str(exc)

    # Refuse to wipe sessions.json while the remote VM is still burning GPU quota!
    if sess is not None:
        sess.status = "unreachable"
        save_active_session(sess, slot=s)
    out["session_kept"] = sess is not None
    out["warning"] = (
        f"Kernel {base_kref} is still {kstatus} on Kaggle and could not be stopped cleanly. "
        "Session kept in sessions.json to prevent hidden GPU quota drain "
        "(pass force=True / --force to delete the ephemeral runner, or stop it on kaggle.com)."
    )
    return out
