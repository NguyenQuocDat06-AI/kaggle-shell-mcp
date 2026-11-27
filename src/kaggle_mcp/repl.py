"""Interactive SSH-style REPL (`kgsh shell`), live log follower (`kgsh logs -f`), and rich Quota view."""

from __future__ import annotations

import sys
import time
from typing import Any

from rich.console import Console
from rich.table import Table

from .client import WorkerClient
from .config import Settings, normalize_slot
from .quota import fetch_quota_summary


def render_quota_table(qdata: dict[str, Any], console: Console | None = None) -> Table:
    """Build a Rich table displaying weekly Kaggle GPU & TPU quota statistics."""
    tbl = Table(title=f"Kaggle Weekly Accelerator Quota ({qdata.get('username', '')})")
    tbl.add_column("Pool", style="bold cyan")
    tbl.add_column("Used", justify="right")
    tbl.add_column("Remaining", justify="right", style="bold green")
    tbl.add_column("Total Limit", justify="right")
    tbl.add_column("Usage %", justify="right")

    for key, label in (("gpu", "GPU (T4x2 / L4)"), ("tpu", "TPU (v5e / v6e)")):
        b = qdata.get(key, {})
        rem_style = "bold red" if b.get("remaining_hours", 0) <= 1.0 else "bold green"
        tbl.add_row(
            label,
            f"{b.get('used_hours', 0):.2f}h",
            f"[{rem_style}]{b.get('remaining_hours', 0):.2f}h[/{rem_style}]",
            f"{b.get('total_hours', 0):.2f}h",
            f"{b.get('used_pct', 0):.1f}%",
        )

    c = console or Console()
    c.print(tbl)
    refresh = qdata.get("quota_refresh_time")
    if refresh:
        c.print(f"[dim]Weekly reset at: {refresh}[/dim]")
    return tbl


def stream_job_logs(
    settings: Settings,
    job_id: str,
    slot: str = "1",
    poll_sec: float = 1.5,
) -> int:
    """Continuously stream incremental output from a background job (`tail -f`)."""
    console = Console()
    client = WorkerClient(settings, slot=slot, caller="cli")
    offset = 0
    printed_fallback = ""

    console.print(
        f"[bold cyan]Streaming logs for[/] [yellow]{job_id}[/] [dim](slot={client.slot}, Ctrl+C to detach)[/]"
    )
    try:
        while True:
            res = client.job_logs_chunk(job_id=job_id, offset=offset)
            chunk = res.get("chunk", "")
            if res.get("fallback"):
                if chunk != printed_fallback:
                    delta = chunk[len(printed_fallback) :] if chunk.startswith(printed_fallback) else chunk
                    if delta:
                        sys.stdout.write(delta + ("" if delta.endswith("\n") else "\n"))
                        sys.stdout.flush()
                    printed_fallback = chunk
            elif chunk:
                sys.stdout.write(chunk)
                if not chunk.endswith("\n"):
                    sys.stdout.write("\n")
                sys.stdout.flush()
                offset = int(res.get("next_offset", offset))

            job = res.get("job") or {}
            status = job.get("status", "running")
            if status != "running":
                ec = job.get("exit_code", 0)
                color = "green" if ec == 0 else "red"
                console.print(f"\n[{color}]Job {job_id} {status} (exit_code={ec})[/{color}]")
                return int(ec or 0)

            time.sleep(max(0.5, float(poll_sec)))
    except KeyboardInterrupt:
        console.print("\n[dim]Detached from log stream (job continues running on Kaggle).[/dim]")
        return 0


def run_interactive_shell(
    settings: Settings,
    slot: str = "1",
    session: str = "default",
    timeout_sec: float = 300.0,
) -> int:
    """Attach an interactive SSH-like REPL sharing the stateful PTY with the MCP Agent."""
    console = Console()
    client = WorkerClient(settings, slot=slot, caller="shell")

    st = client.status()
    gpus = st.get("gpus") or []
    gpu_desc = ", ".join(f"GPU{g['index']}:{g['name']}" for g in gpus) or "CPU"
    init_res = client.term_exec("pwd", session=session, timeout_sec=15.0)
    cwd = init_res.get("cwd", "/kaggle/working")

    console.print(
        f"[bold green]Connected to Kaggle slot '{client.slot}'[/] "
        f"([cyan]{gpu_desc}[/], session=[yellow]{session}[/])\n"
        "[dim]Commands: `:status`, `:jobs`, `:quota`, `:slot <id>`, `:down`, or `exit` to detach.[/dim]"
    )

    while True:
        try:
            prompt = f"[bold cyan]kgsh[/][dim]({client.slot})[/]:[bold blue]{cwd}[/]$ "
            line = console.input(prompt).strip()
        except EOFError:
            console.print("\n[dim]Detached (worker continues running in background).[/dim]")
            return 0
        except KeyboardInterrupt:
            console.print("^C [dim](type `exit` to detach)[/dim]")
            continue

        if not line:
            continue
        if line in {"exit", "quit", ":q"}:
            console.print("[dim]Detached (worker continues running in background).[/dim]")
            return 0

        if line.startswith(":"):
            parts = line[1:].split()
            cmd = parts[0].lower() if parts else ""
            if cmd == "status":
                cur = client.status()
                rem_h = cur.get("remaining_before_drain_sec", 0) / 3600.0
                console.print(
                    f"Uptime: {cur.get('uptime_sec', 0):.0f}s | Drain in: {rem_h:.2f}h | "
                    f"RAM: {cur.get('ram', {}).get('used_gb')}/{cur.get('ram', {}).get('total_gb')} GB | "
                    f"Active jobs: {cur.get('active_jobs', 0)}"
                )
                for g in cur.get("gpus") or []:
                    console.print(
                        f"  GPU {g['index']} ({g['name']}): {g['util_pct']:.0f}% util | "
                        f"VRAM {g['vram_used_mb']:.0f}/{g['vram_total_mb']:.0f} MB | {g['temp_c']:.0f}°C"
                    )
                continue
            if cmd == "jobs":
                jlist = client.job_list().get("jobs", [])
                if not jlist:
                    console.print("[dim]No background jobs on this slot.[/dim]")
                for j in jlist:
                    console.print(
                        f"  {j['job_id']} [{j['status']}] name={j['name']} "
                        f"gpu={j.get('gpu_ids') or 'all'} time={j.get('runtime_sec')}s"
                    )
                continue
            if cmd == "quota":
                render_quota_table(fetch_quota_summary(settings), console=console)
                continue
            if cmd == "slot" and len(parts) >= 2:
                new_slot = normalize_slot(parts[1])
                client = WorkerClient(settings, slot=new_slot, caller="shell")
                cwd = client.term_exec("pwd", session=session, timeout_sec=15.0).get("cwd", "/kaggle/working")
                console.print(f"[green]Switched to slot '{client.slot}'.[/green]")
                continue
            if cmd == "down":
                client.shutdown()
                console.print(f"[yellow]Sent graceful shutdown to slot '{client.slot}'.[/yellow]")
                return 0
            console.print(f"[red]Unknown meta-command: {line}[/red]")
            continue

        try:
            res = client.term_exec(line, session=session, timeout_sec=timeout_sec)
            cwd = res.get("cwd") or cwd
            out = res.get("output") or ""
            if out:
                sys.stdout.write(out + ("\n" if not out.endswith("\n") else ""))
                sys.stdout.flush()
            ec = res.get("exit_code", 0)
            if res.get("timed_out"):
                console.print(
                    f"[yellow]Command timed out after {timeout_sec:.0f}s (sent SIGINT). "
                    "Use `kgsh spawn` for long training runs.[/yellow]"
                )
            elif ec != 0:
                console.print(f"[red][exit {ec} · {res.get('duration_sec', 0):.2f}s][/red]")
        except KeyboardInterrupt:
            console.print("\n[yellow]Interrupted local wait.[/yellow]")
        except Exception as exc:
            console.print(f"[red]Error: {exc}[/red]")
