"""kaggle-shell-mcp (`kgsh`) Server & CLI entrypoint exposing 12 multi-slot & competition GPU tools."""

from __future__ import annotations

import argparse
import json
import logging
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

try:
    from mcp.server.mcpserver import MCPServer as _MCPServerClass
except ImportError:
    from mcp.server.fastmcp import FastMCP as _MCPServerClass  # type: ignore[no-redef]

from . import __version__
from .batch import build_wheelhouse, read_batch_logs_windowed, submit_offline_batch
from .client import WorkerClient
from .config import (
    clear_active_session,
    load_active_session,
    load_all_sessions,
    load_settings,
    normalize_slot,
)
from .launcher import (
    TERMINAL_KERNEL_STATUSES,
    fetch_remote_state,
    get_kaggle_api,
    get_kernel_status,
    launch_workers_multi,
    pull_kernel_outputs,
)

logging.getLogger("httpx").setLevel(logging.WARNING)

mcp = _MCPServerClass(
    "kaggle-shell-mcp",
    instructions=(
        "Control up to 2 concurrent headless Kaggle GPU sessions (`slot='1'`, `slot='2'`) or offline "
        "Competition GPUs (`NvidiaL4x4`, `NvidiaRtxPro6000`). "
        "Use `kaggle_up` + `term_exec` / `job_spawn` / `fs_sync` for interactive GPU sessions with internet; "
        "use `kaggle_wheelhouse` + `kaggle_batch_run` + `kaggle_batch_logs` when competition hardware forces "
        "`enable_internet=False`. Call `kaggle_down` when finished to preserve weekly GPU quota."
    ),
)


def _fmt(data: Any) -> str:
    return json.dumps(data, ensure_ascii=False, indent=2)


@mcp.tool()
def kaggle_up(
    gpu: bool = True,
    accelerator: str = "NvidiaTeslaT4",
    slot: str = "1",
    idle_timeout_min: int = 0,
    competition: str = "",
    mount_datasets: list[str] | None = None,
    mount_models: list[str] | None = None,
    chain_previous_output: bool = False,
    wait_ready_sec: int = 240,
) -> str:
    """Provision 1 or 2 concurrent headless Kaggle GPU workers (stays alive up to 11h40m; 8h40m on TPU).

    - `slot`: `'1'` (default), `'2'` for a second parallel GPU VM, or `'1,2'` to boot both GPU VMs simultaneously!
    - `accelerator`: `'NvidiaTeslaT4'` (default 2x15GB), `'NvidiaL4'`, `'NvidiaH100'`, `'TpuV5E8'`, or `'cpu'`.
    - `idle_timeout_min`: `0` = disabled by default; set e.g. `30` for optional idle auto-shutdown.
    - `competition`: competition slug to attach (`competition_sources`). Note: if the competition forces internet OFF, use `kaggle_batch_run` instead.
    - `mount_datasets` / `mount_models`: attach Kaggle Datasets or Kaggle Hub Models at `/kaggle/input`.
    """
    settings = load_settings()
    slots = [s.strip() for s in slot.split(",") if s.strip()] or ["1"]
    res = launch_workers_multi(
        settings=settings,
        slots=slots,
        gpu=gpu,
        accelerator=accelerator,
        idle_timeout_min=idle_timeout_min,
        competition=competition or None,
        mount_datasets=mount_datasets,
        mount_models=mount_models,
        chain_previous_output=chain_previous_output,
        wait_ready_sec=wait_ready_sec,
    )
    return _fmt(res)


def _inspect_one_slot(settings: Any, api: Any, slot_name: str) -> dict[str, Any]:
    s = normalize_slot(slot_name)
    active = load_active_session(s)
    kref = active.kernel_ref if (active and active.kernel_ref) else settings.kernel_ref_for_slot(s, mode="live")
    ver_ref = f"{kref}/{active.version_number}" if (active and active.version_number) else kref
    kinfo = get_kernel_status(api, ver_ref)

    entry: dict[str, Any] = {
        "slot": s,
        "mode": active.mode if active else "live",
        "accelerator": active.accelerator if active else None,
        "kernel_ref": ver_ref,
        "kaggle_api_status": kinfo["status"],
        "failure_message": kinfo["failure_message"],
        "local_session_id": active.session_id if active else None,
    }

    if active and active.mode == "live":
        if kinfo["status"] in TERMINAL_KERNEL_STATUSES:
            clear_active_session(s)
            entry["local_session_id"] = None
            entry["cleaned_stale_session"] = True
        elif kinfo["status"] == "RUNNING":
            try:
                entry["worker_live"] = WorkerClient(settings, slot=s).status()
                return entry
            except Exception as exc:
                entry["worker_connect_note"] = str(exc)

        if kinfo["status"] != "NOT_CREATED_YET":
            rstate = fetch_remote_state(api, settings, slot=s)
            if rstate:
                entry["last_handshake_state"] = rstate

    return entry


@mcp.tool()
def kaggle_status(slot: str = "all") -> str:
    """Inspect Kaggle kernel status, Dual-T4 / multi-slot GPU telemetry, RAM, disk, and batch job states.

    - `slot='all'` (default): inspects all active/registered slots (or slot `'1'` if none registered).
    - `slot='1'` or `slot='2'`: inspects a specific worker slot.
    """
    settings = load_settings()
    api = get_kaggle_api(settings)
    raw = (slot or "all").strip().lower()

    if raw != "all":
        return _fmt(_inspect_one_slot(settings, api, raw))

    all_sessions = load_all_sessions()
    target_slots = sorted(all_sessions.keys()) if all_sessions else ["1"]
    if len(target_slots) == 1:
        return _fmt(_inspect_one_slot(settings, api, target_slots[0]))

    slots_report = {s: _inspect_one_slot(settings, api, s) for s in target_slots}
    running_gpu_slots = [
        s
        for s, info in slots_report.items()
        if info.get("kaggle_api_status") in {"RUNNING", "QUEUED"} and info.get("accelerator") != "CPU"
    ]
    return _fmt(
        {
            "active_gpu_slots_count": len(running_gpu_slots),
            "active_gpu_slots": running_gpu_slots,
            "slots": slots_report,
        }
    )


def _shutdown_one_slot(settings: Any, s: str, pull_outputs: bool, local_output_dir: str) -> dict[str, Any]:
    out: dict[str, Any] = {"slot": s, "shutdown_sent": False}
    sess = load_active_session(s)
    kref = sess.kernel_ref if (sess and sess.kernel_ref) else ""

    if not sess or sess.mode == "live":
        try:
            client = WorkerClient(settings, slot=s)
            out["worker_response"] = client.shutdown()
            out["shutdown_sent"] = True
        except Exception as exc:
            out["shutdown_note"] = str(exc)

    clear_active_session(s)

    if pull_outputs:
        dest = Path(local_output_dir) / f"slot_{s}" if local_output_dir else Path(f"./kaggle_outputs/slot_{s}")
        out["pulled_files"] = pull_kernel_outputs(settings, dest, slot=s, kernel_ref=kref)
    return out


@mcp.tool()
def kaggle_down(
    slot: str = "1",
    pull_outputs: bool = False,
    local_output_dir: str = "./kaggle_outputs",
) -> str:
    """Gracefully stop a remote Kaggle worker (`exit 0`) to save GPU quota; pass `slot='all'` to stop all slots."""
    settings = load_settings()
    raw = (slot or "1").strip().lower()

    if raw == "all":
        slots = sorted(load_all_sessions().keys()) or ["1"]
        with ThreadPoolExecutor(max_workers=min(4, len(slots))) as pool:
            results = list(
                pool.map(lambda s: _shutdown_one_slot(settings, s, pull_outputs, local_output_dir), slots)
            )
        return _fmt({"stopped_slots": results})

    s = normalize_slot(raw)
    out = _shutdown_one_slot(settings, s, pull_outputs=False, local_output_dir=local_output_dir)
    if pull_outputs:
        out["pulled_files"] = pull_kernel_outputs(settings, Path(local_output_dir), slot=s)
    return _fmt(out)


@mcp.tool()
def term_exec(
    command: str,
    session: str = "default",
    timeout_sec: int = 60,
    slot: str = "1",
) -> str:
    """Execute a bash command inside a persistent stateful PTY on Kaggle `slot` (preserves `cwd`, env, CUDA, venv)."""
    settings = load_settings()
    res = WorkerClient(settings, slot=slot).term_exec(
        command=command,
        session=session,
        timeout_sec=float(timeout_sec),
    )
    res["slot"] = normalize_slot(slot)
    return _fmt(res)


@mcp.tool()
def job_spawn(
    command: str,
    cwd: str = "/kaggle/working",
    name: str = "",
    gpu_ids: str = "",
    slot: str = "1",
) -> str:
    """Launch a detached background GPU/CPU job on `slot`.

    - `gpu_ids`: optional `CUDA_VISIBLE_DEVICES` pin (e.g. `'0'` or `'1'`) to run 2 separate GPU jobs per Dual-T4 VM!
    - `slot`: target worker VM (`'1'` or `'2'`). Combine 2 slots x 2 GPUs to run up to 4 parallel single-GPU jobs.
    """
    settings = load_settings()
    res = WorkerClient(settings, slot=slot).job_spawn(
        command=command,
        cwd=cwd,
        name=name,
        gpu_ids=gpu_ids,
    )
    res["slot"] = normalize_slot(slot)
    return _fmt(res)


@mcp.tool()
def job_list(slot: str = "1") -> str:
    """List all running and finished background jobs on worker `slot`."""
    settings = load_settings()
    res = WorkerClient(settings, slot=slot).job_list()
    res["slot"] = normalize_slot(slot)
    return _fmt(res)


@mcp.tool()
def job_logs(
    job_id: str,
    tail_lines: int = 100,
    grep: str = "",
    slot: str = "1",
) -> str:
    """Read tail lines of a background job log on `slot` with automatic `tqdm` (`\\r`) compaction."""
    settings = load_settings()
    res = WorkerClient(settings, slot=slot).job_logs(job_id=job_id, tail_lines=tail_lines, grep=grep)
    res["slot"] = normalize_slot(slot)
    return _fmt(res)


@mcp.tool()
def job_kill(job_id: str, signal: str = "SIGTERM", slot: str = "1") -> str:
    """Send `SIGTERM` or `SIGKILL` to a background job's process group on `slot` and release VRAM."""
    settings = load_settings()
    res = WorkerClient(settings, slot=slot).job_kill(job_id=job_id, sig=signal)
    res["slot"] = normalize_slot(slot)
    return _fmt(res)


@mcp.tool()
def fs_sync(
    direction: str,
    local_path: str,
    remote_path: str = "/kaggle/working",
    slot: str = "1",
) -> str:
    """Synchronize files or directories between local machine and Kaggle worker `slot` (`push` or `pull`)."""
    settings = load_settings()
    client = WorkerClient(settings, slot=slot)
    mode = direction.strip().lower()
    if mode == "push":
        return _fmt(client.fs_push(local_path=local_path, remote_path=remote_path))
    if mode == "pull":
        return _fmt(client.fs_pull(remote_path=remote_path, local_path=local_path))
    raise ValueError("`direction` must be 'push' or 'pull'.")


@mcp.tool()
def kaggle_wheelhouse(
    packages: list[str],
    slug: str = "wheels",
    python_version: str = "312",
) -> str:
    """Download Linux x86_64 binary `.whl` packages and publish them as a private Kaggle Dataset for offline runs."""
    settings = load_settings()
    res = build_wheelhouse(settings=settings, packages=packages, slug=slug, python_version=python_version)
    return _fmt(res)


@mcp.tool()
def kaggle_batch_run(
    command: str,
    local_path: str = "",
    accelerator: str = "NvidiaTeslaT4",
    competition: str = "",
    enable_internet: bool = False,
    wheel_datasets: list[str] | None = None,
    mount_datasets: list[str] | None = None,
    mount_models: list[str] | None = None,
    slot: str = "batch-1",
) -> str:
    """Run an offline/competition batch job on high-end Kaggle hardware (`NvidiaL4x4`, `NvidiaRtxPro6000`, `TpuV5E8`).

    Automatically bundles `local_path` into a private code Dataset, installs offline wheels from `wheel_datasets`,
    executes `command` even when `enable_internet=False`, and streams logs via `kaggle_batch_logs`.
    """
    settings = load_settings()
    res = submit_offline_batch(
        settings=settings,
        command=command,
        local_path=local_path,
        slot=slot,
        accelerator=accelerator,
        competition=competition or None,
        enable_internet=enable_internet,
        wheel_datasets=wheel_datasets,
        mount_datasets=mount_datasets,
        mount_models=mount_models,
    )
    return _fmt(res)


@mcp.tool()
def kaggle_batch_logs(
    slot: str = "batch-1",
    kernel_ref: str = "",
    tail_lines: int = 100,
    grep: str = "",
    wait_sec: int = 5,
) -> str:
    """Stream live or completed logs from a Kaggle batch/competition kernel via Host SSE (works even with internet OFF)."""
    settings = load_settings()
    res = read_batch_logs_windowed(
        settings=settings,
        slot=slot,
        kernel_ref=kernel_ref,
        tail_lines=tail_lines,
        grep=grep,
        wait_sec=float(wait_sec),
    )
    return _fmt(res)


def _build_cli_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="kgsh",
        description="kgsh (kaggle-shell-mcp) — Multi-Slot Kaggle GPU Shell & Offline Competition Runner",
    )
    parser.add_argument("--version", action="version", version=f"kgsh {__version__}")
    sub = parser.add_subparsers(dest="cmd", required=True)

    sub.add_parser("mcp", help="Run stdio MCP server explicitly")

    p_up = sub.add_parser("up", help="Boot 1 or 2 headless Kaggle GPU workers")
    p_up.add_argument("-s", "--slot", default="1", help="Slot ID ('1', '2', or '1,2' for dual parallel VMs)")
    p_up.add_argument("-a", "--acc", default="NvidiaTeslaT4", help="Accelerator (t4, l4, h100, tpuv5e8, cpu)")
    p_up.add_argument("--no-gpu", action="store_true", help="Boot CPU-only instance")
    p_up.add_argument("--idle", type=int, default=0, help="Optional idle auto-shutdown minutes (0 = disabled)")
    p_up.add_argument("--wait", type=int, default=240, help="Max seconds to wait for tunnel readiness")
    p_up.add_argument("--comp", default="", help="Attach competition slug")
    p_up.add_argument("--dataset", action="append", default=[], help="Mount Kaggle dataset owner/slug")
    p_up.add_argument("--model", action="append", default=[], help="Mount Kaggle Hub model slug")
    p_up.add_argument("--chain", action="store_true", help="Mount previous kernel output")

    p_st = sub.add_parser("status", help="Show kernel status and live GPU telemetry")
    p_st.add_argument("-s", "--slot", default="all", help="Slot ID or 'all' (default)")

    p_down = sub.add_parser("down", help="Gracefully shut down active Kaggle worker(s)")
    p_down.add_argument("-s", "--slot", default="1", help="Slot ID ('1', '2', or 'all')")
    p_down.add_argument("--pull", action="store_true", help="Download committed /kaggle/working outputs")
    p_down.add_argument("--out", default="./kaggle_outputs", help="Local output folder when --pull is set")

    p_exec = sub.add_parser("exec", help="Execute command in persistent remote PTY")
    p_exec.add_argument("command", help="Bash command string")
    p_exec.add_argument("-s", "--slot", default="1", help="Target worker slot")
    p_exec.add_argument("--session", default="default", help="PTY session name")
    p_exec.add_argument("--timeout", type=int, default=60, help="Timeout in seconds")

    p_spawn = sub.add_parser("spawn", help="Start detached background job")
    p_spawn.add_argument("command", help="Background command to run")
    p_spawn.add_argument("-s", "--slot", default="1", help="Target worker slot")
    p_spawn.add_argument("--gpus", default="", help="Pin CUDA_VISIBLE_DEVICES (e.g. '0' or '1')")
    p_spawn.add_argument("--cwd", default="/kaggle/working", help="Remote working directory")
    p_spawn.add_argument("--name", default="", help="Optional friendly job name")

    p_jobs = sub.add_parser("jobs", help="List background jobs")
    p_jobs.add_argument("-s", "--slot", default="1", help="Target worker slot")

    p_logs = sub.add_parser("logs", help="Tail logs of a background job")
    p_logs.add_argument("job_id", help="Job identifier")
    p_logs.add_argument("-s", "--slot", default="1", help="Target worker slot")
    p_logs.add_argument("--tail", type=int, default=100, help="Number of tail lines")
    p_logs.add_argument("--grep", default="", help="Optional regex filter")

    p_kill = sub.add_parser("kill", help="Stop a running background job")
    p_kill.add_argument("job_id", help="Job identifier")
    p_kill.add_argument("-s", "--slot", default="1", help="Target worker slot")
    p_kill.add_argument("--signal", default="SIGTERM", help="Signal name (SIGTERM or SIGKILL)")

    p_push = sub.add_parser("push", help="Upload local file/directory to Kaggle")
    p_push.add_argument("local_path", help="Local source path")
    p_push.add_argument("remote_path", nargs="?", default="/kaggle/working", help="Remote destination directory")
    p_push.add_argument("-s", "--slot", default="1", help="Target worker slot")

    p_pull = sub.add_parser("pull", help="Download remote file/directory from Kaggle")
    p_pull.add_argument("remote_path", help="Remote source file or directory")
    p_pull.add_argument("local_path", nargs="?", default="./kaggle_pull", help="Local destination directory")
    p_pull.add_argument("-s", "--slot", default="1", help="Target worker slot")

    p_whl = sub.add_parser("wheels", help="Build Linux wheelhouse dataset for offline competitions")
    p_whl.add_argument("packages", nargs="+", help="Packages to bundle (e.g. peft bitsandbytes)")
    p_whl.add_argument("--slug", default="wheels", help="Dataset suffix slug")
    p_whl.add_argument("--py", default="312", help="Target Python version (e.g. 312 or 313)")

    p_brun = sub.add_parser("batch-run", help="Submit offline competition batch job (L4x4 / RTX Pro 6000)")
    p_brun.add_argument("command", help="Command to execute inside competition container")
    p_brun.add_argument("--path", default="", help="Local project folder to bundle as dataset")
    p_brun.add_argument("-s", "--slot", default="batch-1", help="Batch slot name")
    p_brun.add_argument("-a", "--acc", default="NvidiaTeslaT4", help="Accelerator (l4x4, rtx6000, t4, tpuv5e8)")
    p_brun.add_argument("--comp", default="", help="Competition slug")
    p_brun.add_argument("--internet", action="store_true", help="Enable internet (default: False for competitions)")
    p_brun.add_argument("--wheels", action="append", default=[], help="Wheelhouse dataset owner/slug")
    p_brun.add_argument("--dataset", action="append", default=[], help="Extra dataset owner/slug")
    p_brun.add_argument("--model", action="append", default=[], help="Kaggle Hub model slug")

    p_blogs = sub.add_parser("batch-logs", help="Read live Host SSE logs from offline/batch run")
    p_blogs.add_argument("-s", "--slot", default="batch-1", help="Batch slot name")
    p_blogs.add_argument("--kernel", default="", help="Explicit owner/kernel-slug[/version]")
    p_blogs.add_argument("--tail", type=int, default=100, help="Tail lines")
    p_blogs.add_argument("--grep", default="", help="Regex filter")
    p_blogs.add_argument("--wait", type=int, default=5, help="SSE collection window seconds")

    return parser


def _run_cli(argv: list[str]) -> None:
    parser = _build_cli_parser()
    args = parser.parse_args(argv)

    if args.cmd == "mcp":
        mcp.run(transport="stdio")
    elif args.cmd == "up":
        acc = "cpu" if args.no_gpu else args.acc
        print(
            kaggle_up(
                gpu=not args.no_gpu,
                accelerator=acc,
                slot=args.slot,
                idle_timeout_min=args.idle,
                competition=args.comp,
                mount_datasets=args.dataset or None,
                mount_models=args.model or None,
                chain_previous_output=args.chain,
                wait_ready_sec=args.wait,
            )
        )
    elif args.cmd == "status":
        print(kaggle_status(slot=args.slot))
    elif args.cmd == "down":
        print(kaggle_down(slot=args.slot, pull_outputs=args.pull, local_output_dir=args.out))
    elif args.cmd == "exec":
        print(term_exec(command=args.command, session=args.session, timeout_sec=args.timeout, slot=args.slot))
    elif args.cmd == "spawn":
        print(job_spawn(command=args.command, cwd=args.cwd, name=args.name, gpu_ids=args.gpus, slot=args.slot))
    elif args.cmd == "jobs":
        print(job_list(slot=args.slot))
    elif args.cmd == "logs":
        print(job_logs(job_id=args.job_id, tail_lines=args.tail, grep=args.grep, slot=args.slot))
    elif args.cmd == "kill":
        print(job_kill(job_id=args.job_id, signal=args.signal, slot=args.slot))
    elif args.cmd == "push":
        print(fs_sync(direction="push", local_path=args.local_path, remote_path=args.remote_path, slot=args.slot))
    elif args.cmd == "pull":
        print(fs_sync(direction="pull", local_path=args.local_path, remote_path=args.remote_path, slot=args.slot))
    elif args.cmd == "wheels":
        print(kaggle_wheelhouse(packages=args.packages, slug=args.slug, python_version=args.py))
    elif args.cmd == "batch-run":
        print(
            kaggle_batch_run(
                command=args.command,
                local_path=args.path,
                accelerator=args.acc,
                competition=args.comp,
                enable_internet=args.internet,
                wheel_datasets=args.wheels or None,
                mount_datasets=args.dataset or None,
                mount_models=args.model or None,
                slot=args.slot,
            )
        )
    elif args.cmd == "batch-logs":
        print(
            kaggle_batch_logs(
                slot=args.slot,
                kernel_ref=args.kernel,
                tail_lines=args.tail,
                grep=args.grep,
                wait_sec=args.wait,
            )
        )


def main() -> None:
    argv = sys.argv[1:]
    if not argv:
        mcp.run(transport="stdio")
        return
    if argv[0] == "--cli":
        argv = argv[1:]
    _run_cli(argv)


if __name__ == "__main__":
    main()
