"""Authenticated HTTP client to the remote Kaggle worker with auto-healing tunnel lookup."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import httpx

from .config import ActiveSession, Settings, load_active_session, save_active_session
from .launcher import fetch_remote_state, get_kaggle_api, probe_worker_ping
from .sync import pack_tar_gz, unpack_tar_gz


class WorkerClient:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings

    def _get_ready_session(self, allow_refresh: bool = True) -> ActiveSession:
        sess = load_active_session()
        if not sess:
            raise RuntimeError("Chưa có phiên Kaggle nào đang mở. Hãy gọi tool `kaggle_up` trước.")

        if sess.base_url and probe_worker_ping(sess.base_url, sess.session_id):
            return sess

        if allow_refresh:
            refreshed = self._refresh_endpoint(sess)
            if refreshed and refreshed.base_url:
                return refreshed

        raise RuntimeError(
            f"Không kết nối được tới worker (session={sess.session_id}). "
            "Kiểm tra bằng `kaggle_status` hoặc gọi `kaggle_up` để mở phiên mới."
        )

    def _refresh_endpoint(self, sess: ActiveSession) -> ActiveSession | None:
        api = get_kaggle_api(self.settings)
        rstate = fetch_remote_state(api, self.settings)
        if not rstate or rstate.get("session_id") != sess.session_id:
            return None
        if rstate.get("status") == "stopped":
            return None

        ts_url = (rstate.get("ts_url") or "").strip()
        tunnel_url = (rstate.get("tunnel_url") or "").strip()
        for url in [u for u in (ts_url, tunnel_url) if u]:
            if probe_worker_ping(url, sess.session_id):
                sess.tunnel_url = tunnel_url
                sess.ts_url = ts_url if url == ts_url else ""
                save_active_session(sess)
                return sess
        return None

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
        headers = {"Authorization": f"Bearer {sess.worker_token}"}
        if extra_headers:
            headers.update(extra_headers)

        for attempt in range(2):
            url = f"{sess.base_url}{path}"
            try:
                with httpx.Client(timeout=timeout, follow_redirects=True) as client:
                    resp = client.request(
                        method,
                        url,
                        json=json_body,
                        content=content,
                        headers=headers,
                    )
                if resp.status_code in {502, 504, 530} and attempt == 0:
                    refreshed = self._refresh_endpoint(sess)
                    if refreshed:
                        sess = refreshed
                        continue
                if resp.status_code >= 400:
                    raise RuntimeError(f"Worker HTTP {resp.status_code}: {resp.text[:500]}")
                return resp
            except (httpx.ConnectError, httpx.ReadTimeout, httpx.RemoteProtocolError) as exc:
                if attempt == 0:
                    refreshed = self._refresh_endpoint(sess)
                    if refreshed:
                        sess = refreshed
                        continue
                raise RuntimeError(f"Lỗi kết nối tới Kaggle worker ({url}): {exc}") from exc

        raise RuntimeError("Request failed after endpoint refresh.")

    def status(self) -> dict[str, Any]:
        return self._request("GET", "/status", timeout=20.0).json()

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

    def job_spawn(self, command: str, cwd: str = "/kaggle/working", name: str = "") -> dict[str, Any]:
        return self._request(
            "POST",
            "/jobs/spawn",
            json_body={"command": command, "cwd": cwd, "name": name},
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
            "remote_path": remote_path,
            "local_dest": str(dest.expanduser().resolve()),
            "downloaded_kb": round(len(resp.content) / 1024, 1),
            "extracted_count": len(extracted),
            "sample_files": extracted[:20],
        }
