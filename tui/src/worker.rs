//! Async HTTP/1.1 + gzip client for the remote Kaggle worker + HTTP 404 PTY fallbacks & tar sync.

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use regex::Regex;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tar::{Archive, Builder, EntryType};

use crate::state::{ActiveSession, ActivityEvent};

static HTML_TAG_RE: OnceLock<Regex> = OnceLock::new();
static IDEM_SEQ: AtomicU64 = AtomicU64::new(1);
const MAX_TAR_EXTRACT_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB Tar Bomb cap

pub fn summarize_http_error(slot: &str, status: u16, raw_body: &str) -> String {
    let body = raw_body.trim();
    let low = body.to_ascii_lowercase();
    let is_html = low.contains("<!doctype html") || low.contains("<html");
    if status == 530 || low.contains("cloudflare tunnel error") {
        return format!(
            "Worker [slot={}] tunnel closed (HTTP {} — VM stopped or tunnel offline).",
            slot, status
        );
    }
    if status == 524 {
        return format!(
            "Worker [slot={}] HTTP 524: Cloudflare 100s timeout exceeded (use detached Job for long runs).",
            slot
        );
    }
    if (status == 502 || status == 504) && is_html {
        return format!("Worker [slot={}] tunnel unreachable (HTTP {}).", slot, status);
    }
    if is_html {
        let re = HTML_TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
        let plain: String = re
            .replace_all(body, " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let short: String = plain.chars().take(120).collect();
        let msg = if short.is_empty() {
            "HTML error response"
        } else {
            &short
        };
        return format!("Worker [slot={}] HTTP {}: {}", slot, status, msg);
    }
    let one_line: String = body
        .lines()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .chars()
        .take(200)
        .collect();
    format!("Worker [slot={}] HTTP {}: {}", slot, status, one_line)
}

#[derive(Debug)]
pub enum WorkerError {
    Http { status: u16, message: String },
    Transport(String),
    Other(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Http { message, .. } => write!(f, "{}", message),
            WorkerError::Transport(s) | WorkerError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for WorkerError {}

impl WorkerError {
    pub fn is_404(&self) -> bool {
        matches!(self, WorkerError::Http { status: 404, .. })
    }
    pub fn is_tunnel_down(&self) -> bool {
        match self {
            WorkerError::Http { status, .. } => matches!(*status, 502 | 504 | 530),
            WorkerError::Transport(_) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub util_pct: f64,
    pub vram_used_mb: f64,
    pub vram_total_mb: f64,
    pub temp_c: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GpuProcess {
    pub pid: i64,
    pub gpu_index: String,
    pub process_name: String,
    pub used_mb: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RamInfo {
    pub used_gb: f64,
    pub total_gb: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiskInfo {
    pub used_gb: f64,
    pub free_gb: f64,
    pub total_gb: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WorkerStatus {
    pub session_id: String,
    pub uptime_sec: f64,
    pub idle_sec: f64,
    pub remaining_before_drain_sec: f64,
    pub tunnel_url: String,
    pub ts_url: String,
    pub cpu_pct: f64,
    pub gpus: Vec<GpuInfo>,
    pub gpu_processes: Vec<GpuProcess>,
    pub ram: RamInfo,
    pub disk_working: DiskInfo,
    pub disk_tmp: DiskInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JobInfo {
    pub job_id: String,
    pub name: String,
    pub gpu_ids: String,
    pub status: String,
    pub runtime_sec: f64,
    pub log_size_kb: f64,
    pub started_at: f64,
    pub pid: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LogChunk {
    pub chunk: String,
    pub next_offset: u64,
    pub replace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PtyLive {
    pub running: bool,
    pub session: String,
    pub command: String,
    pub elapsed_sec: f64,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MonitorPayload {
    pub status: WorkerStatus,
    pub jobs: Vec<JobInfo>,
    pub latest_seq: u64,
    pub events: Vec<ActivityEvent>,
    pub log_chunk: Option<LogChunk>,
    pub pty_live: Option<PtyLive>,
    pub pty_sessions_live: BTreeMap<String, PtyLive>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TermExecResult {
    pub output: String,
    pub exit_code: i64,
    pub duration_sec: f64,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub mtime: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FsReadText {
    pub path: String,
    pub size_bytes: u64,
    pub truncated: bool,
    pub is_binary: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DuChild {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DuResult {
    pub path: String,
    pub total_bytes: u64,
    pub partial: bool,
    pub children: Vec<DuChild>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InspectArchiveEntry {
    pub name: String,
    pub size_bytes: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InspectTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InspectResult {
    pub path: String,
    pub kind: String,
    pub format: String,
    pub size_bytes: u64,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_count: u64,
    pub estimated: bool,
    pub total_entries: u64,
    pub truncated: bool,
    pub entries: Vec<InspectArchiveEntry>,
    pub total_tensors: u64,
    pub metadata: BTreeMap<String, Value>,
    pub tensors: Vec<InspectTensor>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FindMatch {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub line: u64,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FindResult {
    pub root: String,
    pub matches: Vec<FindMatch>,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct WorkerHttpClient {
    http: Client,
}

impl Default for WorkerHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerHttpClient {
    pub fn new() -> Self {
        let http = Client::builder()
            .pool_max_idle_per_host(6)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .gzip(true)
            .connect_timeout(Duration::from_secs(6))
            .build()
            .expect("reqwest client init");
        Self { http }
    }

    #[allow(clippy::too_many_arguments)]
    async fn req_bytes(
        &self,
        sess: &ActiveSession,
        method: reqwest::Method,
        path: &str,
        json_body: Option<&Value>,
        raw_body: Option<Vec<u8>>,
        extra_headers: &[(&str, &str)],
        timeout_sec: u64,
    ) -> std::result::Result<Vec<u8>, WorkerError> {
        let base = sess.base_url();
        if base.is_empty() {
            return Err(WorkerError::Other(format!(
                "Slot '{}' has no tunnel endpoint.",
                sess.slot
            )));
        }
        let url = format!("{}{}", base, path);
        let is_readonly_post = matches!(
            path,
            "/monitor" | "/fs/list" | "/fs/read_text" | "/fs/du" | "/fs/inspect" | "/fs/find"
        );
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header(AUTHORIZATION, format!("Bearer {}", sess.worker_token))
            .header("X-Kgsh-Caller", "tui")
            .timeout(Duration::from_secs(timeout_sec));
        if method == reqwest::Method::POST && !is_readonly_post {
            let ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let seq = IDEM_SEQ.fetch_add(1, Ordering::Relaxed);
            req = req.header(
                "X-Idempotency-Key",
                format!("tui-{}-{}-{}", std::process::id(), ns, seq),
            );
        }

        for (k, v) in extra_headers {
            req = req.header(*k, *v);
        }
        if let Some(jb) = json_body {
            req = req.json(jb);
        } else if let Some(bytes) = raw_body {
            req = req.body(bytes);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| WorkerError::Transport(format!("Connect {} failed: {}", url, e)))?;

        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| WorkerError::Transport(format!("Read body failed: {}", e)))?;

        if status >= 400 {
            let txt = String::from_utf8_lossy(&body);
            return Err(WorkerError::Http {
                status,
                message: summarize_http_error(&sess.slot, status, &txt),
            });
        }
        Ok(body.to_vec())
    }

    async fn post_json<T: for<'de> Deserialize<'de>>(
        &self,
        sess: &ActiveSession,
        path: &str,
        body: Value,
        timeout_sec: u64,
    ) -> std::result::Result<T, WorkerError> {
        let raw = self
            .req_bytes(
                sess,
                reqwest::Method::POST,
                path,
                Some(&body),
                None,
                &[],
                timeout_sec,
            )
            .await?;
        serde_json::from_slice(&raw)
            .map_err(|e| WorkerError::Other(format!("JSON decode {}: {}", path, e)))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        sess: &ActiveSession,
        path: &str,
        timeout_sec: u64,
    ) -> std::result::Result<T, WorkerError> {
        let raw = self
            .req_bytes(sess, reqwest::Method::GET, path, None, None, &[], timeout_sec)
            .await?;
        serde_json::from_slice(&raw)
            .map_err(|e| WorkerError::Other(format!("JSON decode {}: {}", path, e)))
    }

    pub async fn prewarm(&self, url: &str) {
        let clean = url.trim().trim_end_matches('/');
        if clean.is_empty() {
            return;
        }
        let _ = self
            .http
            .get(format!("{}/ping", clean))
            .timeout(Duration::from_secs(5))
            .send()
            .await;
    }

    pub async fn monitor(
        &self,
        sess: &ActiveSession,
        since_seq: u64,
        log_job_id: &str,
        log_offset: u64,
    ) -> std::result::Result<(MonitorPayload, f64), WorkerError> {
        let t0 = Instant::now();
        let res: std::result::Result<MonitorPayload, WorkerError> = self
            .post_json(
                sess,
                "/monitor",
                json!({
                    "since_seq": since_seq,
                    "log_job_id": log_job_id,
                    "log_offset": log_offset,
                }),
                12,
            )
            .await;
        match res {
            Ok(payload) => Ok((payload, t0.elapsed().as_secs_f64() * 1000.0)),
            Err(e) if e.is_404() => {
                let st: WorkerStatus = self.get_json(sess, "/status", 12).await?;
                #[derive(Deserialize)]
                struct JobsWrap {
                    #[serde(default)]
                    jobs: Vec<JobInfo>,
                }
                let jw: JobsWrap = self.get_json(sess, "/jobs/list", 12).await.unwrap_or(JobsWrap {
                    jobs: vec![],
                });
                Ok((
                    MonitorPayload {
                        status: st,
                        jobs: jw.jobs,
                        latest_seq: since_seq,
                        events: vec![],
                        log_chunk: None,
                        pty_live: None,
                        pty_sessions_live: BTreeMap::new(),
                    },
                    t0.elapsed().as_secs_f64() * 1000.0,
                ))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn term_exec(
        &self,
        sess: &ActiveSession,
        command: &str,
        session: &str,
        timeout_sec: f64,
    ) -> std::result::Result<TermExecResult, WorkerError> {
        let http_timeout = (timeout_sec.ceil() as u64).saturating_add(15).max(30);
        self.post_json(
            sess,
            "/term/exec",
            json!({
                "command": command,
                "session": session,
                "timeout_sec": timeout_sec,
            }),
            http_timeout,
        )
        .await
    }

    pub async fn term_interrupt(
        &self,
        sess: &ActiveSession,
        session: &str,
    ) -> std::result::Result<Value, WorkerError> {
        self.post_json(sess, "/term/interrupt", json!({ "session": session }), 10)
            .await
    }

    pub async fn job_spawn(
        &self,
        sess: &ActiveSession,
        command: &str,
        gpu_ids: &str,
        cwd: &str,
        name: &str,
    ) -> std::result::Result<JobInfo, WorkerError> {
        self.post_json(
            sess,
            "/jobs/spawn",
            json!({
                "command": command,
                "gpu_ids": gpu_ids,
                "cwd": cwd,
                "name": name,
            }),
            30,
        )
        .await
    }

    pub async fn job_kill(
        &self,
        sess: &ActiveSession,
        job_id: &str,
        signal: &str,
    ) -> std::result::Result<Value, WorkerError> {
        self.post_json(
            sess,
            "/jobs/kill",
            json!({ "job_id": job_id, "signal": signal }),
            20,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn shutdown(&self, sess: &ActiveSession) -> std::result::Result<Value, WorkerError> {
        self.post_json(sess, "/shutdown", json!({ "graceful": true }), 20)
            .await
    }

    pub async fn fs_list(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
    ) -> std::result::Result<Vec<FsEntry>, WorkerError> {
        #[derive(Deserialize)]
        struct ListWrap {
            #[serde(default)]
            entries: Vec<FsEntry>,
        }
        match self
            .post_json::<ListWrap>(sess, "/fs/list", json!({ "path": remote_path }), 15)
            .await
        {
            Ok(w) => Ok(w.entries),
            Err(e) if e.is_404() => {
                let p_json = serde_json::to_string(remote_path).unwrap_or_else(|_| "\"/kaggle/working\"".into());
                let py = format!(
                    "python3 -c \"import json,pathlib; p=pathlib.Path({}); print(json.dumps([{{'name':c.name,'path':str(c),'is_dir':c.is_dir(),'size_bytes':0 if c.is_dir() else c.stat().st_size,'mtime':round(c.stat().st_mtime,2)}} for c in sorted(p.iterdir(), key=lambda x:(not x.is_dir(), x.name.lower()))[:200]] if p.is_dir() else []))\"",
                    p_json
                );
                let out = self.term_exec(sess, &py, "default", 15.0).await?.output;
                let line = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("[]");
                Ok(serde_json::from_str(line).unwrap_or_default())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_read_text(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
        max_bytes: usize,
    ) -> std::result::Result<FsReadText, WorkerError> {
        match self
            .post_json::<FsReadText>(
                sess,
                "/fs/read_text",
                json!({ "path": remote_path, "max_bytes": max_bytes }),
                20,
            )
            .await
        {
            Ok(res) => Ok(res),
            Err(e) if e.is_404() => {
                let cmd = format!("head -c {} {}", max_bytes, sh_quote(remote_path));
                let res = self.term_exec(sess, &cmd, "default", 15.0).await?;
                Ok(FsReadText {
                    path: remote_path.to_string(),
                    size_bytes: res.output.len() as u64,
                    truncated: res.output.len() >= max_bytes,
                    is_binary: res.output.as_bytes().contains(&0),
                    content: res.output,
                })
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_write_text(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
        content: &str,
        allow_overwrite_large: bool,
    ) -> std::result::Result<Value, WorkerError> {
        match self
            .post_json(
                sess,
                "/fs/write_text",
                json!({
                    "path": remote_path,
                    "content": content,
                    "allow_overwrite_large": allow_overwrite_large,
                }),
                30,
            )
            .await
        {
            Ok(res) => Ok(res),
            Err(e) if e.is_404() => {
                let b64 = b64_encode(content.as_bytes());
                let p_json = serde_json::to_string(remote_path).unwrap_or_default();
                let py = format!(
                    "python3 -c \"import base64,pathlib; p=pathlib.Path({}).resolve(); assert (str(p).startswith('/kaggle/working/') or str(p).startswith('/tmp/')) and not str(p).startswith('/tmp/kaggle_mcp_runtime'), 'Unsafe path'; p.parent.mkdir(parents=True,exist_ok=True); d=base64.b64decode('{}'); p.write_bytes(d); print(len(d))\"",
                    p_json, b64
                );
                let res = self.term_exec(sess, &py, "tui", 30.0).await?;
                if res.exit_code != 0 {
                    return Err(WorkerError::Other(format!("Write failed: {}", res.output)));
                }
                Ok(json!({ "ok": true, "path": remote_path, "size_bytes": content.len() }))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_mkdir(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
    ) -> std::result::Result<Value, WorkerError> {
        match self
            .post_json(sess, "/fs/mkdir", json!({ "path": remote_path }), 15)
            .await
        {
            Ok(v) => Ok(v),
            Err(e) if e.is_404() => {
                let p_json = serde_json::to_string(remote_path).unwrap_or_default();
                let py = format!(
                    "python3 -c \"import pathlib; p=pathlib.Path({}).resolve(); assert (str(p).startswith('/kaggle/working/') or str(p).startswith('/tmp/')) and not str(p).startswith('/tmp/kaggle_mcp_runtime'), 'Unsafe path'; p.mkdir(parents=True,exist_ok=True)\"",
                    p_json
                );
                let res = self.term_exec(sess, &py, "tui", 15.0).await?;
                if res.exit_code != 0 {
                    return Err(WorkerError::Other(res.output));
                }
                Ok(json!({ "ok": true, "path": remote_path }))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_delete(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
    ) -> std::result::Result<Value, WorkerError> {
        match self
            .post_json(sess, "/fs/delete", json!({ "path": remote_path }), 20)
            .await
        {
            Ok(v) => Ok(v),
            Err(e) if e.is_404() => {
                let p_json = serde_json::to_string(remote_path).unwrap_or_default();
                let py = format!(
                    "python3 -c \"import pathlib,shutil; p=pathlib.Path({}).resolve(); assert str(p).startswith('/kaggle/working/') or str(p).startswith('/tmp/'), 'Unsafe path'; shutil.rmtree(p) if p.is_dir() else p.unlink()\"",
                    p_json
                );
                let res = self.term_exec(sess, &py, "default", 20.0).await?;
                if res.exit_code != 0 {
                    return Err(WorkerError::Other(res.output));
                }
                Ok(json!({ "ok": true, "path": remote_path }))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_rename(
        &self,
        sess: &ActiveSession,
        src: &str,
        dst: &str,
    ) -> std::result::Result<Value, WorkerError> {
        match self
            .post_json(sess, "/fs/rename", json!({ "src": src, "dst": dst }), 30)
            .await
        {
            Ok(v) => Ok(v),
            Err(e) if e.is_404() => {
                let s_j = serde_json::to_string(src).unwrap_or_default();
                let d_j = serde_json::to_string(dst).unwrap_or_default();
                let py = format!(
                    "python3 -c \"import pathlib,shutil; s=pathlib.Path({}).resolve(); d=pathlib.Path({}).resolve(); assert (str(s).startswith('/kaggle/working/') or str(s).startswith('/tmp/')) and (str(d).startswith('/kaggle/working/') or str(d).startswith('/tmp/')); d.parent.mkdir(parents=True,exist_ok=True); shutil.move(str(s),str(d))\"",
                    s_j, d_j
                );
                let res = self.term_exec(sess, &py, "default", 30.0).await?;
                if res.exit_code != 0 {
                    return Err(WorkerError::Other(res.output));
                }
                Ok(json!({ "ok": true, "src": src, "dst": dst }))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_copy(
        &self,
        sess: &ActiveSession,
        src: &str,
        dst: &str,
    ) -> std::result::Result<Value, WorkerError> {
        match self
            .post_json(sess, "/fs/copy", json!({ "src": src, "dst": dst }), 60)
            .await
        {
            Ok(v) => Ok(v),
            Err(e) if e.is_404() => {
                let s_j = serde_json::to_string(src).unwrap_or_default();
                let d_j = serde_json::to_string(dst).unwrap_or_default();
                let py = format!(
                    "python3 -c \"import pathlib,shutil; s=pathlib.Path({}).resolve(); d=pathlib.Path({}).resolve(); assert str(d).startswith('/kaggle/working/') or str(d).startswith('/tmp/'); shutil.copytree(s,d,dirs_exist_ok=True) if s.is_dir() else (d.parent.mkdir(parents=True,exist_ok=True), shutil.copy2(s,d))\"",
                    s_j, d_j
                );
                let res = self.term_exec(sess, &py, "default", 60.0).await?;
                if res.exit_code != 0 {
                    return Err(WorkerError::Other(res.output));
                }
                Ok(json!({ "ok": true, "src": src, "dst": dst }))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_du(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
    ) -> std::result::Result<DuResult, WorkerError> {
        match self
            .post_json::<DuResult>(
                sess,
                "/fs/du",
                json!({ "path": remote_path, "max_sec": 3.0 }),
                20,
            )
            .await
        {
            Ok(r) => Ok(r),
            Err(e) if e.is_404() => {
                let res = self
                    .term_exec(
                        sess,
                        &format!("du -sb --max-depth=1 {} 2>/dev/null | sort -nr", sh_quote(remote_path)),
                        "default",
                        20.0,
                    )
                    .await?;
                let mut children = Vec::new();
                for line in res.output.lines() {
                    let mut parts = line.split_whitespace();
                    let (Some(sz_s), Some(p)) = (parts.next(), parts.next()) else {
                        continue;
                    };
                    let Ok(sz) = sz_s.parse::<u64>() else {
                        continue;
                    };
                    if p.trim_end_matches('/') == remote_path.trim_end_matches('/') {
                        continue;
                    }
                    let name = p.rsplit('/').next().unwrap_or(p).to_string();
                    children.push(DuChild {
                        name,
                        path: p.to_string(),
                        is_dir: true,
                        size_bytes: sz,
                        pct: 0.0,
                    });
                }
                let total: u64 = children.iter().map(|c| c.size_bytes).sum();
                let denom = total.max(1) as f64;
                for c in &mut children {
                    c.pct = ((c.size_bytes as f64 / denom) * 1000.0).round() / 10.0;
                }
                children.truncate(100);
                Ok(DuResult {
                    path: remote_path.to_string(),
                    total_bytes: total,
                    partial: false,
                    children,
                })
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_inspect(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
    ) -> std::result::Result<InspectResult, WorkerError> {
        self.post_json(sess, "/fs/inspect", json!({ "path": remote_path }), 25)
            .await
    }

    pub async fn fs_extract(
        &self,
        sess: &ActiveSession,
        src: &str,
        dest_dir: &str,
    ) -> std::result::Result<Value, WorkerError> {
        self.post_json(
            sess,
            "/fs/extract",
            json!({ "src": src, "dest_dir": dest_dir }),
            120,
        )
        .await
    }

    pub async fn fs_archive(
        &self,
        sess: &ActiveSession,
        paths: &[String],
        dest_path: &str,
    ) -> std::result::Result<Value, WorkerError> {
        self.post_json(
            sess,
            "/fs/archive",
            json!({ "paths": paths, "dest_path": dest_path }),
            120,
        )
        .await
    }

    pub async fn fs_find(
        &self,
        sess: &ActiveSession,
        root: &str,
        name_glob: &str,
        content_regex: &str,
    ) -> std::result::Result<FindResult, WorkerError> {
        match self
            .post_json::<FindResult>(
                sess,
                "/fs/find",
                json!({
                    "root": root,
                    "name_glob": name_glob,
                    "content_regex": content_regex,
                    "max_results": 150,
                }),
                25,
            )
            .await
        {
            Ok(r) => Ok(r),
            Err(e) if e.is_404() => {
                let cmd = if !content_regex.is_empty() {
                    format!(
                        "grep -rnI --max-count=1 -- {} {} | head -n 120",
                        sh_quote(content_regex),
                        sh_quote(root)
                    )
                } else {
                    let g = if name_glob.is_empty() { "*" } else { name_glob };
                    format!("find {} -iname {} | head -n 150", sh_quote(root), sh_quote(g))
                };
                let res = self.term_exec(sess, &cmd, "default", 25.0).await?;
                let mut matches = Vec::new();
                for ln in res.output.lines() {
                    let trimmed = ln.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if !content_regex.is_empty() {
                        let parts: Vec<&str> = trimmed.splitn(3, ':').collect();
                        if parts.len() == 3 {
                            let lno = parts[1].parse::<u64>().unwrap_or(0);
                            matches.push(FindMatch {
                                path: parts[0].to_string(),
                                name: parts[0].rsplit('/').next().unwrap_or(parts[0]).to_string(),
                                is_dir: false,
                                size_bytes: 0,
                                line: lno,
                                snippet: parts[2].chars().take(120).collect(),
                            });
                            continue;
                        }
                    }
                    matches.push(FindMatch {
                        path: trimmed.to_string(),
                        name: trimmed.rsplit('/').next().unwrap_or(trimmed).to_string(),
                        is_dir: false,
                        size_bytes: 0,
                        line: 0,
                        snippet: String::new(),
                    });
                }
                Ok(FindResult {
                    root: root.to_string(),
                    matches,
                    truncated: false,
                })
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_publish_dataset(
        &self,
        sess: &ActiveSession,
        source_dir: &str,
        slug: &str,
        title: &str,
        version_notes: &str,
        public: bool,
    ) -> std::result::Result<Value, WorkerError> {
        self.post_json(
            sess,
            "/fs/publish_dataset",
            json!({
                "source_dir": source_dir,
                "slug": slug,
                "title": title,
                "version_notes": version_notes,
                "public": public,
            }),
            180,
        )
        .await
    }

    pub async fn proc_kill(
        &self,
        sess: &ActiveSession,
        pid: i64,
        sig: &str,
    ) -> std::result::Result<Value, WorkerError> {
        if pid <= 1 {
            return Err(WorkerError::Other(format!("Refused signal to PID {}", pid)));
        }
        match self
            .post_json(sess, "/proc/kill", json!({ "pid": pid, "sig": sig }), 15)
            .await
        {
            Ok(v) => Ok(v),
            Err(e) if e.is_404() => {
                let flag = if sig.eq_ignore_ascii_case("SIGKILL") {
                    "-KILL"
                } else {
                    "-TERM"
                };
                self.term_exec(sess, &format!("kill {} {}", flag, pid), "default", 15.0)
                    .await?;
                Ok(json!({ "ok": true, "pid": pid, "sig": sig }))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fs_push(
        &self,
        sess: &ActiveSession,
        local_path: &Path,
        remote_dir: &str,
    ) -> Result<(u64, f64)> {
        let lp = local_path.to_path_buf();
        let tar_bytes = tokio::task::spawn_blocking(move || pack_tar_gz(&lp)).await??;
        let kb = (tar_bytes.len() as f64 / 1024.0 * 10.0).round() / 10.0;
        let raw = self
            .req_bytes(
                sess,
                reqwest::Method::POST,
                "/fs/upload",
                None,
                Some(tar_bytes),
                &[
                    (CONTENT_TYPE.as_str(), "application/gzip"),
                    ("X-Remote-Path", remote_dir),
                ],
                300,
            )
            .await?;
        let v: Value = serde_json::from_slice(&raw).unwrap_or_default();
        let count = v
            .get("extracted_entries")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        Ok((count, kb))
    }

    pub async fn fs_pull(
        &self,
        sess: &ActiveSession,
        remote_path: &str,
        local_dir: &Path,
    ) -> Result<(usize, f64, PathBuf)> {
        let raw = self
            .req_bytes(
                sess,
                reqwest::Method::POST,
                "/fs/download",
                Some(&json!({ "remote_path": remote_path })),
                None,
                &[],
                300,
            )
            .await?;
        let kb = (raw.len() as f64 / 1024.0 * 10.0).round() / 10.0;
        let dest = local_dir.to_path_buf();
        let extracted = tokio::task::spawn_blocking(move || unpack_tar_gz_safe(&raw, &dest)).await??;
        Ok((extracted.len(), kb, local_dir.to_path_buf()))
    }
}

pub fn is_secret_tar_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let low = name.to_ascii_lowercase();
    if matches!(low.as_str(), ".env.example" | ".env.sample" | ".env.template") {
        return false;
    }
    if matches!(
        low.as_str(),
        ".env" | "kaggle.json" | "access_token" | ".netrc" | "id_rsa" | "id_ed25519"
    ) {
        return true;
    }
    if low.starts_with(".env.") {
        return true;
    }
    if [".pem", ".key", ".p12", ".pfx"]
        .iter()
        .any(|suf| low.ends_with(suf))
    {
        return true;
    }
    false
}

fn should_skip_tar(path: &Path) -> bool {
    let ignore_dirs: HashSet<&str> = [
        ".git",
        ".venv",
        "venv",
        "__pycache__",
        "node_modules",
        ".kaggle_mcp",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        "kaggle_pull",
        "kaggle_outputs",
        "kaggle_downloads",
        "wandb",
        "target",
    ]
    .into_iter()
    .collect();
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if ignore_dirs.contains(name) {
            return true;
        }
        if name.ends_with(".pyc") || name.ends_with(".pyo") {
            return true;
        }
    }
    is_secret_tar_file(path)
}

pub fn pack_tar_gz(local_path: &Path) -> Result<Vec<u8>> {
    let abs = std::fs::canonicalize(local_path)
        .with_context(|| format!("Local path not found: {:?}", local_path))?;
    if abs.is_file() && is_secret_tar_file(&abs) {
        bail!(
            "Refused to upload credential file {:?} by default to prevent secret leakage.",
            abs.file_name().unwrap_or_default()
        );
    }
    let mut buf = Vec::new();
    {
        let enc = GzEncoder::new(&mut buf, Compression::fast());
        let mut tar = Builder::new(enc);
        let top_name = abs
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("upload");
        if abs.is_file() {
            tar.append_path_with_name(&abs, top_name)?;
        } else {
            fn walk_dir<W: std::io::Write>(
                tar: &mut Builder<W>,
                root: &Path,
                cur: &Path,
                base_arc: &str,
            ) -> Result<()> {
                for entry in std::fs::read_dir(cur)? {
                    let entry = entry?;
                    let ft = entry.file_type()?;
                    if ft.is_symlink() {
                        continue;
                    }
                    let p = entry.path();
                    if should_skip_tar(&p) {
                        continue;
                    }
                    let rel = p.strip_prefix(root)?;
                    let arc = Path::new(base_arc)
                        .join(rel)
                        .to_string_lossy()
                        .replace('\\', "/");
                    if ft.is_dir() {
                        tar.append_dir(&arc, &p)?;
                        walk_dir(tar, root, &p, base_arc)?;
                    } else if ft.is_file() {
                        tar.append_path_with_name(&p, &arc)?;
                    }
                }
                Ok(())
            }
            tar.append_dir(top_name, &abs)?;
            walk_dir(&mut tar, &abs, &abs, top_name)?;
        }
        let enc = tar.into_inner()?;
        enc.finish()?;
    }
    Ok(buf)
}

fn ensure_no_symlink_components(canonical_dest: &Path, rel: &Path) -> Result<PathBuf> {
    let mut cur = canonical_dest.to_path_buf();
    let comps: Vec<Component> = rel.components().collect();
    for (idx, comp) in comps.iter().enumerate() {
        match comp {
            Component::Normal(part) => {
                cur.push(part);
                if let Ok(meta) = std::fs::symlink_metadata(&cur) {
                    if meta.file_type().is_symlink() {
                        bail!("Blocked extraction through existing symlink: {:?}", cur);
                    }
                } else if idx + 1 < comps.len() {
                    // Parent dir doesn't exist yet; safe to create later
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("Blocked path traversal in tar archive: {:?}", rel);
            }
        }
    }
    Ok(cur)
}

pub fn unpack_tar_gz_safe(tar_bytes: &[u8], dest_dir: &Path) -> Result<Vec<String>> {
    unpack_tar_gz_safe_with_limit(tar_bytes, dest_dir, MAX_TAR_EXTRACT_BYTES)
}

pub fn unpack_tar_gz_safe_with_limit(
    tar_bytes: &[u8],
    dest_dir: &Path,
    max_bytes: u64,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(dest_dir)?;
    let canonical_dest = std::fs::canonicalize(dest_dir)?;
    let dec = GzDecoder::new(Cursor::new(tar_bytes));
    let mut archive = Archive::new(dec);
    let mut extracted = Vec::new();
    let mut total_written: u64 = 0;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let etype = entry.header().entry_type();
        if etype == EntryType::Symlink || etype == EntryType::Link {
            continue;
        }
        let rel = entry.path()?.into_owned();
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        {
            bail!("Blocked path traversal in tar archive: {:?}", rel);
        }
        let out_path = ensure_no_symlink_components(&canonical_dest, &rel)?;
        if etype == EntryType::Directory {
            std::fs::create_dir_all(&out_path)?;
        } else if etype == EntryType::Regular {
            let remaining = max_bytes.saturating_sub(total_written);
            if entry.size() > remaining {
                bail!(
                    "Archive exceeds maximum safe extraction limit ({} bytes)",
                    max_bytes
                );
            }
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(&out_path)?;
            let mut limited = (&mut entry).take(remaining.saturating_add(1));
            let written = std::io::copy(&mut limited, &mut out_file)?;
            total_written = total_written.saturating_add(written);
            if total_written > max_bytes {
                drop(out_file);
                let _ = std::fs::remove_file(&out_path);
                bail!(
                    "Archive exceeds maximum safe extraction limit ({} bytes)",
                    max_bytes
                );
            }
        }
        extracted.push(rel.to_string_lossy().replace('\\', "/"));
    }
    Ok(extracted)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

pub fn b64_encode(input: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[((n >> 18) & 63) as usize] as char);
        out.push(TBL[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TBL[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TBL[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_cloudflare_530_html() {
        let html = "<!doctype html><html><head><title>Cloudflare Tunnel error | demo.trycloudflare.com</title></head></html>";
        let msg = summarize_http_error("1", 530, html);
        assert!(msg.contains("slot=1"));
        assert!(msg.contains("HTTP 530"));
        assert!(!msg.contains("<!doctype"));
    }

    #[test]
    fn test_unpack_blocks_path_traversal_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("kgsh_tar_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let src_file = dir.join("hello.txt");
        std::fs::write(&src_file, b"hello rust").unwrap();
        let tar_gz = pack_tar_gz(&src_file).unwrap();
        let out_dir = dir.join("out");
        let files = unpack_tar_gz_safe(&tar_gz, &out_dir).unwrap();
        assert_eq!(files, vec!["hello.txt"]);
        assert_eq!(std::fs::read_to_string(out_dir.join("hello.txt")).unwrap(), "hello rust");

        // Build malicious tar containing `../evil.txt`
        let mut evil_buf = Vec::new();
        {
            let enc = GzEncoder::new(&mut evil_buf, Compression::default());
            let mut tar = Builder::new(enc);
            let mut hdr = tar::Header::new_gnu();
            let data = b"pwn";
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            let _ = hdr.set_path("../evil.txt");
            // Bypass Builder::append_data path restriction using raw header write
            let _ = tar.append_link(&mut hdr, "../evil.txt", "target");
            tar.finish().unwrap();
        }
        // Raw gnu tar with `../evil.txt` regular file
        let mut bad_tar = Vec::new();
        {
            let enc = GzEncoder::new(&mut bad_tar, Compression::default());
            let mut tar = Builder::new(enc);
            let mut hdr = tar::Header::new_gnu();
            let path_bytes = b"../evil.txt";
            let raw = hdr.as_mut_bytes();
            raw[..path_bytes.len()].copy_from_slice(path_bytes);
            hdr.set_entry_type(EntryType::Regular);
            hdr.set_size(3);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tar.append(&hdr, &b"bad"[..]).unwrap();
            let enc = tar.into_inner().unwrap();
            enc.finish().unwrap();
        }
        assert!(unpack_tar_gz_safe(&bad_tar, &dir.join("out2")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pack_tar_gz_filters_secrets_and_unpack_enforces_size_limit() {
        let dir = std::env::temp_dir().join(format!("kgsh_sec_tar_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        std::fs::create_dir_all(proj.join("kaggle_pull")).unwrap();
        std::fs::write(proj.join("train.py"), b"print(1)\n").unwrap();
        std::fs::write(proj.join(".env"), b"KAGGLE_API_TOKEN=secret\n").unwrap();
        std::fs::write(proj.join(".env.prod"), b"HF_TOKEN=secret\n").unwrap();
        std::fs::write(proj.join(".env.example"), b"KAGGLE_API_TOKEN=\n").unwrap();
        std::fs::write(proj.join("kaggle.json"), b"{}").unwrap();
        std::fs::write(proj.join("priv.pem"), b"KEY").unwrap();
        std::fs::write(proj.join("kaggle_pull").join("old.bin"), b"123456").unwrap();

        // Standalone secret file upload refused
        assert!(pack_tar_gz(&proj.join(".env")).is_err());

        let archive = pack_tar_gz(&proj).unwrap();
        let out = dir.join("extracted");
        unpack_tar_gz_safe(&archive, &out).unwrap();
        assert!(out.join("proj").join("train.py").is_file());
        assert!(out.join("proj").join(".env.example").is_file());
        assert!(!out.join("proj").join(".env").exists());
        assert!(!out.join("proj").join(".env.prod").exists());
        assert!(!out.join("proj").join("kaggle.json").exists());
        assert!(!out.join("proj").join("priv.pem").exists());
        assert!(!out.join("proj").join("kaggle_pull").exists());

        // Tar Bomb size ceiling enforced
        let big_src = dir.join("big.bin");
        std::fs::write(&big_src, vec![b'X'; 1024]).unwrap();
        let big_tar = pack_tar_gz(&big_src).unwrap();
        assert!(unpack_tar_gz_safe_with_limit(&big_tar, &dir.join("bomb"), 128).is_err());

        // HTTP 524 Cloudflare timeout summarized cleanly
        let msg524 = summarize_http_error("1", 524, "<html>524 A Timeout Occurred</html>");
        assert!(msg524.contains("HTTP 524"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
