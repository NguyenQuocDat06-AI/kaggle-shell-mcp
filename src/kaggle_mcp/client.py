"""Authenticated HTTP client to a slotted remote Kaggle worker with shared SSL & keep-alive pooling."""

from __future__ import annotations

import re
import ssl
import threading
import uuid
from pathlib import Path
from typing import Any

import httpx

from .config import (
    ActiveSession,
    Settings,
    clear_active_session,
    load_active_session,
    normalize_slot,
    save_active_session,
)
from .launcher import (
    TERMINAL_KERNEL_STATUSES,
    fetch_remote_state,
    get_kaggle_api,
    get_kernel_status,
    probe_worker_ping,
)
from .sync import pack_tar_gz, unpack_tar_gz

_HTML_TAG_RE = re.compile(r"<[^>]+>")


class WorkerSessionStoppedError(RuntimeError):
    """Raised when the remote Kaggle worker has cleanly stopped or terminated."""


def summarize_http_error(slot: str, status_code: int, raw_body: str) -> str:
    """Turn noisy Cloudflare HTML error pages (530/502/504) into a concise single-line message."""
    body = (raw_body or "").strip()
    low = body.lower()
    is_html = "<!doctype html" in low or "<html" in low

    if status_code == 530 or "cloudflare tunnel error" in low:
        return f"Worker [slot={slot}] tunnel closed (HTTP {status_code} — VM stopped or tunnel offline)."
    if status_code == 524:
        return f"Worker [slot={slot}] HTTP 524: Cloudflare 100s origin timeout exceeded."
    if status_code in {502, 504} and is_html:
        return f"Worker [slot={slot}] tunnel unreachable (HTTP {status_code})."
    if is_html:
        plain = " ".join(_HTML_TAG_RE.sub(" ", body).split())[:120]
        return f"Worker [slot={slot}] HTTP {status_code}: {plain or 'HTML error response'}"
    one_line = " ".join(body.splitlines()).strip()[:200]
    return f"Worker [slot={slot}] HTTP {status_code}: {one_line}"

_POOL_LOCK = threading.Lock()
_CLIENT_POOL: dict[str, httpx.Client] = {}
# Pre-load CA certs once per process instead of re-reading certifi PEMs on every client (~250ms saved)
_SHARED_SSL_CTX = ssl.create_default_context()


def _get_pooled_client(base_url: str) -> httpx.Client:
    """Return a persistent HTTP/1.1 keep-alive client sharing one pre-loaded SSLContext."""
    with _POOL_LOCK:
        client = _CLIENT_POOL.get(base_url)
        if client is None or client.is_closed:
            client = httpx.Client(
                verify=_SHARED_SSL_CTX,
                follow_redirects=True,
                limits=httpx.Limits(
                    max_keepalive_connections=16,
                    max_connections=32,
                    keepalive_expiry=120.0,
                ),
                headers={"Accept-Encoding": "gzip, deflate"},
            )
            _CLIENT_POOL[base_url] = client
        return client


def _evict_pooled_client(base_url: str) -> None:
    with _POOL_LOCK:
        client = _CLIENT_POOL.pop(base_url, None)
    if client is not None:
        try:
            client.close()
        except Exception:
            pass


class WorkerClient:
    def __init__(
        self,
        settings: Settings,
        slot: str = "1",
        caller: str = "mcp",
        session_override: ActiveSession | None = None,
    ) -> None:
        self.settings = settings
        self.slot = normalize_slot(slot)
        self.caller = caller
        self._session_override = session_override

    def _get_ready_session(self, allow_refresh: bool = True) -> ActiveSession:
        sess = self._session_override or load_active_session(self.slot)
        if not sess:
            raise RuntimeError(
                f"Chưa có phiên Kaggle nào ở slot '{self.slot}'. Hãy gọi `kaggle_up(slot='{self.slot}')` trước."
            )
        if sess.mode == "batch":
            raise RuntimeError(
                f"Slot '{self.slot}' ({sess.kernel_ref}) là phiên chạy batch offline (không có live tunnel). "
                "Hãy dùng `kaggle_batch_logs` hoặc `kaggle_status` để theo dõi."
            )

        if sess.base_url:
            return sess

        if allow_refresh:
            refreshed = self._refresh_endpoint(sess)
            if refreshed and refreshed.base_url:
                return refreshed

        raise RuntimeError(
            f"Không kết nối được tới worker ở slot '{self.slot}' (session={sess.session_id}). "
            f"Kiểm tra bằng `kaggle_status(slot='{self.slot}')` hoặc gọi `kaggle_up(slot='{self.slot}')`."
        )

    def _refresh_endpoint(self, sess: ActiveSession) -> ActiveSession | None:
        api = get_kaggle_api(self.settings)
        rstate = fetch_remote_state(api, self.settings, slot=self.slot)
        if rstate and rstate.get("session_id") == sess.session_id and rstate.get("status") == "stopped":
            _evict_pooled_client(sess.base_url)
            clear_active_session(self.slot)
            self._session_override = None
            raise WorkerSessionStoppedError(
                f"Slot '{self.slot}' worker stopped (idle timeout or shutdown). Press F5 / call kaggle_up to boot."
            )

        kref = sess.kernel_ref or self.settings.kernel_ref_for_slot(self.slot, mode="live")
        ver_ref = f"{kref}/{sess.version_number}" if sess.version_number else kref
        kinfo = get_kernel_status(api, ver_ref)
        kstatus = kinfo.get("status", "")
        if kstatus in TERMINAL_KERNEL_STATUSES:
            _evict_pooled_client(sess.base_url)
            clear_active_session(self.slot)
            self._session_override = None
            raise WorkerSessionStoppedError(
                f"Slot '{self.slot}' Kaggle kernel ended ({kstatus}). Press F5 / call kaggle_up to boot."
            )

        if not rstate or rstate.get("session_id") != sess.session_id:
            return None

        ts_url = (rstate.get("ts_url") or "").strip()
        tunnel_url = (rstate.get("tunnel_url") or "").strip()
        for url in [u for u in (ts_url, tunnel_url) if u]:
            if probe_worker_ping(url, sess.session_id):
                sess.tunnel_url = tunnel_url
                sess.ts_url = ts_url if url == ts_url else ""
                save_active_session(sess, slot=self.slot)
                self._session_override = sess
                return sess
        return None

    _SAFE_READONLY_POSTS = {
        "/monitor",
        "/fs/list",
        "/fs/read_text",
        "/fs/du",
        "/fs/inspect",
        "/fs/find",
        "/jobs/logs",
        "/jobs/logs_chunk",
    }

    def _request(
        self,
        method: str,
        path: str,
        *,
        json_body: dict[str, Any] | None = None,
        content: bytes | None = None,
        extra_headers: dict[str, str] | None = None,
        timeout: float = 90.0,
    ) -> httpx.Response:
        sess = self._get_ready_session(allow_refresh=True)
        headers = {
            "Authorization": f"Bearer {sess.worker_token}",
            "X-Kgsh-Caller": self.caller,
        }
        if method.upper() == "POST" and path not in self._SAFE_READONLY_POSTS:
            headers["X-Idempotency-Key"] = uuid.uuid4().hex
        if extra_headers:
            headers.update(extra_headers)

        req_timeout = httpx.Timeout(
            connect=5.0,
            read=float(timeout),
            write=min(60.0, float(timeout)),
            pool=5.0,
        )
        can_retry_after_send = method.upper() == "GET" or path in self._SAFE_READONLY_POSTS

        for attempt in range(2):
            base = sess.base_url
            url = f"{base}{path}"
            try:
                client = _get_pooled_client(base)
                resp = client.request(
                    method,
                    url,
                    json=json_body,
                    content=content,
                    headers=headers,
                    timeout=req_timeout,
                )
                if resp.status_code in {502, 530} and attempt == 0:
                    _evict_pooled_client(base)
                    refreshed = self._refresh_endpoint(sess)
                    if refreshed:
                        sess = refreshed
                        continue
                if resp.status_code == 504 and attempt == 0 and can_retry_after_send:
                    _evict_pooled_client(base)
                    refreshed = self._refresh_endpoint(sess)
                    if refreshed:
                        sess = refreshed
                        continue
                if resp.status_code >= 400:
                    raise RuntimeError(summarize_http_error(self.slot, resp.status_code, resp.text))
                return resp
            except (httpx.ConnectError, httpx.PoolTimeout) as exc:
                _evict_pooled_client(base)
                if attempt == 0:
                    refreshed = self._refresh_endpoint(sess)
                    if refreshed:
                        sess = refreshed
                        continue
                raise RuntimeError(f"Lỗi kết nối tới Kaggle worker [slot={self.slot}] ({url}): {exc}") from exc
            except (httpx.ReadTimeout, httpx.RemoteProtocolError) as exc:
                _evict_pooled_client(base)
                if attempt == 0 and can_retry_after_send:
                    refreshed = self._refresh_endpoint(sess)
                    if refreshed:
                        sess = refreshed
                        continue
                raise RuntimeError(f"Lỗi kết nối tới Kaggle worker [slot={self.slot}] ({url}): {exc}") from exc

        raise RuntimeError("Request failed after endpoint refresh.")

    def status(self) -> dict[str, Any]:
        return self._request("GET", "/status", timeout=20.0).json()

    def monitor(self, since_seq: int = 0, log_job_id: str = "", log_offset: int = 0) -> dict[str, Any]:
        """Single-roundtrip combined poll for TUI over persistent keep-alive TLS."""
        try:
            return self._request(
                "POST",
                "/monitor",
                json_body={
                    "since_seq": int(since_seq),
                    "log_job_id": log_job_id,
                    "log_offset": int(log_offset),
                },
                timeout=12.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            st = self.status()
            jobs = self.job_list().get("jobs", [])
            log_chunk = None
            if log_job_id:
                try:
                    lg = self.job_logs(log_job_id, tail_lines=80)
                    log_chunk = {"chunk": lg.get("logs", ""), "next_offset": 0, "replace": True, "job": lg.get("job")}
                except Exception:
                    log_chunk = None
            return {
                "status": st,
                "jobs": jobs,
                "latest_seq": since_seq,
                "events": [],
                "log_chunk": log_chunk,
            }

    def shutdown(self) -> dict[str, Any]:
        return self._request("POST", "/shutdown", json_body={"graceful": True}, timeout=20.0).json()

    def term_exec(self, command: str, session: str = "default", timeout_sec: float = 60.0) -> dict[str, Any]:
        http_timeout = max(30.0, float(timeout_sec) + 15.0)
        return self._request(
            "POST",
            "/term/exec",
            json_body={"command": command, "session": session, "timeout_sec": timeout_sec},
            timeout=http_timeout,
        ).json()

    def job_spawn(
        self,
        command: str,
        cwd: str = "/kaggle/working",
        name: str = "",
        gpu_ids: str = "",
    ) -> dict[str, Any]:
        return self._request(
            "POST",
            "/jobs/spawn",
            json_body={"command": command, "cwd": cwd, "name": name, "gpu_ids": gpu_ids},
            timeout=30.0,
        ).json()

    def job_list(self) -> dict[str, Any]:
        return self._request("GET", "/jobs/list", timeout=20.0).json()

    def job_logs(self, job_id: str, tail_lines: int = 100, grep: str = "") -> dict[str, Any]:
        return self._request(
            "POST",
            "/jobs/logs",
            json_body={"job_id": job_id, "tail_lines": tail_lines, "grep": grep},
            timeout=30.0,
        ).json()

    def job_logs_chunk(self, job_id: str, offset: int = 0, max_bytes: int = 65536) -> dict[str, Any]:
        """Incremental byte-offset log reader for `kgsh logs -f`."""
        try:
            return self._request(
                "POST",
                "/jobs/logs_chunk",
                json_body={"job_id": job_id, "offset": int(offset), "max_bytes": int(max_bytes)},
                timeout=20.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            lg = self.job_logs(job_id, tail_lines=120)
            txt = lg.get("logs", "")
            return {"job": lg.get("job", {}), "chunk": txt, "next_offset": len(txt), "fallback": True}

    def job_kill(self, job_id: str, sig: str = "SIGTERM") -> dict[str, Any]:
        return self._request(
            "POST",
            "/jobs/kill",
            json_body={"job_id": job_id, "signal": sig},
            timeout=20.0,
        ).json()

    def fs_push(self, local_path: str, remote_path: str = "/kaggle/working") -> dict[str, Any]:
        src = Path(local_path)
        tar_bytes = pack_tar_gz(src)
        resp = self._request(
            "POST",
            "/fs/upload",
            content=tar_bytes,
            extra_headers={
                "Content-Type": "application/gzip",
                "X-Remote-Path": remote_path,
            },
            timeout=300.0,
        )
        res = resp.json()
        res["slot"] = self.slot
        res["uploaded_kb"] = round(len(tar_bytes) / 1024, 1)
        return res

    def fs_pull(self, remote_path: str, local_path: str) -> dict[str, Any]:
        dest = Path(local_path)
        resp = self._request(
            "POST",
            "/fs/download",
            json_body={"remote_path": remote_path},
            timeout=300.0,
        )
        extracted = unpack_tar_gz(resp.content, dest)
        return {
            "ok": True,
            "slot": self.slot,
            "remote_path": remote_path,
            "local_dest": str(dest.expanduser().resolve()),
            "downloaded_kb": round(len(resp.content) / 1024, 1),
            "extracted_count": len(extracted),
            "sample_files": extracted[:20],
        }

    def fs_list(self, remote_path: str = "/kaggle/working") -> list[dict[str, Any]]:
        """List directory entries on the remote worker for the TUI File Explorer."""
        try:
            resp = self._request("POST", "/fs/list", json_body={"path": remote_path}, timeout=15.0)
            return list(resp.json().get("entries", []))
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import json as _json

            py = (
                "python3 -c \"import json,pathlib; p=pathlib.Path("
                + repr(remote_path)
                + "); print(json.dumps([{'name':c.name,'path':str(c),'is_dir':c.is_dir(),"
                "'size_bytes':0 if c.is_dir() else c.stat().st_size,"
                "'mtime':round(c.stat().st_mtime,2)} "
                "for c in sorted(p.iterdir(), key=lambda x:(not x.is_dir(), x.name.lower()))[:150]] if p.is_dir() else []))\""
            )
            out = self.term_exec(py, timeout_sec=15.0).get("output", "[]")
            try:
                return list(_json.loads(out.splitlines()[-1]))
            except Exception:
                return []

    def fs_read_text(self, remote_path: str, max_bytes: int = 32768) -> dict[str, Any]:
        """Read a UTF-8 preview of a remote file for the TUI File Preview Modal."""
        try:
            return self._request(
                "POST",
                "/fs/read_text",
                json_body={"path": remote_path, "max_bytes": max_bytes},
                timeout=15.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            res = self.term_exec(f"head -c {int(max_bytes)} {remote_path!r}", timeout_sec=15.0)
            txt = res.get("output", "")
            return {"path": remote_path, "size_bytes": len(txt), "truncated": False, "content": txt}

    def fs_write_text(
        self,
        remote_path: str,
        content: str,
        *,
        allow_overwrite_large: bool = False,
    ) -> dict[str, Any]:
        """Create or overwrite a UTF-8 file on the remote worker (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/write_text",
                json_body={
                    "path": remote_path,
                    "content": content,
                    "allow_overwrite_large": allow_overwrite_large,
                },
                timeout=30.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import base64 as _b64

            b64 = _b64.b64encode(content.encode("utf-8")).decode("ascii")
            py = (
                "python3 -c \"import base64,pathlib; p=pathlib.Path("
                + repr(remote_path)
                + ").resolve(); "
                + f"assert {self._is_writable_sandbox_expr('p')}, 'Refused unsafe path'; "
                + "p.parent.mkdir(parents=True, exist_ok=True); "
                + f"data=base64.b64decode({b64!r}); p.write_bytes(data); "
                + "print(len(data))\""
            )
            res = self.term_exec(py, timeout_sec=30.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote write failed: {res.get('output', '')}") from None
            return {"ok": True, "path": remote_path, "size_bytes": len(content.encode("utf-8"))}

    def fs_mkdir(self, remote_path: str) -> dict[str, Any]:
        """Create a directory on the remote worker (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/mkdir",
                json_body={"path": remote_path},
                timeout=15.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            py = (
                "python3 -c \"import pathlib; p=pathlib.Path("
                + repr(remote_path)
                + ").resolve(); "
                + f"assert {self._is_writable_sandbox_expr('p')}, 'Refused unsafe path'; "
                + "p.mkdir(parents=True, exist_ok=True)\""
            )
            res = self.term_exec(py, timeout_sec=15.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote mkdir failed: {res.get('output', '')}") from None
            return {"ok": True, "path": remote_path}

    def fs_delete(self, remote_path: str) -> dict[str, Any]:
        """Delete a file or directory under /kaggle/working or /tmp (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/delete",
                json_body={"path": remote_path},
                timeout=20.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            py = (
                "python3 -c \"import pathlib,shutil; p=pathlib.Path("
                + repr(remote_path)
                + ").resolve(); "
                + "assert str(p).startswith('/kaggle/working/') or str(p).startswith('/tmp/'), 'Refused unsafe path'; "
                + "shutil.rmtree(p) if p.is_dir() else p.unlink()\""
            )
            res = self.term_exec(py, timeout_sec=20.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote delete failed: {res.get('output', '')}") from None
            return {"ok": True, "path": remote_path}

    @staticmethod
    def _is_writable_sandbox_expr(var_name: str) -> str:
        return (
            f"((str({var_name}).startswith('/kaggle/working/') or str({var_name}).startswith('/tmp/')) "
            f"and not str({var_name}).startswith('/tmp/kaggle_mcp_runtime'))"
        )

    def fs_rename(self, src_path: str, dst_path: str) -> dict[str, Any]:
        """Rename or move a file/directory within /kaggle/working or /tmp (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/rename",
                json_body={"src": src_path, "dst": dst_path},
                timeout=30.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            py = (
                "python3 -c \"import pathlib,shutil; "
                f"s=pathlib.Path({src_path!r}).resolve(); d=pathlib.Path({dst_path!r}).resolve(); "
                f"assert {self._is_writable_sandbox_expr('s')} and {self._is_writable_sandbox_expr('d')}, 'Refused unsafe path'; "
                "d.parent.mkdir(parents=True, exist_ok=True); shutil.move(str(s), str(d))\""
            )
            res = self.term_exec(py, timeout_sec=30.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote rename failed: {res.get('output', '')}") from None
            return {"ok": True, "src": src_path, "dst": dst_path}

    def fs_copy(self, src_path: str, dst_path: str) -> dict[str, Any]:
        """Duplicate a file/directory into /kaggle/working or /tmp (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/copy",
                json_body={"src": src_path, "dst": dst_path},
                timeout=60.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            py = (
                "python3 -c \"import pathlib,shutil; "
                f"s=pathlib.Path({src_path!r}).resolve(); d=pathlib.Path({dst_path!r}).resolve(); "
                f"assert {self._is_writable_sandbox_expr('d')}, 'Refused unsafe path'; "
                "shutil.copytree(s, d, dirs_exist_ok=True) if s.is_dir() else (d.parent.mkdir(parents=True, exist_ok=True), shutil.copy2(s, d))\""
            )
            res = self.term_exec(py, timeout_sec=60.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote copy failed: {res.get('output', '')}") from None
            return {"ok": True, "src": src_path, "dst": dst_path}

    def fs_du(self, remote_path: str = "/kaggle/working", max_sec: float = 3.0) -> dict[str, Any]:
        """Compute per-entry disk usage under remote_path (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/du",
                json_body={"path": remote_path, "max_sec": max_sec},
                timeout=20.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import json as _json
            import shlex as _shlex

            out = self.term_exec(
                f"du -sb --max-depth=1 {_shlex.quote(remote_path)} 2>/dev/null | sort -nr",
                timeout_sec=20.0,
            ).get("output", "")
            children = []
            for line in out.splitlines():
                parts = line.split(None, 1)
                if len(parts) != 2 or not parts[0].isdigit():
                    continue
                sz = int(parts[0])
                p = parts[1].strip()
                if p.rstrip("/") == remote_path.rstrip("/"):
                    continue
                name = p.rsplit("/", 1)[-1]
                children.append(
                    {"name": name, "path": p, "is_dir": True, "size_bytes": sz, "pct": 0.0}
                )
            total = sum(c["size_bytes"] for c in children)
            denom = max(1, total)
            for c in children:
                c["pct"] = round((c["size_bytes"] / denom) * 100.0, 1)
            _ = _json
            return {
                "path": remote_path,
                "total_bytes": total,
                "partial": False,
                "children": children[:100],
            }

    def fs_inspect(self, remote_path: str) -> dict[str, Any]:
        """Inspect structured remote files (.csv/.parquet/.zip/.tar.gz/.safetensors/.pt) without full read."""
        try:
            return self._request(
                "POST",
                "/fs/inspect",
                json_body={"path": remote_path},
                timeout=25.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import json as _json

            py = (
                "python3 -c \"import csv,json,pathlib,struct,zipfile; "
                f"fp=pathlib.Path({remote_path!r}); sz=fp.stat().st_size; ext=fp.suffix.lower(); "
                "res={'path':str(fp),'kind':'binary_info','format':ext.lstrip('.'),'size_bytes':sz,'note':'Legacy worker inspect'}; "
                "\nif ext in ('.csv','.tsv'):\n"
                " r=csv.reader(open(fp,encoding='utf-8',errors='replace'),delimiter='\\t' if ext=='.tsv' else ','); "
                " cols=next(r,[]); rows=[row[:40] for _,row in zip(range(25),r)]; "
                " res={'path':str(fp),'kind':'table','format':ext.lstrip('.'),'size_bytes':sz,'columns':cols[:40],'rows':rows,'row_count':len(rows),'estimated':True}\n"
                "elif ext=='.zip':\n"
                " z=zipfile.ZipFile(fp); infos=z.infolist(); "
                " res={'path':str(fp),'kind':'archive','format':'zip','size_bytes':sz,'total_entries':len(infos),'entries':[{'name':i.filename,'size_bytes':i.file_size,'is_dir':i.is_dir()} for i in infos[:200]]}\n"
                "elif ext=='.safetensors':\n"
                " f=open(fp,'rb'); hl=struct.unpack('<Q',f.read(8))[0]; hdr=json.loads(f.read(min(hl,4194304))); "
                " keys=[k for k in hdr if k!='__metadata__']; "
                " res={'path':str(fp),'kind':'tensors','format':'safetensors','size_bytes':sz,'total_tensors':len(keys),'metadata':hdr.get('__metadata__',{}),'tensors':[{'name':k,'dtype':hdr[k].get('dtype','?'),'shape':hdr[k].get('shape',[])} for k in keys[:200]]}\n"
                "print(json.dumps(res))\""
            )
            out = self.term_exec(py, timeout_sec=25.0).get("output", "")
            for line in reversed(out.splitlines()):
                try:
                    return dict(_json.loads(line))
                except Exception:
                    continue
            raise RuntimeError(f"Failed to inspect remote file: {remote_path}") from None

    def fs_extract(self, src_path: str, dest_dir: str = "") -> dict[str, Any]:
        """Extract a .zip / .tar.gz / .tgz / .tar archive inside /kaggle/working or /tmp."""
        try:
            return self._request(
                "POST",
                "/fs/extract",
                json_body={"src": src_path, "dest_dir": dest_dir},
                timeout=120.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            target_dir = dest_dir.strip()
            if not target_dir:
                stem = src_path.rsplit(".", 1)[0]
                if stem.endswith(".tar"):
                    stem = stem[:-4]
                target_dir = stem
            py = (
                "python3 -c \"import pathlib,tarfile,zipfile; "
                f"s=pathlib.Path({src_path!r}).resolve(); d=pathlib.Path({target_dir!r}).resolve(); "
                f"assert {self._is_writable_sandbox_expr('d')}, 'Refused unsafe dest'; "
                "d.mkdir(parents=True, exist_ok=True); "
                "c=0\nif str(s).lower().endswith('.zip'):\n"
                " with zipfile.ZipFile(s) as z:\n"
                "  for i in z.infolist():\n"
                "   assert (d/i.filename).resolve().is_relative_to(d); z.extract(i,d); c+=1\n"
                "else:\n"
                " with tarfile.open(s,'r:*') as t:\n"
                "  for m in t:\n"
                "   assert (d/m.name).resolve().is_relative_to(d)\n"
                "   if not (m.issym() or m.islnk()): t.extract(m,d); c+=1\n"
                "print(c)\""
            )
            res = self.term_exec(py, timeout_sec=120.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote extract failed: {res.get('output', '')}") from None
            try:
                cnt = int(res.get("output", "0").strip().splitlines()[-1])
            except Exception:
                cnt = 0
            return {"ok": True, "src": src_path, "dest_dir": target_dir, "count": cnt}

    def fs_archive(self, paths: list[str], dest_path: str = "") -> dict[str, Any]:
        """Bundle multiple remote files/folders into a .tar.gz archive (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/archive",
                json_body={"paths": list(paths), "dest_path": dest_path},
                timeout=120.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import json as _json

            out_target = dest_path.strip() or "/kaggle/working/bundle_archive.tar.gz"
            py = (
                "python3 -c \"import json,pathlib,tarfile; "
                f"srcs={list(paths)!r}; dst=pathlib.Path({out_target!r}).resolve(); "
                f"assert {self._is_writable_sandbox_expr('dst')}, 'Refused unsafe archive dest'; "
                "dst.parent.mkdir(parents=True, exist_ok=True); "
                "tf=tarfile.open(dst,'w:gz')\n"
                "for idx,rp in enumerate(srcs):\n"
                " sp=pathlib.Path(rp).resolve(); tf.add(str(sp), arcname=f'{idx}_{sp.name}' if len(srcs)>1 else sp.name)\n"
                "tf.close(); print(json.dumps({'ok':True,'path':str(dst),'size_bytes':dst.stat().st_size,'count':len(srcs)}))\""
            )
            res = self.term_exec(py, timeout_sec=120.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote archive failed: {res.get('output', '')}") from None
            return dict(_json.loads(res.get("output", "{}").splitlines()[-1]))

    def fs_find(
        self,
        root: str = "/kaggle/working",
        name_glob: str = "",
        content_regex: str = "",
        max_results: int = 150,
    ) -> dict[str, Any]:
        """Find files by glob or grep text content on the worker (with 404 PTY fallback)."""
        try:
            return self._request(
                "POST",
                "/fs/find",
                json_body={
                    "root": root,
                    "name_glob": name_glob,
                    "content_regex": content_regex,
                    "max_results": max_results,
                },
                timeout=25.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import json as _json

            py = (
                "python3 -c \"import fnmatch,json,os,pathlib,re; "
                f"root={root!r}; ng={name_glob!r}; cr={content_regex!r}; mx={int(max_results)}; "
                "rx=re.compile(cr,re.I) if cr else None; skip={'.git','__pycache__','node_modules','.venv'}; "
                "m=[]\nfor dp,dns,fns in os.walk(root):\n"
                " dns[:]=[d for d in dns if d not in skip]\n"
                " for fn in fns:\n"
                "  if ng and not fnmatch.fnmatch(fn.lower(),ng.lower()): continue\n"
                "  fp=pathlib.Path(dp)/fn\n"
                "  try: sz=fp.stat().st_size\n"
                "  except OSError: continue\n"
                "  if rx is None: m.append({'path':str(fp),'name':fn,'is_dir':False,'size_bytes':sz,'line':0,'snippet':''})\n"
                "  elif sz<=2097152:\n"
                "   try:\n"
                "    raw=fp.read_bytes()\n"
                "    if b'\\x00' in raw[:4096]: continue\n"
                "    for lno,ln in enumerate(raw.decode('utf-8','replace').splitlines(),1):\n"
                "     if rx.search(ln): m.append({'path':str(fp),'name':fn,'is_dir':False,'size_bytes':sz,'line':lno,'snippet':ln.strip()[:160]}); break\n"
                "   except OSError: continue\n"
                "  if len(m)>=mx: break\n"
                " if len(m)>=mx: break\n"
                "print(json.dumps({'root':root,'matches':m,'truncated':len(m)>=mx}))\""
            )
            out = self.term_exec(py, timeout_sec=25.0).get("output", "")
            for line in reversed(out.splitlines()):
                try:
                    return dict(_json.loads(line))
                except Exception:
                    continue
            return {"root": root, "matches": [], "truncated": False}

    def fs_publish_dataset(
        self,
        source_dir: str,
        slug: str,
        title: str = "",
        version_notes: str = "Published via kgsh",
        public: bool = False,
    ) -> dict[str, Any]:
        """Publish a remote directory directly from Kaggle VM as a Kaggle Dataset."""
        try:
            return self._request(
                "POST",
                "/fs/publish_dataset",
                json_body={
                    "source_dir": source_dir,
                    "slug": slug,
                    "title": title or slug,
                    "version_notes": version_notes,
                    "public": public,
                },
                timeout=180.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            import json as _json

            py = (
                "python3 -c \"import json,os,pathlib,re,shutil,uuid; "
                "from kaggle.api.kaggle_api_extended import KaggleApi; "
                f"src=pathlib.Path({source_dir!r}).resolve(); "
                f"slug=re.sub(r'[^a-z0-9-]+','-',{slug!r}.lower()).strip('-') or 'kaggle-dataset'; "
                "user=os.environ.get('KAGGLE_USERNAME',''); ref=f'{user}/{slug}'; "
                "st=pathlib.Path('/tmp')/f'pub_{uuid.uuid4().hex[:8]}'; st.mkdir(parents=True,exist_ok=True); "
                "[shutil.copytree(x,st/x.name,dirs_exist_ok=True) if x.is_dir() else shutil.copy2(x,st/x.name) for x in src.iterdir() if x.name!='dataset-metadata.json']; "
                f"(st/'dataset-metadata.json').write_text(json.dumps({{'title':{title or slug!r},'id':ref,'licenses':[{{'name':'CC0-1.0'}}]}})); "
                "api=KaggleApi(); api.authenticate()\n"
                "try: api.dataset_create_version(folder=str(st),version_notes="
                + repr(version_notes)
                + ",quiet=True,convert_to_csv=False,dir_mode='zip')\n"
                f"except Exception: api.dataset_create_new(folder=str(st),public={bool(public)!r},quiet=True,convert_to_csv=False,dir_mode='zip')\n"
                "shutil.rmtree(st,ignore_errors=True); "
                "print(json.dumps({'ok':True,'dataset_ref':ref,'url':f'https://www.kaggle.com/datasets/{ref}'}))\""
            )
            res = self.term_exec(py, timeout_sec=180.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Dataset publish failed: {res.get('output', '')}") from None
            return dict(_json.loads(res.get("output", "{}").splitlines()[-1]))

    def proc_kill(self, pid: int, sig: str = "SIGTERM") -> dict[str, Any]:
        """Send SIGTERM or SIGKILL to a remote GPU/compute PID (with 404 PTY fallback)."""
        pid_i = int(pid)
        if pid_i <= 1:
            raise PermissionError(f"Refused to signal protected PID {pid_i}")
        sig_clean = "SIGKILL" if str(sig).strip().upper() == "SIGKILL" else "SIGTERM"
        try:
            return self._request(
                "POST",
                "/proc/kill",
                json_body={"pid": pid_i, "sig": sig_clean},
                timeout=15.0,
            ).json()
        except RuntimeError as exc:
            if "HTTP 404" not in str(exc):
                raise
            flag = "-KILL" if sig_clean == "SIGKILL" else "-TERM"
            res = self.term_exec(f"kill {flag} {pid_i}", timeout_sec=15.0)
            if int(res.get("exit_code", 1)) != 0:
                raise RuntimeError(f"Remote kill failed: {res.get('output', '')}") from None
            return {"ok": True, "pid": pid_i, "sig": sig_clean}
