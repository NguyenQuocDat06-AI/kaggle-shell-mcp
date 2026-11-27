"""JSON stdin/stdout CLI bridge exposing Kaggle SDK & launcher helpers to the native Rust TUI."""

from __future__ import annotations

import contextlib
import io
import json
import sys
from typing import Any

from .batch import read_batch_logs_windowed
from .client import WorkerClient, WorkerSessionStoppedError
from .config import (
    load_active_session,
    load_settings,
    normalize_slot,
)
from .kaggle_hub import (
    delete_kaggle_kernel,
    download_kaggle_hub_item,
    fetch_kernel_logs,
    inspect_kaggle_hub_item,
    search_kaggle_hub,
    submit_competition_file,
)
from .launcher import launch_workers_multi, normalize_str_list, shutdown_slot
from .quota import fetch_quota_summary


def dispatch_bridge_action(action: str, payload: dict[str, Any]) -> Any:
    """Execute a Kaggle platform action cleanly and return a JSON-serializable result."""
    act = str(action or "").strip().lower()
    settings = load_settings()

    if act == "quota":
        return fetch_quota_summary(settings)

    if act == "boot":
        slots_raw = payload.get("slots") or "1"
        if isinstance(slots_raw, str):
            slots = [s.strip() for s in slots_raw.split(",") if s.strip()] or ["1"]
        else:
            slots = [str(s).strip() for s in slots_raw if str(s).strip()] or ["1"]
        acc = str(payload.get("accelerator") or "NvidiaTeslaT4")
        ds = normalize_str_list(payload.get("datasets"))
        mdls = normalize_str_list(payload.get("models"))
        comp = str(payload.get("competition") or "").strip() or None
        idle = max(0, int(payload.get("idle_timeout_min") or 0))
        chain = bool(payload.get("chain_previous_output", False))
        wait_sec = max(30, int(payload.get("wait_ready_sec") or 240))
        return launch_workers_multi(
            settings=settings,
            slots=slots,
            gpu=acc.lower() != "cpu",
            accelerator=acc,
            idle_timeout_min=idle,
            mount_datasets=ds or None,
            mount_models=mdls or None,
            competition=comp,
            chain_previous_output=chain,
            wait_ready_sec=wait_sec,
        )

    if act == "down":
        slot = normalize_slot(str(payload.get("slot") or "1"))
        force = bool(payload.get("force", False))
        return shutdown_slot(settings, slot, force=force, caller="tui")

    if act == "hub_search":
        return search_kaggle_hub(
            settings,
            category=str(payload.get("category") or "datasets"),
            query=str(payload.get("query") or ""),
        )

    if act == "hub_inspect":
        return inspect_kaggle_hub_item(
            settings,
            category=str(payload.get("category") or "datasets"),
            ref=str(payload.get("ref") or ""),
        )

    if act == "hub_download":
        dest = download_kaggle_hub_item(
            settings,
            category=str(payload.get("category") or "datasets"),
            ref=str(payload.get("ref") or ""),
            base_dir=str(payload.get("base_dir") or "./kaggle_downloads"),
        )
        return {"ok": True, "dest": dest}

    if act == "hub_kernel_logs":
        text = fetch_kernel_logs(
            settings,
            kernel_ref=str(payload.get("kernel_ref") or ""),
            tail_lines=int(payload.get("tail_lines") or 120),
        )
        return {"logs": text}

    if act == "hub_kernel_delete":
        return delete_kaggle_kernel(settings, kernel_ref=str(payload.get("kernel_ref") or ""))

    if act == "submit_csv":
        return submit_competition_file(
            settings,
            competition=str(payload.get("competition") or ""),
            local_csv_path=str(payload.get("local_csv_path") or ""),
            message=str(payload.get("message") or "Submitted via kgsh"),
        )

    if act == "refresh_endpoint":
        slot = normalize_slot(str(payload.get("slot") or "1"))
        sess = load_active_session(slot)
        if not sess:
            return {"stopped": True, "message": f"No active session for slot '{slot}'."}
        try:
            updated = WorkerClient(settings, slot=slot, caller="tui", session_override=sess)._refresh_endpoint(sess)
        except WorkerSessionStoppedError as exc:
            return {"stopped": True, "message": str(exc)}
        if updated is None:
            return {"stopped": False, "updated": False}
        return {
            "stopped": False,
            "updated": True,
            "tunnel_url": updated.tunnel_url,
            "ts_url": updated.ts_url,
            "base_url": updated.base_url,
        }

    if act == "batch_logs":
        slot = normalize_slot(str(payload.get("slot") or "1"))
        return read_batch_logs_windowed(
            settings,
            slot=slot,
            tail_lines=int(payload.get("tail_lines") or 80),
            wait_sec=float(payload.get("wait_sec") or 2.0),
        )

    raise ValueError(f"Unknown bridge action: {action!r}")


def main(argv: list[str] | None = None) -> int:
    if hasattr(sys.stdin, "reconfigure"):
        sys.stdin.reconfigure(encoding="utf-8", errors="replace")
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    args = argv if argv is not None else sys.argv[1:]
    if not args:
        sys.stdout.write(json.dumps({"ok": False, "error": "Missing bridge action"}, ensure_ascii=True) + "\n")
        return 2

    action = args[0]
    raw_in = sys.stdin.read().strip() if not sys.stdin.isatty() else ""
    try:
        payload = json.loads(raw_in) if raw_in else {}
        if not isinstance(payload, dict):
            raise ValueError("Bridge stdin payload must be a JSON object.")
    except Exception as exc:
        sys.stdout.write(
            json.dumps({"ok": False, "error": f"Invalid JSON stdin: {exc}"}, ensure_ascii=True) + "\n"
        )
        return 2

    real_stdout = sys.stdout
    buf = io.StringIO()
    try:
        # Isolate stdout so noisy third-party SDK prints never corrupt the single-line JSON contract
        with contextlib.redirect_stdout(buf):
            data = dispatch_bridge_action(action, payload)
        real_stdout.write(json.dumps({"ok": True, "data": data}, ensure_ascii=True) + "\n")
        real_stdout.flush()
        return 0
    except Exception as exc:
        real_stdout.write(
            json.dumps({"ok": False, "error": f"{type(exc).__name__}: {exc}"}, ensure_ascii=True) + "\n"
        )
        real_stdout.flush()
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
