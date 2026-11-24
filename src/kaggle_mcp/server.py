"""kaggle-shell-mcp Server & CLI entrypoint exposing 9 Kaggle GPU remote terminal & job tools."""

from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path
from typing import Any

try:
    from mcp.server.mcpserver import MCPServer as _MCPServerClass
except ImportError:
    from mcp.server.fastmcp import FastMCP as _MCPServerClass  # type: ignore[no-redef]

from . import __version__
from .client import WorkerClient
from .config import clear_active_session, load_active_session, load_settings
from .launcher import (
    fetch_remote_state,
    get_kaggle_api,
    get_kernel_status,
    launch_worker,
    pull_kernel_outputs,
)

logging.getLogger("httpx").setLevel(logging.WARNING)

mcp = _MCPServerClass(
    "kaggle-shell-mcp",
    instructions=(
        "Control a headless Kaggle Dual-T4 GPU session (up to 12h) via stateful PTY shell "
        "and detached background jobs. Use `kaggle_up` to provision GPUs, `term_exec` for "
        "short stateful commands, `job_spawn` / `job_logs` for long training runs, "
        "`fs_sync` to push code or pull checkpoints, and `kaggle_down` as soon as work "
        "completes to preserve weekly GPU quota."
    ),
)


def _fmt(data: Any) -> str:
    return json.dumps(data, ensure_ascii=False, indent=2)


@mcp.tool()
def kaggle_up(
    gpu: bool = True,
    idle_timeout_min: int = 0,
    mount_datasets: list[str] | None = None,
    chain_previous_output: bool = False,
    wait_ready_sec: int = 240,
) -> str:
    """Provision a headless Kaggle GPU worker (stays alive up to 11h40m safe drain).

    - `gpu=True`: requests Dual NVIDIA T4 x2 (`NvidiaTeslaT4`, 2x15GB VRAM).
    - `idle_timeout_min=0`: optional idle auto-shutdown in minutes (`0` = disabled by default; set e.g. `30` to auto-stop when idle).
    - `mount_datasets`: list of Kaggle dataset slugs (`["owner/dataset-slug"]`) mounted at `/kaggle/input`.
    - `chain_previous_output=True`: mounts previous `/kaggle/working` output to continue >12h training runs.
    """
    settings = load_settings()
    res = launch_worker(
        settings=settings,
        gpu=gpu,
        idle_timeout_min=idle_timeout_min,
        mount_datasets=mount_datasets,
        chain_previous_output=chain_previous_output,
        wait_ready_sec=wait_ready_sec,
    )
    return _fmt(res)


@mcp.tool()
def kaggle_status() -> str:
    """Inspect Kaggle kernel status, tunnel health, Dual-T4 GPU/VRAM utilization, RAM, disk, and remaining session time."""
    settings = load_settings()
    api = get_kaggle_api(settings)
    kinfo = get_kernel_status(api, settings.kernel_ref)
    active = load_active_session()

    result: dict[str, Any] = {
        "kernel_ref": settings.kernel_ref,
        "kaggle_api_status": kinfo["status"],
        "failure_message": kinfo["failure_message"],
        "local_session_id": active.session_id if active else None,
    }

    if active and kinfo["status"] == "RUNNING":
        try:
            client = WorkerClient(settings)
            result["worker_live"] = client.status()
            return _fmt(result)
        except Exception as exc:
            result["worker_connect_note"] = str(exc)

    if kinfo["status"] != "NOT_CREATED_YET":
        rstate = fetch_remote_state(api, settings)
        if rstate:
            result["last_handshake_state"] = rstate
    return _fmt(result)


@mcp.tool()
def kaggle_down(pull_outputs: bool = False, local_output_dir: str = "./kaggle_outputs") -> str:
    """Gracefully stop the remote Kaggle worker (`exit 0`) immediately to save GPU quota; optionally pull `/kaggle/working`."""
    settings = load_settings()
    out: dict[str, Any] = {"shutdown_sent": False}

    try:
        client = WorkerClient(settings)
        out["worker_response"] = client.shutdown()
        out["shutdown_sent"] = True
    except Exception as exc:
        out["shutdown_note"] = str(exc)

    clear_active_session()

    if pull_outputs:
        files = pull_kernel_outputs(settings, Path(local_output_dir))
        out["pulled_files"] = files
    return _fmt(out)


@mcp.tool()
def term_exec(command: str, session: str = "default", timeout_sec: int = 60) -> str:
    """Execute a bash command inside a persistent stateful PTY on Kaggle (preserves `cwd`, env vars, CUDA, and virtualenvs).

    Use for quick inspection, setup, and short commands (`< timeout_sec`). Use `job_spawn` for long training runs.
    """
    settings = load_settings()
    res = WorkerClient(settings).term_exec(
        command=command,
        session=session,
        timeout_sec=float(timeout_sec),
    )
    return _fmt(res)


@mcp.tool()
def job_spawn(command: str, cwd: str = "/kaggle/working", name: str = "") -> str:
    """Launch a detached long-running GPU/CPU background job immune to network drops, logging to `/kaggle/working/mcp_logs`."""
    settings = load_settings()
    res = WorkerClient(settings).job_spawn(command=command, cwd=cwd, name=name)
    return _fmt(res)


@mcp.tool()
def job_list() -> str:
    """List all running and finished background jobs with PID, runtime, exit code, and log size."""
    settings = load_settings()
    return _fmt(WorkerClient(settings).job_list())


@mcp.tool()
def job_logs(job_id: str, tail_lines: int = 100, grep: str = "") -> str:
    """Read tail lines of a background job log with automatic `tqdm` carriage-return (`\\r`) compaction."""
    settings = load_settings()
    res = WorkerClient(settings).job_logs(job_id=job_id, tail_lines=tail_lines, grep=grep)
    return _fmt(res)


@mcp.tool()
def job_kill(job_id: str, signal: str = "SIGTERM") -> str:
    """Send `SIGTERM` or `SIGKILL` to a background job's entire process group and release GPU VRAM."""
    settings = load_settings()
    res = WorkerClient(settings).job_kill(job_id=job_id, sig=signal)
    return _fmt(res)


@mcp.tool()
def fs_sync(direction: str, local_path: str, remote_path: str = "/kaggle/working") -> str:
    """Synchronize files or directories between the local machine and Kaggle over compressed `.tar.gz` streams.

    - `direction="push"`: pack `local_path` (auto-ignoring `.git`, `.venv`, `__pycache__`, `node_modules`) into `remote_path`.
    - `direction="pull"`: download `remote_path` from Kaggle and extract into `local_path`.
    """
    settings = load_settings()
    client = WorkerClient(settings)
    mode = direction.strip().lower()
    if mode == "push":
        return _fmt(client.fs_push(local_path=local_path, remote_path=remote_path))
    if mode == "pull":
        return _fmt(client.fs_pull(remote_path=remote_path, local_path=local_path))
    raise ValueError("`direction` must be 'push' or 'pull'.")


def _build_cli_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="kgsh",
        description="kgsh (kaggle-shell-mcp) — Headless 12h Kaggle Dual-T4 GPU Worker & MCP Server",
    )
    parser.add_argument("--version", action="version", version=f"kgsh {__version__}")
    sub = parser.add_subparsers(dest="cmd", required=True)

    sub.add_parser("mcp", help="Run stdio MCP server explicitly")

    p_up = sub.add_parser("up", help="Boot headless Kaggle GPU worker")
    p_up.add_argument("--no-gpu", action="store_true", help="Boot CPU-only instance")
    p_up.add_argument(
        "--idle",
        type=int,
        default=0,
        help="Optional idle auto-shutdown minutes (0 = disabled by default)",
    )
    p_up.add_argument("--wait", type=int, default=240, help="Max seconds to wait for tunnel readiness")
    p_up.add_argument("--dataset", action="append", default=[], help="Mount Kaggle dataset owner/slug")
    p_up.add_argument("--chain", action="store_true", help="Mount previous kernel output")

    sub.add_parser("status", help="Show kernel status and live GPU telemetry")

    p_down = sub.add_parser("down", help="Gracefully shut down active Kaggle worker")
    p_down.add_argument("--pull", action="store_true", help="Download committed /kaggle/working outputs")
    p_down.add_argument("--out", default="./kaggle_outputs", help="Local output folder when --pull is set")

    p_exec = sub.add_parser("exec", help="Execute command in persistent remote PTY")
    p_exec.add_argument("command", help="Bash command string")
    p_exec.add_argument("--session", default="default", help="PTY session name")
    p_exec.add_argument("--timeout", type=int, default=60, help="Timeout in seconds")

    p_spawn = sub.add_parser("spawn", help="Start detached background job")
    p_spawn.add_argument("command", help="Background command to run")
    p_spawn.add_argument("--cwd", default="/kaggle/working", help="Remote working directory")
    p_spawn.add_argument("--name", default="", help="Optional friendly job name")

    sub.add_parser("jobs", help="List background jobs")

    p_logs = sub.add_parser("logs", help="Tail logs of a background job")
    p_logs.add_argument("job_id", help="Job identifier")
    p_logs.add_argument("--tail", type=int, default=100, help="Number of tail lines")
    p_logs.add_argument("--grep", default="", help="Optional regex filter")

    p_kill = sub.add_parser("kill", help="Stop a running background job")
    p_kill.add_argument("job_id", help="Job identifier")
    p_kill.add_argument("--signal", default="SIGTERM", help="Signal name (SIGTERM or SIGKILL)")

    p_push = sub.add_parser("push", help="Upload local file/directory to Kaggle")
    p_push.add_argument("local_path", help="Local source path")
    p_push.add_argument("remote_path", nargs="?", default="/kaggle/working", help="Remote destination directory")

    p_pull = sub.add_parser("pull", help="Download remote file/directory from Kaggle")
    p_pull.add_argument("remote_path", help="Remote source file or directory")
    p_pull.add_argument("local_path", nargs="?", default="./kaggle_pull", help="Local destination directory")

    return parser


def _run_cli(argv: list[str]) -> None:
    parser = _build_cli_parser()
    args = parser.parse_args(argv)

    if args.cmd == "mcp":
        mcp.run(transport="stdio")
    elif args.cmd == "up":
        print(
            kaggle_up(
                gpu=not args.no_gpu,
                idle_timeout_min=args.idle,
                mount_datasets=args.dataset or None,
                chain_previous_output=args.chain,
                wait_ready_sec=args.wait,
            )
        )
    elif args.cmd == "status":
        print(kaggle_status())
    elif args.cmd == "down":
        print(kaggle_down(pull_outputs=args.pull, local_output_dir=args.out))
    elif args.cmd == "exec":
        print(term_exec(command=args.command, session=args.session, timeout_sec=args.timeout))
    elif args.cmd == "spawn":
        print(job_spawn(command=args.command, cwd=args.cwd, name=args.name))
    elif args.cmd == "jobs":
        print(job_list())
    elif args.cmd == "logs":
        print(job_logs(job_id=args.job_id, tail_lines=args.tail, grep=args.grep))
    elif args.cmd == "kill":
        print(job_kill(job_id=args.job_id, signal=args.signal))
    elif args.cmd == "push":
        print(fs_sync(direction="push", local_path=args.local_path, remote_path=args.remote_path))
    elif args.cmd == "pull":
        print(fs_sync(direction="pull", local_path=args.local_path, remote_path=args.remote_path))


def main() -> None:
    argv = sys.argv[1:]
    if not argv:
        # Default zero-arg invocation runs the stdio MCP server
        mcp.run(transport="stdio")
        return
    if argv[0] == "--cli":
        argv = argv[1:]
    _run_cli(argv)


if __name__ == "__main__":
    main()
