#!/usr/bin/env python3
"""Self-contained Kaggle GPU worker: PTY shell, detached jobs, tunnel & watchdogs.

Runs directly inside the Kaggle Kernel Python runtime so CUDA/Conda environment
variables are preserved 100% without needing openssh-server or root apt packages.
"""

from __future__ import annotations

import base64
import csv
import fcntl
import fnmatch
import gzip
import hmac
import io
import json
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tarfile
import termios
import threading
import time
import urllib.request
import uuid
import zipfile
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

BOOTSTRAP_CONFIG_B64 = "__BOOTSTRAP_CONFIG_B64__"

CFG = json.loads(base64.b64decode(BOOTSTRAP_CONFIG_B64.encode("ascii")).decode("utf-8"))
SESSION_ID: str = CFG["session_id"]
WORKER_TOKEN: str = CFG["worker_token"]
KAGGLE_USERNAME: str = CFG["kaggle_username"]
KAGGLE_SECRET: str = CFG["kaggle_secret"]
STATE_DATASET_REF: str = CFG["state_dataset_ref"]
TS_AUTHKEY: str = CFG.get("ts_authkey", "")
TS_HOSTNAME: str = CFG.get("ts_hostname", "kaggle-mcp-worker")
IDLE_TIMEOUT_SEC: int = int(CFG.get("idle_timeout_sec", 0))
MAX_LIFETIME_SEC: int = int(CFG.get("max_lifetime_sec", 42000))  # 11h40m
DRAIN_AT_SEC: int = int(CFG.get("drain_at_sec", 41700))          # 11h35m
RPC_PORT: int = int(CFG.get("rpc_port", 8765))

WORKING_DIR = Path("/kaggle/working") if Path("/kaggle/working").exists() else Path.cwd()
LOG_DIR = WORKING_DIR / "mcp_logs"
TMP_DIR = Path("/tmp/kaggle_mcp_runtime")
LOG_DIR.mkdir(parents=True, exist_ok=True)
TMP_DIR.mkdir(parents=True, exist_ok=True)

START_MONO = time.monotonic()
START_WALL = time.time()
SHUTDOWN_EVENT = threading.Event()

ANSI_CURSOR_RE = re.compile(r"\x1b\[[0-9;?]*[ABCDEFGHJKSTfhlm]")
TRYCF_URL_RE = re.compile(r"https://[a-zA-Z0-9-]+\.trycloudflare\.com")


def fixup_cuda_env() -> None:
    extra_bins = ["/opt/bin", "/usr/local/nvidia/bin", "/usr/local/cuda/bin", "/opt/conda/bin"]
    cur_path = os.environ.get("PATH", "")
    parts = [p for p in cur_path.split(":") if p]
    for b in extra_bins:
        if os.path.isdir(b) and b not in parts:
            parts.insert(0, b)
    os.environ["PATH"] = ":".join(parts)

    extra_libs = ["/usr/local/nvidia/lib64", "/usr/local/cuda/lib64"]
    cur_ld = os.environ.get("LD_LIBRARY_PATH", "")
    ld_parts = [p for p in cur_ld.split(":") if p]
    for lib in extra_libs:
        if os.path.isdir(lib) and lib not in ld_parts:
            ld_parts.insert(0, lib)
    os.environ["LD_LIBRARY_PATH"] = ":".join(ld_parts)
    os.environ["PYTHONUNBUFFERED"] = "1"


def clean_tqdm(text: str) -> str:
    """Collapse carriage-return progress bar updates so Agent context stays compact."""
    text = text.replace("\r\n", "\n")
    cleaned_lines: list[str] = []
    for raw_line in text.split("\n"):
        if "\r" in raw_line:
            segments = [s for s in raw_line.split("\r") if s != ""]
            raw_line = segments[-1] if segments else ""
        cleaned_lines.append(ANSI_CURSOR_RE.sub("", raw_line))
    return "\n".join(cleaned_lines)


def query_gpus() -> list[dict]:
    try:
        res = subprocess.run(
            [
                "nvidia-smi",
                "--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu",
                "--format=csv,noheader,nounits",
            ],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if res.returncode != 0:
            return []
        gpus = []
        for line in res.stdout.strip().splitlines():
            cols = [c.strip() for c in line.split(",")]
            if len(cols) >= 6:
                gpus.append(
                    {
                        "index": int(cols[0]),
                        "name": cols[1],
                        "util_pct": float(cols[2]) if cols[2].replace(".", "").isdigit() else 0.0,
                        "vram_used_mb": float(cols[3]) if cols[3].replace(".", "").isdigit() else 0.0,
                        "vram_total_mb": float(cols[4]) if cols[4].replace(".", "").isdigit() else 0.0,
                        "temp_c": float(cols[5]) if cols[5].replace(".", "").isdigit() else 0.0,
                    }
                )
        return gpus
    except Exception:
        return []


def query_gpu_processes() -> list[dict]:
    """Return active CUDA compute processes via nvidia-smi."""
    try:
        idx_res = subprocess.run(
            ["nvidia-smi", "--query-gpu=index,uuid", "--format=csv,noheader"],
            capture_output=True,
            text=True,
            timeout=2,
            check=False,
        )
        uuid_to_idx: dict[str, str] = {}
        if idx_res.returncode == 0:
            for line in idx_res.stdout.strip().splitlines():
                parts = [p.strip() for p in line.split(",")]
                if len(parts) >= 2:
                    uuid_to_idx[parts[1]] = parts[0]

        res = subprocess.run(
            [
                "nvidia-smi",
                "--query-compute-apps=pid,gpu_uuid,process_name,used_memory",
                "--format=csv,noheader,nounits",
            ],
            capture_output=True,
            text=True,
            timeout=2,
            check=False,
        )
        if res.returncode != 0:
            return []
        procs: list[dict] = []
        for line in res.stdout.strip().splitlines():
            line = line.strip()
            if not line or "No running processes" in line:
                continue
            parts = [p.strip() for p in line.split(",")]
            if len(parts) < 4:
                continue
            try:
                pid = int(parts[0])
            except ValueError:
                continue
            gpu_uuid = parts[1]
            proc_name = parts[2]
            try:
                used_mb = int(float(parts[3]))
            except ValueError:
                used_mb = 0
            procs.append(
                {
                    "pid": pid,
                    "gpu_index": uuid_to_idx.get(gpu_uuid, "?"),
                    "process_name": Path(proc_name).name or proc_name,
                    "full_name": proc_name,
                    "used_mb": used_mb,
                }
            )
        procs.sort(key=lambda item: item["used_mb"], reverse=True)
        return procs
    except Exception:
        return []


def query_tpu() -> dict | None:
    accel_devs = sorted(Path("/dev").glob("accel[0-9]*"))
    vfio_devs = [p for p in sorted(Path("/dev/vfio").glob("[0-9]*")) if p.name.isdigit()] if Path("/dev/vfio").is_dir() else []
    tpu_env = os.environ.get("TPU_ACCELERATOR_TYPE") or os.environ.get("TPU_NAME") or ""
    count = len(accel_devs) or len(vfio_devs)
    if not count and not tpu_env:
        return None
    return {
        "type": tpu_env or "Cloud TPU VM",
        "device_nodes": count or 8,
        "driver": "accel" if accel_devs else ("vfio" if vfio_devs else "env"),
    }


def query_ram_gb() -> dict:
    info: dict[str, float] = {}
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith(("MemTotal:", "MemAvailable:")):
                k, v = line.split(":", 1)
                kb = float(v.strip().split()[0])
                info[k] = round(kb / (1024 * 1024), 2)
        total = info.get("MemTotal", 0.0)
        avail = info.get("MemAvailable", 0.0)
        return {"used_gb": round(max(0.0, total - avail), 2), "total_gb": total}
    except Exception:
        return {"used_gb": 0.0, "total_gb": 0.0}


def query_disk_gb(path: Path) -> dict:
    try:
        u = shutil.disk_usage(path)
        gb = 1024 ** 3
        return {
            "used_gb": round(u.used / gb, 2),
            "free_gb": round(u.free / gb, 2),
            "total_gb": round(u.total / gb, 2),
        }
    except Exception:
        return {"used_gb": 0.0, "free_gb": 0.0, "total_gb": 0.0}


class PtySession:
    """Persistent bash PTY preserving cwd, shell variables, and virtualenvs."""

    def __init__(self, name: str, cwd: Path = WORKING_DIR) -> None:
        self.name = name
        self.cwd = str(cwd)
        self.lock = threading.Lock()
        self._live_lock = threading.Lock()
        self._live_running = False
        self._live_cmd = ""
        self._live_started = 0.0
        self._live_sentinel = ""
        self._live_buf = bytearray()
        self.master_fd: int = -1
        self.proc: subprocess.Popen | None = None
        self._spawn()

    def _spawn(self) -> None:
        master_fd, slave_fd = pty.openpty()
        # 40 rows x 160 cols prevents command output wrapping
        winsize = struct.pack("HHHH", 40, 160, 0, 0)
        fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, winsize)

        env = os.environ.copy()
        env["TERM"] = "dumb"
        env["PS1"] = ""

        self.proc = subprocess.Popen(
            ["/bin/bash", "--norc", "--noprofile"],
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            cwd=self.cwd if os.path.isdir(self.cwd) else str(WORKING_DIR),
            env=env,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave_fd)
        fl = fcntl.fcntl(master_fd, fcntl.F_GETFL)
        fcntl.fcntl(master_fd, fcntl.F_SETFL, fl | os.O_NONBLOCK)
        self.master_fd = master_fd

        # Disable input echo so sent commands do not duplicate in output
        os.write(self.master_fd, b"stty -echo\n")
        time.sleep(0.15)
        self._drain_nonblocking()

    def _drain_nonblocking(self, wait_sec: float = 0.02) -> None:
        while True:
            r, _, _ = select.select([self.master_fd], [], [], wait_sec)
            if not r:
                break
            try:
                if not os.read(self.master_fd, 65536):
                    break
            except OSError:
                break

    def ensure_alive(self) -> None:
        if self.proc is None or self.proc.poll() is not None:
            try:
                if self.master_fd >= 0:
                    os.close(self.master_fd)
            except OSError:
                pass
            self._spawn()

    def exec_command(self, command: str, timeout_sec: float = 60.0) -> dict:
        with self.lock:
            self.ensure_alive()
            self._drain_nonblocking(0.0)

            uid = uuid.uuid4().hex
            half = len(uid) // 2
            # Split token across adjacent quotes so even echoed input can never match sentinel
            sentinel_prefix = f"__MCP_END_{uid}__:"
            payload = (
                f"{command}\n"
                f"__mcp_ec=$?; printf '\\n__MCP_END_%s__:%d:%s\\n' "
                f"'{uid[:half]}''{uid[half:]}' \"$__mcp_ec\" \"$PWD\"\n"
            )

            t0 = time.monotonic()
            with self._live_lock:
                self._live_running = True
                self._live_cmd = command
                self._live_started = t0
                self._live_sentinel = sentinel_prefix
                self._live_buf.clear()

            os.write(self.master_fd, payload.encode("utf-8", errors="replace"))

            chunks: list[bytes] = []
            buf_tail = ""
            exit_code = -1
            timed_out = False
            deadline = t0 + max(1.0, float(timeout_sec))

            try:
                while True:
                    rem = deadline - time.monotonic()
                    if rem <= 0:
                        timed_out = True
                        # Interrupt hung foreground command so PTY stays reusable
                        try:
                            os.write(self.master_fd, b"\x03\n")
                            time.sleep(0.2)
                            self._drain_nonblocking()
                        except OSError:
                            pass
                        break

                    r, _, _ = select.select([self.master_fd], [], [], min(0.25, rem))
                    if not r:
                        if self.proc and self.proc.poll() is not None:
                            break
                        continue

                    try:
                        data = os.read(self.master_fd, 65536)
                        if not data:
                            break
                        chunks.append(data)
                        with self._live_lock:
                            self._live_buf.extend(data)
                            if len(self._live_buf) > 24576:
                                del self._live_buf[:-24576]
                        buf_tail = (buf_tail + data.decode("utf-8", errors="replace"))[-4096:]
                        if sentinel_prefix in buf_tail:
                            m = re.search(
                                rf"{re.escape(sentinel_prefix)}(-?\d+):([^\r\n]*)",
                                buf_tail,
                            )
                            if m:
                                exit_code = int(m.group(1))
                                self.cwd = m.group(2).strip() or self.cwd
                                break
                    except BlockingIOError:
                        continue
                    except OSError:
                        break
            finally:
                with self._live_lock:
                    self._live_running = False
                    self._live_cmd = ""
                    self._live_sentinel = ""
                    self._live_buf.clear()

            raw_text = b"".join(chunks).decode("utf-8", errors="replace")
            idx = raw_text.find(sentinel_prefix)
            if idx != -1:
                raw_text = raw_text[:idx]

            cleaned = clean_tqdm(raw_text).strip("\n")
            if len(cleaned) > 60000:
                cleaned = (
                    cleaned[:15000]
                    + "\n\n... [truncated middle output] ...\n\n"
                    + cleaned[-40000:]
                )

            return {
                "session": self.name,
                "exit_code": exit_code,
                "cwd": self.cwd,
                "timed_out": timed_out,
                "duration_sec": round(time.monotonic() - t0, 3),
                "output": cleaned,
            }

    def live_snapshot(self) -> dict | None:
        with self._live_lock:
            if not self._live_running:
                return None
            cmd = self._live_cmd
            elapsed = round(time.monotonic() - self._live_started, 1)
            raw = bytes(self._live_buf)
            sent = self._live_sentinel
        txt = raw.decode("utf-8", errors="replace")
        if sent:
            idx = txt.find(sent)
            if idx != -1:
                txt = txt[:idx]
        cleaned = clean_tqdm(txt).strip("\n")
        lines = cleaned.splitlines()[-60:]
        tail = "\n".join(lines)[-8000:]
        return {
            "running": True,
            "session": self.name,
            "command": cmd,
            "elapsed_sec": elapsed,
            "output": tail,
        }

    def interrupt(self) -> bool:
        with self._live_lock:
            if not self._live_running or self.master_fd < 0:
                return False
            fd = self.master_fd
        try:
            os.write(fd, b"\x03")
            return True
        except OSError:
            return False

    def close(self) -> None:
        try:
            if self.proc and self.proc.poll() is None:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
        except Exception:
            pass
        try:
            if self.master_fd >= 0:
                os.close(self.master_fd)
        except OSError:
            pass


class PtyManager:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._sessions: dict[str, PtySession] = {}

    def get(self, name: str = "default") -> PtySession:
        with self._lock:
            if name not in self._sessions:
                self._sessions[name] = PtySession(name)
            return self._sessions[name]

    def names(self) -> list[str]:
        with self._lock:
            return list(self._sessions.keys())

    def live_snapshot(self) -> dict | None:
        with self._lock:
            sessions = list(self._sessions.values())
        for s in sessions:
            snap = s.live_snapshot()
            if snap is not None:
                return snap
        return None

    def interrupt(self, name: str = "default") -> bool:
        with self._lock:
            target = self._sessions.get(name)
            sessions = list(self._sessions.values())
        if target is not None and target.interrupt():
            return True
        for s in sessions:
            if s.interrupt():
                return True
        return False

    def close_all(self) -> None:
        with self._lock:
            for s in self._sessions.values():
                s.close()
            self._sessions.clear()


class JobManager:
    """Detached GPU/CPU process manager writing persistent logs to /kaggle/working/mcp_logs."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._jobs: dict[str, dict] = {}
        self._procs: dict[str, subprocess.Popen] = {}
        self._log_handles: dict[str, io.BufferedWriter] = {}

    def spawn(self, command: str, cwd: str = "", name: str = "", gpu_ids: str = "") -> dict:
        with self._lock:
            job_id = f"job_{int(time.time())}_{uuid.uuid4().hex[:6]}"
            work_cwd = cwd if (cwd and os.path.isdir(cwd)) else str(WORKING_DIR)
            log_path = LOG_DIR / f"{job_id}.log"
            fh = open(log_path, "wb")

            job_env = os.environ.copy()
            gpu_ids_clean = str(gpu_ids or "").strip()
            if gpu_ids_clean:
                job_env["CUDA_VISIBLE_DEVICES"] = gpu_ids_clean

            proc = subprocess.Popen(
                ["/bin/bash", "-c", command],
                cwd=work_cwd,
                env=job_env,
                stdin=subprocess.DEVNULL,
                stdout=fh,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

            meta = {
                "job_id": job_id,
                "name": name or job_id,
                "command": command,
                "cwd": work_cwd,
                "gpu_ids": gpu_ids_clean,
                "pid": proc.pid,
                "status": "running",
                "exit_code": None,
                "started_at": time.time(),
                "ended_at": None,
                "log_path": str(log_path),
            }
            self._jobs[job_id] = meta
            self._procs[job_id] = proc
            self._log_handles[job_id] = fh
            self._persist_index()
            return dict(meta)

    def _refresh_locked(self) -> None:
        changed = False
        for job_id, proc in list(self._procs.items()):
            rc = proc.poll()
            meta = self._jobs[job_id]
            if rc is not None and meta["status"] == "running":
                meta["status"] = "completed" if rc == 0 else "failed"
                meta["exit_code"] = rc
                meta["ended_at"] = time.time()
                fh = self._log_handles.pop(job_id, None)
                if fh:
                    try:
                        fh.flush()
                        fh.close()
                    except Exception:
                        pass
                changed = True
        if changed:
            self._persist_index()

    def _persist_index(self) -> None:
        try:
            idx_file = LOG_DIR / "jobs_index.json"
            idx_file.write_text(json.dumps(list(self._jobs.values()), indent=2), encoding="utf-8")
        except Exception:
            pass

    def list_jobs(self) -> list[dict]:
        with self._lock:
            self._refresh_locked()
            now = time.time()
            out = []
            for meta in self._jobs.values():
                item = dict(meta)
                end_t = item["ended_at"] or now
                item["runtime_sec"] = round(end_t - item["started_at"], 1)
                lp = Path(item["log_path"])
                item["log_size_kb"] = round(lp.stat().st_size / 1024, 1) if lp.exists() else 0.0
                out.append(item)
            return out

    def active_count(self) -> int:
        with self._lock:
            self._refresh_locked()
            return sum(1 for m in self._jobs.values() if m["status"] == "running")

    def read_logs(self, job_id: str, tail_lines: int = 100, grep: str = "") -> dict:
        with self._lock:
            self._refresh_locked()
            if job_id not in self._jobs:
                raise KeyError(f"Unknown job_id: {job_id}")
            meta = dict(self._jobs[job_id])
            fh = self._log_handles.get(job_id)
            if fh:
                try:
                    fh.flush()
                except Exception:
                    pass

        lp = Path(meta["log_path"])
        if not lp.is_file():
            return {"job": meta, "logs": ""}

        raw = lp.read_bytes()
        if len(raw) > 2 * 1024 * 1024:
            raw = raw[-2 * 1024 * 1024 :]
        cleaned = clean_tqdm(raw.decode("utf-8", errors="replace"))
        lines = cleaned.splitlines()
        if grep:
            pat = re.compile(grep, re.IGNORECASE)
            lines = [ln for ln in lines if pat.search(ln)]
        tail = lines[-max(1, int(tail_lines)) :]
        return {"job": meta, "logs": "\n".join(tail), "returned_lines": len(tail)}

    def read_logs_chunk(self, job_id: str, offset: int = 0, max_bytes: int = 65536) -> dict:
        with self._lock:
            self._refresh_locked()
            if job_id not in self._jobs:
                raise KeyError(f"Unknown job_id: {job_id}")
            meta = dict(self._jobs[job_id])
            fh = self._log_handles.get(job_id)
            if fh:
                try:
                    fh.flush()
                except Exception:
                    pass

        lp = Path(meta["log_path"])
        if not lp.is_file():
            return {"job": meta, "chunk": "", "next_offset": 0}

        size = lp.stat().st_size
        start = max(0, int(offset))
        if start == 0 and size > max_bytes:
            start = size - max_bytes
        if start >= size:
            return {"job": meta, "chunk": "", "next_offset": size}

        with open(lp, "rb") as rf:
            rf.seek(start)
            data = rf.read(max(1024, int(max_bytes)))
        next_off = start + len(data)
        cleaned = clean_tqdm(data.decode("utf-8", errors="replace"))
        return {"job": meta, "chunk": cleaned, "next_offset": next_off}

    def kill_job(self, job_id: str, sig_name: str = "SIGTERM") -> dict:
        sig = getattr(signal, sig_name.upper(), signal.SIGTERM)
        with self._lock:
            self._refresh_locked()
            if job_id not in self._jobs:
                raise KeyError(f"Unknown job_id: {job_id}")
            proc = self._procs.get(job_id)
            if proc and proc.poll() is None:
                try:
                    os.killpg(os.getpgid(proc.pid), sig)
                except ProcessLookupError:
                    pass
        time.sleep(0.4)
        with self._lock:
            self._refresh_locked()
            return dict(self._jobs[job_id])

    def terminate_all_running(self) -> None:
        with self._lock:
            self._refresh_locked()
            running_ids = [jid for jid, m in self._jobs.items() if m["status"] == "running"]
        for jid in running_ids:
            try:
                self.kill_job(jid, "SIGTERM")
            except Exception:
                pass


class EventBus:
    """Thread-safe ring buffer capturing all MCP/CLI/TUI operations for live terminal visibility."""

    def __init__(self, maxlen: int = 250) -> None:
        self._lock = threading.Lock()
        self._seq = 0
        self._items: deque[dict] = deque(maxlen=maxlen)

    def push(
        self,
        caller: str,
        kind: str,
        summary: str,
        detail: str = "",
        exit_code: int | None = None,
        duration_sec: float | None = None,
    ) -> dict:
        with self._lock:
            self._seq += 1
            ev = {
                "seq": self._seq,
                "ts": round(time.time(), 3),
                "caller": caller or "mcp",
                "kind": kind,
                "summary": summary,
                "detail": detail[:1500] if detail else "",
                "exit_code": exit_code,
                "duration_sec": duration_sec,
            }
            self._items.append(ev)
            return ev

    def since(self, after_seq: int = 0) -> tuple[int, list[dict]]:
        with self._lock:
            items = [dict(e) for e in self._items if e["seq"] > after_seq]
            return self._seq, items


class WorkerState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.last_rpc_mono = time.monotonic()
        self.tunnel_url = ""
        self.ts_url = ""
        self.kaggle_sdk_ready = False
        self.pty_mgr = PtyManager()
        self.job_mgr = JobManager()
        self.events = EventBus()
        self.gpu_util_window: deque[float] = deque(maxlen=6)

    def touch_rpc(self) -> None:
        with self.lock:
            self.last_rpc_mono = time.monotonic()

    def idle_sec(self) -> float:
        with self.lock:
            return time.monotonic() - self.last_rpc_mono


STATE = WorkerState()


def ensure_kaggle_sdk_for_publish() -> None:
    if STATE.kaggle_sdk_ready:
        return
    if KAGGLE_SECRET.startswith("KGAT_"):
        os.environ["KAGGLE_API_TOKEN"] = KAGGLE_SECRET
        os.environ.pop("KAGGLE_KEY", None)
        # Ensure Kaggle SDK inside container supports KGAT_ bearer tokens + six
        subprocess.run(
            [sys.executable, "-m", "pip", "install", "-q", "-U", "kaggle>=2.0.0", "six"],
            check=False,
        )
    else:
        os.environ["KAGGLE_KEY"] = KAGGLE_SECRET
    os.environ["KAGGLE_USERNAME"] = KAGGLE_USERNAME
    STATE.kaggle_sdk_ready = True


def publish_state(status: str = "running") -> None:
    """Publish current tunnel URL and session heartbeat to private Kaggle Dataset."""
    try:
        ensure_kaggle_sdk_for_publish()
        from kaggle.api.kaggle_api_extended import KaggleApi

        ds_dir = TMP_DIR / "state_ds"
        ds_dir.mkdir(parents=True, exist_ok=True)

        meta = {
            "title": STATE_DATASET_REF.split("/")[-1],
            "id": STATE_DATASET_REF,
            "licenses": [{"name": "CC0-1.0"}],
        }
        (ds_dir / "dataset-metadata.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")

        state_payload = {
            "session_id": SESSION_ID,
            "status": status,
            "tunnel_url": STATE.tunnel_url,
            "ts_url": STATE.ts_url,
            "started_at": START_WALL,
            "updated_at": time.time(),
            "gpus": query_gpus(),
        }
        (ds_dir / "state.json").write_text(json.dumps(state_payload, indent=2), encoding="utf-8")

        api = KaggleApi()
        api.authenticate()
        try:
            api.dataset_create_version(
                folder=str(ds_dir),
                version_notes=f"{status}:{SESSION_ID[:8]}",
                quiet=True,
                convert_to_csv=False,
                dir_mode="skip",
            )
        except Exception:
            api.dataset_create_new(
                folder=str(ds_dir),
                public=False,
                quiet=True,
                convert_to_csv=False,
                dir_mode="skip",
            )
        print(f"[worker] published state ({status}) -> {STATE_DATASET_REF}", flush=True)
    except Exception as exc:
        print(f"[worker] publish_state error: {exc}", flush=True)


def start_tailscale_if_configured() -> None:
    if not TS_AUTHKEY:
        return
    try:
        ts_dir = TMP_DIR / "ts"
        ts_dir.mkdir(parents=True, exist_ok=True)
        tgz = ts_dir / "ts.tgz"
        urllib.request.urlretrieve(
            "https://pkgs.tailscale.com/stable/tailscale_latest_amd64.tgz",
            tgz,
        )
        subprocess.run(["tar", "-xzf", str(tgz), "-C", str(ts_dir)], check=True)
        extracted = next(ts_dir.glob("tailscale_*_amd64"))
        sock = str(TMP_DIR / "tailscaled.sock")
        state_dir = str(TMP_DIR / "ts-state")

        subprocess.Popen(
            [
                str(extracted / "tailscaled"),
                "--tun=userspace-networking",
                f"--statedir={state_dir}",
                f"--socket={sock}",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        time.sleep(2.0)
        subprocess.run(
            [
                str(extracted / "tailscale"),
                f"--socket={sock}",
                "up",
                f"--auth-key={TS_AUTHKEY}",
                f"--hostname={TS_HOSTNAME}",
                "--accept-dns=false",
            ],
            check=True,
            timeout=30,
        )
        ip_res = subprocess.run(
            [str(extracted / "tailscale"), f"--socket={sock}", "ip", "-4"],
            capture_output=True,
            text=True,
            check=True,
        )
        ts_ip = ip_res.stdout.strip().splitlines()[0].strip()
        if ts_ip:
            STATE.ts_url = f"http://{ts_ip}:{RPC_PORT}"
            print(f"[worker] tailscale ready: {STATE.ts_url}", flush=True)
            publish_state("running")
    except Exception as exc:
        print(f"[worker] tailscale setup warning: {exc}", flush=True)


def cloudflared_supervisor() -> None:
    cf_bin = TMP_DIR / "cloudflared"
    if not cf_bin.is_file():
        urllib.request.urlretrieve(
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64",
            cf_bin,
        )
        cf_bin.chmod(0o755)

    while not SHUTDOWN_EVENT.is_set():
        # Prefer QUIC UDP edge connection (zero TCP HoL blocking); fall back to auto HTTP/2 if UDP fails quickly
        proto_args = ["--protocol", "quic"] if not STATE.tunnel_url else []
        proc = subprocess.Popen(
            [
                str(cf_bin),
                "tunnel",
                "--no-autoupdate",
                *proto_args,
                "--url",
                f"http://127.0.0.1:{RPC_PORT}",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        t_start = time.monotonic()
        try:
            assert proc.stdout is not None
            for line in proc.stdout:
                if SHUTDOWN_EVENT.is_set():
                    break
                m = TRYCF_URL_RE.search(line)
                if m:
                    url = m.group(0)
                    if url != STATE.tunnel_url:
                        STATE.tunnel_url = url
                        print(f"[worker] cloudflared tunnel ready: {url}", flush=True)
                        publish_state("running")
        except Exception as exc:
            print(f"[worker] cloudflared stream error: {exc}", flush=True)
        finally:
            if proc.poll() is None:
                proc.terminate()
        if not SHUTDOWN_EVENT.is_set():
            if time.monotonic() - t_start < 6.0 and proto_args:
                STATE.tunnel_url = ""
            time.sleep(2.0)


def safe_extract_tar(tar_bytes: bytes, dest_dir: Path) -> int:
    dest_dir = dest_dir.resolve()
    dest_dir.mkdir(parents=True, exist_ok=True)
    count = 0
    with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r:gz") as tf:
        for member in tf.getmembers():
            target = (dest_dir / member.name).resolve()
            if not target.is_relative_to(dest_dir):
                raise ValueError(f"Blocked path traversal in archive: {member.name}")
            if member.issym() or member.islnk():
                continue
            tf.extract(member, path=dest_dir)
            count += 1
    return count


def safe_extract_tar_file(tar_path: Path, dest_dir: Path) -> int:
    dest_dir = dest_dir.resolve()
    dest_dir.mkdir(parents=True, exist_ok=True)
    count = 0
    with tarfile.open(tar_path, mode="r:*") as tf:
        for member in tf:
            target = (dest_dir / member.name).resolve()
            if not target.is_relative_to(dest_dir):
                raise ValueError(f"Blocked path traversal in archive: {member.name}")
            if member.issym() or member.islnk():
                continue
            tf.extract(member, path=dest_dir)
            count += 1
    return count


def safe_extract_zip(zip_path: Path, dest_dir: Path) -> int:
    dest_dir = dest_dir.resolve()
    dest_dir.mkdir(parents=True, exist_ok=True)
    count = 0
    with zipfile.ZipFile(zip_path, "r") as zf:
        for info in zf.infolist():
            target = (dest_dir / info.filename).resolve()
            if not target.is_relative_to(dest_dir):
                raise ValueError(f"Blocked path traversal in zip: {info.filename}")
            if ((info.external_attr >> 16) & 0o170000) == 0o120000:
                continue
            zf.extract(info, path=dest_dir)
            count += 1
    return count


def _assert_writable_path(raw_path: str, *, allow_root: bool = False) -> Path:
    raw = str(raw_path or "").strip()
    if not raw:
        raise PermissionError("Empty remote path.")
    target = Path(raw).resolve()
    roots = [WORKING_DIR.resolve(), Path("/tmp").resolve()]
    for root in roots:
        if target == root:
            if allow_root:
                return target
            raise PermissionError(f"Refused operation on sandbox root itself: {target}")
        if target.is_relative_to(root):
            return target
    raise PermissionError(f"Path outside writable sandbox (/kaggle/working, /tmp): {target}")


def compute_disk_usage(root_path: Path, max_sec: float = 3.0, max_entries: int = 20000) -> dict:
    root_path = root_path.resolve()
    if not root_path.exists():
        raise FileNotFoundError(f"Path not found: {root_path}")
    if root_path.is_file():
        sz = root_path.stat().st_size
        return {
            "path": str(root_path),
            "total_bytes": sz,
            "partial": False,
            "scanned_entries": 1,
            "children": [
                {
                    "name": root_path.name,
                    "path": str(root_path),
                    "is_dir": False,
                    "size_bytes": sz,
                    "pct": 100.0,
                }
            ],
        }

    deadline = time.monotonic() + max(0.2, min(10.0, float(max_sec)))
    budget_left = [max(100, int(max_entries))]
    partial = [False]

    def _dir_size(path_obj: Path) -> int:
        total = 0
        stack = [path_obj]
        while stack:
            if time.monotonic() >= deadline or budget_left[0] <= 0:
                partial[0] = True
                break
            cur = stack.pop()
            try:
                with os.scandir(cur) as it:
                    for entry in it:
                        budget_left[0] -= 1
                        try:
                            if entry.is_symlink():
                                continue
                            if entry.is_dir(follow_symlinks=False):
                                stack.append(Path(entry.path))
                            else:
                                total += entry.stat(follow_symlinks=False).st_size
                        except OSError:
                            continue
                        if budget_left[0] <= 0 or time.monotonic() >= deadline:
                            partial[0] = True
                            break
            except OSError:
                continue
        return total

    children: list[dict] = []
    try:
        with os.scandir(root_path) as it:
            top_entries = list(it)
    except OSError:
        top_entries = []

    for entry in top_entries:
        if time.monotonic() >= deadline or budget_left[0] <= 0:
            partial[0] = True
            break
        budget_left[0] -= 1
        try:
            is_dir = entry.is_dir(follow_symlinks=False)
            sz = _dir_size(Path(entry.path)) if is_dir else entry.stat(follow_symlinks=False).st_size
        except OSError:
            continue
        children.append(
            {
                "name": entry.name,
                "path": str(Path(entry.path)),
                "is_dir": is_dir,
                "size_bytes": sz,
                "pct": 0.0,
            }
        )

    children.sort(key=lambda c: c["size_bytes"], reverse=True)
    total_bytes = sum(c["size_bytes"] for c in children)
    denom = max(1, total_bytes)
    for c in children:
        c["pct"] = round((c["size_bytes"] / denom) * 100.0, 1)

    return {
        "path": str(root_path),
        "total_bytes": total_bytes,
        "partial": partial[0],
        "scanned_entries": max(100, int(max_entries)) - max(0, budget_left[0]),
        "children": children[:100],
    }


def inspect_remote_file(fp: Path) -> dict:
    fp = fp.resolve()
    if not fp.is_file():
        raise FileNotFoundError(f"File not found: {fp}")
    size = fp.stat().st_size
    lower = fp.name.lower()
    ext = fp.suffix.lower()

    if ext in {".csv", ".tsv"}:
        delim = "\t" if ext == ".tsv" else ","
        columns: list[str] = []
        rows: list[list[str]] = []
        row_count = 0
        estimated = False
        with open(fp, encoding="utf-8", errors="replace", newline="") as cf:
            reader = csv.reader(cf, delimiter=delim)
            try:
                columns = [str(c) for c in next(reader)]
            except StopIteration:
                columns = []
            for r in reader:
                row_count += 1
                if len(rows) < 25:
                    rows.append([str(v) for v in r[:40]])
                if size > 15 * 1024 * 1024 and row_count >= 2000:
                    pos = cf.buffer.tell() if hasattr(cf, "buffer") else 0
                    if pos > 0:
                        row_count = int((row_count / pos) * size)
                    estimated = True
                    break
        return {
            "path": str(fp),
            "kind": "table",
            "format": ext.lstrip("."),
            "size_bytes": size,
            "columns": columns[:40],
            "rows": rows,
            "row_count": row_count,
            "estimated": estimated,
        }

    if ext == ".parquet":
        try:
            import pyarrow.parquet as pq  # type: ignore

            pf = pq.ParquetFile(fp)
            row_count = pf.metadata.num_rows
            columns = list(pf.schema_arrow.names)[:40]
            rows: list[list[str]] = []
            batch = next(pf.iter_batches(batch_size=25, columns=columns or None), None)
            if batch is not None:
                py_dict = batch.to_pydict()
                n_rows = len(next(iter(py_dict.values()), []))
                for idx in range(min(25, n_rows)):
                    rows.append([str(py_dict[col][idx]) for col in columns])
            return {
                "path": str(fp),
                "kind": "table",
                "format": "parquet",
                "size_bytes": size,
                "columns": columns,
                "rows": rows,
                "row_count": row_count,
                "estimated": False,
            }
        except ImportError:
            pass
        if size < 50 * 1024 * 1024:
            try:
                import pandas as pd  # type: ignore

                df = pd.read_parquet(fp)
                columns = [str(c) for c in list(df.columns)[:40]]
                rows = [[str(v) for v in row] for row in df[columns].head(25).values.tolist()]
                return {
                    "path": str(fp),
                    "kind": "table",
                    "format": "parquet",
                    "size_bytes": size,
                    "columns": columns,
                    "rows": rows,
                    "row_count": len(df),
                    "estimated": False,
                }
            except Exception as exc:
                return {
                    "path": str(fp),
                    "kind": "binary_info",
                    "format": "parquet",
                    "size_bytes": size,
                    "note": f"Parquet preview unavailable: {exc}",
                }
        return {
            "path": str(fp),
            "kind": "binary_info",
            "format": "parquet",
            "size_bytes": size,
            "note": "pyarrow not installed and file >= 50 MB; skipped full read.",
        }

    if ext == ".zip":
        entries: list[dict] = []
        with zipfile.ZipFile(fp, "r") as zf:
            infos = zf.infolist()
            for info in infos[:200]:
                entries.append(
                    {
                        "name": info.filename,
                        "size_bytes": info.file_size,
                        "is_dir": info.is_dir(),
                    }
                )
            total_entries = len(infos)
        return {
            "path": str(fp),
            "kind": "archive",
            "format": "zip",
            "size_bytes": size,
            "total_entries": total_entries,
            "truncated": total_entries > 200,
            "entries": entries,
        }

    if lower.endswith((".tar.gz", ".tgz", ".tar")):
        entries = []
        total_entries = 0
        with tarfile.open(fp, "r:*") as tf:
            for member in tf:
                total_entries += 1
                if len(entries) < 200:
                    entries.append(
                        {
                            "name": member.name,
                            "size_bytes": member.size,
                            "is_dir": member.isdir(),
                        }
                    )
        return {
            "path": str(fp),
            "kind": "archive",
            "format": "tar",
            "size_bytes": size,
            "total_entries": total_entries,
            "truncated": total_entries > 200,
            "entries": entries,
        }

    if ext == ".safetensors":
        with open(fp, "rb") as sf:
            len_raw = sf.read(8)
            if len(len_raw) < 8:
                raise ValueError("Invalid safetensors file (<8 bytes).")
            header_len = struct.unpack("<Q", len_raw)[0]
            if header_len > 4 * 1024 * 1024:
                return {
                    "path": str(fp),
                    "kind": "binary_info",
                    "format": "safetensors",
                    "size_bytes": size,
                    "note": f"Header size {header_len} bytes exceeds 4 MB cap.",
                }
            header = json.loads(sf.read(header_len).decode("utf-8"))
        meta = header.get("__metadata__", {}) if isinstance(header, dict) else {}
        tensors: list[dict] = []
        all_keys = [k for k in header.keys() if k != "__metadata__"] if isinstance(header, dict) else []
        for k in all_keys[:400]:
            info = header[k]
            if isinstance(info, dict):
                tensors.append(
                    {
                        "name": k,
                        "dtype": info.get("dtype", "?"),
                        "shape": info.get("shape", []),
                    }
                )
        return {
            "path": str(fp),
            "kind": "tensors",
            "format": "safetensors",
            "size_bytes": size,
            "total_tensors": len(all_keys),
            "truncated": len(all_keys) > 400,
            "metadata": meta,
            "tensors": tensors,
        }

    if ext in {".pt", ".pth", ".bin"}:
        if zipfile.is_zipfile(fp):
            entries = []
            with zipfile.ZipFile(fp, "r") as zf:
                infos = zf.infolist()
                for info in infos[:200]:
                    entries.append(
                        {
                            "name": info.filename,
                            "size_bytes": info.file_size,
                            "is_dir": info.is_dir(),
                        }
                    )
                total_entries = len(infos)
            return {
                "path": str(fp),
                "kind": "archive",
                "format": "pytorch_zip",
                "size_bytes": size,
                "total_entries": total_entries,
                "truncated": total_entries > 200,
                "entries": entries,
            }
        return {
            "path": str(fp),
            "kind": "binary_info",
            "format": ext.lstrip("."),
            "size_bytes": size,
            "note": (
                "Legacy/raw binary checkpoint (non-zip). "
                "torch.load skipped automatically for memory/pickle safety."
            ),
        }

    return {
        "path": str(fp),
        "kind": "binary_info",
        "format": ext.lstrip(".") or "file",
        "size_bytes": size,
        "note": "No structured inspector for this extension.",
    }


def build_tar_bytes(target_path: Path) -> bytes:
    target_path = target_path.resolve()
    if not target_path.exists():
        raise FileNotFoundError(f"Remote path not found: {target_path}")
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        tf.add(str(target_path), arcname=target_path.name)
    return buf.getvalue()


class FastHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 64


class WorkerHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    # Enable TCP_NODELAY immediately on every accepted socket (cuts 40-200ms Nagle stalls)
    disable_nagle_algorithm = True

    def log_message(self, fmt: str, *args) -> None:
        return

    def _send_json(self, code: int, data: dict) -> None:
        body = json.dumps(data, separators=(",", ":")).encode("utf-8")
        use_gzip = len(body) >= 512 and "gzip" in (self.headers.get("Accept-Encoding") or "").lower()
        if use_gzip:
            body = gzip.compress(body, compresslevel=4)
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        if use_gzip:
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Vary", "Accept-Encoding")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _check_auth(self, touch_idle: bool = True) -> bool:
        auth = self.headers.get("Authorization", "")
        expected = f"Bearer {WORKER_TOKEN}"
        if not hmac.compare_digest(auth, expected):
            self._send_json(401, {"error": "Unauthorized"})
            return False
        if touch_idle:
            STATE.touch_rpc()
        return True

    def _caller(self) -> str:
        return (self.headers.get("X-Kgsh-Caller") or "mcp").strip().lower()[:16]

    _hw_lock = threading.Lock()
    _hw_cache_ts: float = 0.0
    _hw_cache: dict = {}

    def _build_status_dict(self) -> dict:
        uptime = time.monotonic() - START_MONO
        now_mono = time.monotonic()
        with WorkerHandler._hw_lock:
            if now_mono - WorkerHandler._hw_cache_ts >= 1.2 or not WorkerHandler._hw_cache:
                try:
                    cores = max(1, os.cpu_count() or 1)
                    cpu_pct = round(min(100.0, (os.getloadavg()[0] / cores) * 100.0), 1)
                except Exception:
                    cpu_pct = 0.0
                WorkerHandler._hw_cache = {
                    "cpu_pct": cpu_pct,
                    "gpus": query_gpus(),
                    "gpu_processes": query_gpu_processes(),
                    "tpu": query_tpu(),
                    "ram": query_ram_gb(),
                    "disk_working": query_disk_gb(WORKING_DIR),
                    "disk_tmp": query_disk_gb(Path("/tmp")),
                }
                WorkerHandler._hw_cache_ts = now_mono
            hw = dict(WorkerHandler._hw_cache)
        return {
            "session_id": SESSION_ID,
            "uptime_sec": round(uptime, 1),
            "remaining_before_drain_sec": round(max(0.0, MAX_LIFETIME_SEC - uptime), 1),
            "idle_sec": round(STATE.idle_sec(), 1),
            "idle_timeout_sec": IDLE_TIMEOUT_SEC,
            "tunnel_url": STATE.tunnel_url,
            "ts_url": STATE.ts_url,
            **hw,
            "active_jobs": STATE.job_mgr.active_count(),
            "pty_sessions": STATE.pty_mgr.names(),
        }

    def _read_json(self) -> dict:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length > 0 else b"{}"
        return json.loads(raw.decode("utf-8"))

    def do_GET(self) -> None:
        if self.path == "/ping":
            self._send_json(
                200,
                {
                    "ok": True,
                    "session_id": SESSION_ID,
                    "uptime_sec": round(time.monotonic() - START_MONO, 1),
                },
            )
            return

        if not self._check_auth():
            return

        if self.path == "/status":
            self._send_json(200, self._build_status_dict())
            return

        if self.path == "/jobs/list":
            self._send_json(200, {"jobs": STATE.job_mgr.list_jobs()})
            return

        self._send_json(404, {"error": f"Unknown path: {self.path}"})

    def do_POST(self) -> None:
        is_monitor = self.path in {"/monitor", "/fs/list", "/fs/read_text", "/fs/du", "/fs/inspect", "/fs/find"}
        if not self._check_auth(touch_idle=not is_monitor):
            return

        caller = self._caller()

        try:
            if self.path == "/fs/list":
                req = self._read_json()
                target_dir = Path(req.get("path") or str(WORKING_DIR))
                if not target_dir.is_dir():
                    self._send_json(200, {"path": str(target_dir), "entries": []})
                    return
                entries = []
                for child in sorted(target_dir.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower()))[:200]:
                    try:
                        st = child.stat()
                        entries.append(
                            {
                                "name": child.name,
                                "path": str(child),
                                "is_dir": child.is_dir(),
                                "size_bytes": 0 if child.is_dir() else st.st_size,
                                "mtime": round(st.st_mtime, 2),
                            }
                        )
                    except OSError:
                        continue
                self._send_json(200, {"path": str(target_dir), "entries": entries})
                return

            if self.path == "/fs/read_text":
                req = self._read_json()
                fp = Path(req["path"])
                max_b = min(65536, max(1024, int(req.get("max_bytes", 32768))))
                if not fp.is_file():
                    self._send_json(404, {"error": f"File not found: {fp}"})
                    return
                size = fp.stat().st_size
                with open(fp, "rb") as rf:
                    raw_b = rf.read(max_b)
                self._send_json(
                    200,
                    {
                        "path": str(fp),
                        "size_bytes": size,
                        "truncated": size > max_b,
                        "content": raw_b.decode("utf-8", errors="replace"),
                    },
                )
                return

            if self.path == "/fs/write_text":
                req = self._read_json()
                fp = Path(req["path"])
                raw_bytes = str(req.get("content", "")).encode("utf-8")
                fp.parent.mkdir(parents=True, exist_ok=True)
                fp.write_bytes(raw_bytes)
                STATE.events.push(
                    caller=caller,
                    kind="fs_write",
                    summary=f"fs_write -> {fp} ({round(len(raw_bytes)/1024, 1)} KB)",
                )
                self._send_json(
                    200,
                    {"ok": True, "path": str(fp), "size_bytes": len(raw_bytes)},
                )
                return

            if self.path == "/fs/mkdir":
                req = self._read_json()
                dp = Path(req["path"])
                dp.mkdir(parents=True, exist_ok=True)
                self._send_json(200, {"ok": True, "path": str(dp)})
                return

            if self.path == "/fs/delete":
                req = self._read_json()
                target = Path(req["path"]).resolve()
                allowed_roots = (str(WORKING_DIR.resolve()) + "/", "/tmp/")
                if not any(str(target).startswith(r) for r in allowed_roots):
                    self._send_json(403, {"error": f"Refused delete outside working/tmp: {target}"})
                    return
                if not target.exists():
                    self._send_json(404, {"error": f"Not found: {target}"})
                    return
                if target.is_dir():
                    shutil.rmtree(target)
                else:
                    target.unlink()
                STATE.events.push(
                    caller=caller,
                    kind="fs_delete",
                    summary=f"fs_delete {target}",
                )
                self._send_json(200, {"ok": True, "path": str(target)})
                return

            if self.path == "/fs/rename":
                req = self._read_json()
                src_p = _assert_writable_path(req["src"])
                dst_p = _assert_writable_path(req["dst"])
                if not src_p.exists():
                    raise FileNotFoundError(f"Source not found: {src_p}")
                dst_p.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(str(src_p), str(dst_p))
                STATE.events.push(
                    caller=caller,
                    kind="fs_rename",
                    summary=f"fs_rename {src_p.name} -> {dst_p}",
                )
                self._send_json(200, {"ok": True, "src": str(src_p), "dst": str(dst_p)})
                return

            if self.path == "/fs/copy":
                req = self._read_json()
                src_p = Path(req["src"]).resolve()
                dst_p = _assert_writable_path(req["dst"])
                if not src_p.exists():
                    raise FileNotFoundError(f"Source not found: {src_p}")
                if src_p.is_dir():
                    if dst_p == src_p or dst_p.is_relative_to(src_p):
                        raise PermissionError("Cannot copy directory into itself.")
                    shutil.copytree(src_p, dst_p, dirs_exist_ok=True)
                else:
                    dst_p.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(src_p, dst_p)
                STATE.events.push(
                    caller=caller,
                    kind="fs_copy",
                    summary=f"fs_copy {src_p.name} -> {dst_p}",
                )
                self._send_json(200, {"ok": True, "src": str(src_p), "dst": str(dst_p)})
                return

            if self.path == "/fs/du":
                req = self._read_json()
                target = Path(req.get("path") or str(WORKING_DIR)).resolve()
                result = compute_disk_usage(
                    target,
                    max_sec=float(req.get("max_sec", 3.0)),
                    max_entries=int(req.get("max_entries", 20000)),
                )
                self._send_json(200, result)
                return

            if self.path == "/fs/inspect":
                req = self._read_json()
                fp = Path(req["path"])
                self._send_json(200, inspect_remote_file(fp))
                return

            if self.path == "/fs/extract":
                req = self._read_json()
                src_p = Path(req["src"]).resolve()
                if not src_p.is_file():
                    raise FileNotFoundError(f"Archive not found: {src_p}")
                raw_dest = str(req.get("dest_dir") or "").strip()
                if not raw_dest:
                    stem = src_p.name
                    for suf in (".tar.gz", ".tgz", ".tar", ".zip"):
                        if stem.lower().endswith(suf):
                            stem = stem[: -len(suf)]
                            break
                    raw_dest = str(src_p.parent / (stem or "extracted"))
                dest_p = _assert_writable_path(raw_dest, allow_root=True)
                if src_p.name.lower().endswith(".zip"):
                    count = safe_extract_zip(src_p, dest_p)
                elif src_p.name.lower().endswith((".tar.gz", ".tgz", ".tar")):
                    count = safe_extract_tar_file(src_p, dest_p)
                else:
                    raise ValueError(f"Unsupported archive format: {src_p.name}")
                STATE.events.push(
                    caller=caller,
                    kind="fs_extract",
                    summary=f"fs_extract {src_p.name} -> {dest_p} ({count} entries)",
                )
                self._send_json(200, {"ok": True, "src": str(src_p), "dest_dir": str(dest_p), "count": count})
                return

            if self.path == "/fs/archive":
                req = self._read_json()
                raw_paths = req.get("paths") or []
                if not raw_paths:
                    raise ValueError("paths must not be empty.")
                sources: list[Path] = []
                for rp in raw_paths:
                    sp = Path(rp).resolve()
                    if not sp.exists():
                        raise FileNotFoundError(f"Not found: {sp}")
                    sources.append(sp)
                default_dest = str(WORKING_DIR / f"bundle_{time.strftime('%Y%m%d_%H%M%S')}.tar.gz")
                dest_p = _assert_writable_path(str(req.get("dest_path") or default_dest))
                dest_p.parent.mkdir(parents=True, exist_ok=True)
                used_arcs: set[str] = set()
                with tarfile.open(dest_p, "w:gz") as tf:
                    for idx, sp in enumerate(sources):
                        arc = sp.name or f"item_{idx}"
                        if arc in used_arcs:
                            arc = f"{idx}_{arc}"
                        used_arcs.add(arc)
                        tf.add(str(sp), arcname=arc)
                sz = dest_p.stat().st_size
                STATE.events.push(
                    caller=caller,
                    kind="fs_archive",
                    summary=f"fs_archive {len(sources)} items -> {dest_p.name} ({round(sz / 1024, 1)} KB)",
                )
                self._send_json(
                    200,
                    {"ok": True, "path": str(dest_p), "size_bytes": sz, "count": len(sources)},
                )
                return

            if self.path == "/fs/find":
                req = self._read_json()
                root = Path(req.get("root") or str(WORKING_DIR)).resolve()
                if not root.exists():
                    self._send_json(200, {"root": str(root), "matches": [], "truncated": False})
                    return
                name_glob = str(req.get("name_glob") or "").strip()
                content_regex = str(req.get("content_regex") or "").strip()
                max_results = min(300, max(1, int(req.get("max_results", 150))))
                rx = re.compile(content_regex, re.IGNORECASE) if content_regex else None
                skip_dirs = {".git", "__pycache__", "node_modules", ".venv"}
                matches: list[dict] = []
                truncated = False
                for dirpath, dirnames, filenames in os.walk(root):
                    dirnames[:] = [d for d in dirnames if d not in skip_dirs]
                    for fn in filenames:
                        if name_glob and not fnmatch.fnmatch(fn.lower(), name_glob.lower()):
                            continue
                        fp = Path(dirpath) / fn
                        try:
                            st = fp.stat()
                        except OSError:
                            continue
                        if rx is None:
                            matches.append(
                                {
                                    "path": str(fp),
                                    "name": fn,
                                    "is_dir": False,
                                    "size_bytes": st.st_size,
                                    "line": 0,
                                    "snippet": "",
                                }
                            )
                        else:
                            if st.st_size > 2 * 1024 * 1024:
                                continue
                            try:
                                with open(fp, "rb") as rf:
                                    head = rf.read(4096)
                                if b"\x00" in head:
                                    continue
                                with open(fp, encoding="utf-8", errors="replace") as tf:
                                    for lno, line_txt in enumerate(tf, 1):
                                        if rx.search(line_txt):
                                            matches.append(
                                                {
                                                    "path": str(fp),
                                                    "name": fn,
                                                    "is_dir": False,
                                                    "size_bytes": st.st_size,
                                                    "line": lno,
                                                    "snippet": line_txt.strip()[:160],
                                                }
                                            )
                                            break
                            except OSError:
                                continue
                        if len(matches) >= max_results:
                            truncated = True
                            break
                    if truncated:
                        break
                self._send_json(
                    200,
                    {"root": str(root), "matches": matches, "truncated": truncated},
                )
                return

            if self.path == "/fs/publish_dataset":
                req = self._read_json()
                src_dir = Path(req["source_dir"]).resolve()
                if not src_dir.is_dir():
                    raise FileNotFoundError(f"Directory not found: {src_dir}")
                raw_slug = str(req.get("slug") or src_dir.name).lower()
                slug = re.sub(r"[^a-z0-9-]+", "-", raw_slug).strip("-") or "kaggle-dataset"
                title = str(req.get("title") or slug).strip()
                notes = str(req.get("version_notes") or "Published via kgsh").strip()
                public = bool(req.get("public", False))
                ds_ref = f"{KAGGLE_USERNAME}/{slug}"

                ensure_kaggle_sdk_for_publish()
                from kaggle.api.kaggle_api_extended import KaggleApi

                stage_dir = TMP_DIR / f"pub_{uuid.uuid4().hex[:8]}"
                stage_dir.mkdir(parents=True, exist_ok=True)
                try:
                    for item in src_dir.iterdir():
                        if item.name == "dataset-metadata.json":
                            continue
                        dst_item = stage_dir / item.name
                        if item.is_dir():
                            shutil.copytree(item, dst_item, dirs_exist_ok=True)
                        else:
                            try:
                                os.link(item, dst_item)
                            except OSError:
                                shutil.copy2(item, dst_item)
                    meta = {
                        "title": title,
                        "id": ds_ref,
                        "licenses": [{"name": "CC0-1.0"}],
                    }
                    (stage_dir / "dataset-metadata.json").write_text(
                        json.dumps(meta, indent=2),
                        encoding="utf-8",
                    )
                    api = KaggleApi()
                    api.authenticate()
                    try:
                        api.dataset_create_version(
                            folder=str(stage_dir),
                            version_notes=notes,
                            quiet=True,
                            convert_to_csv=False,
                            dir_mode="zip",
                        )
                    except Exception:
                        api.dataset_create_new(
                            folder=str(stage_dir),
                            public=public,
                            quiet=True,
                            convert_to_csv=False,
                            dir_mode="zip",
                        )
                finally:
                    shutil.rmtree(stage_dir, ignore_errors=True)

                STATE.events.push(
                    caller=caller,
                    kind="fs_publish",
                    summary=f"publish_dataset {src_dir.name} -> {ds_ref}",
                )
                self._send_json(
                    200,
                    {
                        "ok": True,
                        "dataset_ref": ds_ref,
                        "url": f"https://www.kaggle.com/datasets/{ds_ref}",
                    },
                )
                return

            if self.path == "/proc/kill":
                req = self._read_json()
                pid = int(req["pid"])
                sig_name = str(req.get("sig") or "SIGTERM").strip().upper()
                if pid <= 1 or pid == os.getpid():
                    raise PermissionError(f"Refused signal to protected PID {pid}")
                sig_num = getattr(signal, "SIGKILL", signal.SIGTERM) if sig_name == "SIGKILL" else signal.SIGTERM
                os.kill(pid, sig_num)
                STATE.events.push(
                    caller=caller,
                    kind="proc_kill",
                    summary=f"proc_kill PID {pid} ({sig_name})",
                )
                self._send_json(200, {"ok": True, "pid": pid, "sig": sig_name})
                return

            if self.path == "/monitor":
                req = self._read_json()
                latest_seq, new_events = STATE.events.since(int(req.get("since_seq", 0)))
                log_chunk = None
                log_jid = str(req.get("log_job_id") or "").strip()
                if log_jid:
                    try:
                        log_chunk = STATE.job_mgr.read_logs_chunk(
                            job_id=log_jid,
                            offset=int(req.get("log_offset", 0)),
                        )
                    except KeyError:
                        log_chunk = None
                self._send_json(
                    200,
                    {
                        "status": self._build_status_dict(),
                        "jobs": STATE.job_mgr.list_jobs(),
                        "latest_seq": latest_seq,
                        "events": new_events,
                        "log_chunk": log_chunk,
                        "pty_live": STATE.pty_mgr.live_snapshot(),
                    },
                )
                return

            if self.path == "/term/exec":
                req = self._read_json()
                res = STATE.pty_mgr.get(req.get("session", "default")).exec_command(
                    command=req["command"],
                    timeout_sec=float(req.get("timeout_sec", 60.0)),
                )
                STATE.events.push(
                    caller=caller,
                    kind="term_exec",
                    summary=f"$ {req['command']}",
                    detail=res.get("output", ""),
                    exit_code=res.get("exit_code"),
                    duration_sec=res.get("duration_sec"),
                )
                self._send_json(200, res)
                return

            if self.path == "/term/interrupt":
                req = self._read_json()
                sess_name = str(req.get("session") or "default")
                sent = STATE.pty_mgr.interrupt(sess_name)
                if sent:
                    STATE.events.push(
                        caller=caller,
                        kind="term_interrupt",
                        summary=f"^C interrupt ({sess_name})",
                    )
                self._send_json(
                    200,
                    {"ok": True, "interrupted": sent, "session": sess_name},
                )
                return

            if self.path == "/jobs/spawn":
                req = self._read_json()
                meta = STATE.job_mgr.spawn(
                    command=req["command"],
                    cwd=req.get("cwd", ""),
                    name=req.get("name", ""),
                    gpu_ids=str(req.get("gpu_ids", "")),
                )
                gpu_tag = f" [gpu={meta['gpu_ids']}]" if meta.get("gpu_ids") else ""
                STATE.events.push(
                    caller=caller,
                    kind="job_spawn",
                    summary=f"job_spawn ({meta['name']}){gpu_tag}: {req['command']}",
                    detail=f"job_id={meta['job_id']} pid={meta['pid']}",
                )
                self._send_json(200, meta)
                return

            if self.path == "/jobs/logs":
                req = self._read_json()
                res = STATE.job_mgr.read_logs(
                    job_id=req["job_id"],
                    tail_lines=int(req.get("tail_lines", 100)),
                    grep=req.get("grep", ""),
                )
                self._send_json(200, res)
                return

            if self.path == "/jobs/logs_chunk":
                req = self._read_json()
                res = STATE.job_mgr.read_logs_chunk(
                    job_id=req["job_id"],
                    offset=int(req.get("offset", 0)),
                    max_bytes=int(req.get("max_bytes", 65536)),
                )
                self._send_json(200, res)
                return

            if self.path == "/jobs/kill":
                req = self._read_json()
                meta = STATE.job_mgr.kill_job(
                    job_id=req["job_id"],
                    sig_name=req.get("signal", "SIGTERM"),
                )
                STATE.events.push(
                    caller=caller,
                    kind="job_kill",
                    summary=f"job_kill {req['job_id']} ({req.get('signal', 'SIGTERM')})",
                )
                self._send_json(200, meta)
                return

            if self.path == "/fs/upload":
                remote_dest = Path(self.headers.get("X-Remote-Path", str(WORKING_DIR)))
                length = int(self.headers.get("Content-Length", "0"))
                tar_bytes = self.rfile.read(length)
                extracted = safe_extract_tar(tar_bytes, remote_dest)
                STATE.events.push(
                    caller=caller,
                    kind="fs_push",
                    summary=f"fs_push -> {remote_dest} ({extracted} files, {round(len(tar_bytes)/1024, 1)} KB)",
                )
                self._send_json(
                    200,
                    {"ok": True, "extracted_entries": extracted, "remote_path": str(remote_dest)},
                )
                return

            if self.path == "/fs/download":
                req = self._read_json()
                target = Path(req["remote_path"])
                tar_bytes = build_tar_bytes(target)
                STATE.events.push(
                    caller=caller,
                    kind="fs_pull",
                    summary=f"fs_pull <- {target} ({round(len(tar_bytes)/1024, 1)} KB)",
                )
                self.send_response(200)
                self.send_header("Content-Type", "application/gzip")
                self.send_header("Content-Length", str(len(tar_bytes)))
                self.send_header("X-Target-Name", target.name)
                self.end_headers()
                self.wfile.write(tar_bytes)
                return

            if self.path == "/shutdown":
                STATE.events.push(caller=caller, kind="shutdown", summary="Graceful worker shutdown triggered")
                STATE.job_mgr.terminate_all_running()
                self._send_json(200, {"ok": True, "shutting_down": True})
                threading.Timer(0.5, SHUTDOWN_EVENT.set).start()
                return

            self._send_json(404, {"error": f"Unknown path: {self.path}"})
        except PermissionError as exc:
            self._send_json(403, {"error": str(exc)})
        except (FileNotFoundError, KeyError) as exc:
            self._send_json(404, {"error": str(exc)})
        except Exception as exc:
            self._send_json(500, {"error": f"{type(exc).__name__}: {exc}"})


def main() -> None:
    fixup_cuda_env()
    print(f"[worker] booting session={SESSION_ID} on port {RPC_PORT}", flush=True)

    # Pre-warm Kaggle SDK upgrade in parallel with cloudflared startup
    sdk_thread = threading.Thread(target=ensure_kaggle_sdk_for_publish, daemon=True)
    sdk_thread.start()

    httpd = FastHTTPServer(("0.0.0.0", RPC_PORT), WorkerHandler)
    srv_thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    srv_thread.start()

    cf_thread = threading.Thread(target=cloudflared_supervisor, daemon=True)
    cf_thread.start()

    if TS_AUTHKEY:
        ts_thread = threading.Thread(target=start_tailscale_if_configured, daemon=True)
        ts_thread.start()

    drained = False
    last_gpu_sample = 0.0

    try:
        while not SHUTDOWN_EVENT.wait(5.0):
            now_mono = time.monotonic()
            elapsed = now_mono - START_MONO

            # Sample GPU utilization every 60s for idle quota protection
            if now_mono - last_gpu_sample >= 60.0:
                last_gpu_sample = now_mono
                gpus = query_gpus()
                max_u = max((g["util_pct"] for g in gpus), default=0.0)
                STATE.gpu_util_window.append(max_u)

            # 11h35m checkpoint drain signal
            if not drained and elapsed >= DRAIN_AT_SEC:
                drained = True
                print("[worker] 11h35m reached: sending SIGTERM to running jobs for checkpoint flush", flush=True)
                STATE.job_mgr.terminate_all_running()

            # 11h40m hard exit(0) so Kaggle commits /kaggle/working cleanly before 12h timeout
            if elapsed >= MAX_LIFETIME_SEC:
                print("[worker] 11h40m safe lifetime cap reached; exiting cleanly (0).", flush=True)
                break

            # Idle watchdog: protect weekly 30h GPU quota
            if (
                IDLE_TIMEOUT_SEC > 0
                and STATE.idle_sec() >= IDLE_TIMEOUT_SEC
                and STATE.job_mgr.active_count() == 0
            ):
                avg_gpu = (
                    sum(STATE.gpu_util_window) / len(STATE.gpu_util_window)
                    if STATE.gpu_util_window
                    else 0.0
                )
                if avg_gpu < 3.0:
                    print(
                        f"[worker] idle for {STATE.idle_sec():.0f}s with 0 active jobs & GPU<3%; auto-shutting down.",
                        flush=True,
                    )
                    break
    finally:
        STATE.job_mgr.terminate_all_running()
        STATE.pty_mgr.close_all()
        publish_state("stopped")
        httpd.shutdown()
        print("[worker] clean exit(0).", flush=True)


if __name__ == "__main__":
    main()
    sys.exit(0)
