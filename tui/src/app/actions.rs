use crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::bridge::RefreshEndpointRes;
use super::*;

impl App {
    pub fn interrupt_running_exec(&mut self) {
        let Some(sess) = self.active_session() else {
            return;
        };
        let http = self.http.clone();
        let tx = self.tx.clone();
        self.notify("Sending Ctrl+C interrupt...", false);
        tokio::spawn(async move {
            match http.term_interrupt(&sess, "default").await {
                Ok(_) => {
                    let _ = tx.send(AppMsg::Toast {
                        text: "Sent Ctrl+C to running command".into(),
                        is_err: false,
                    });
                }
                Err(e) if e.is_404() => {
                    let _ = tx.send(AppMsg::Toast {
                        text: "Old worker has no /term/interrupt (reboot F5 or wait timeout)".into(),
                        is_err: true,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Toast {
                        text: format!("Interrupt failed: {}", e),
                        is_err: true,
                    });
                }
            }
        });
    }

    pub fn spawn_run_remote_file(&mut self, side: Side) {
        let target = match self.pane(side) {
            PaneKind::RemoteFiles(rp) => rp
                .entries
                .get(rp.cursor)
                .filter(|e| e.name != ".." && !e.is_dir)
                .cloned(),
            _ => None,
        };
        let Some(ent) = target else {
            self.notify("Highlight a remote .py or .sh file to run.", true);
            return;
        };
        let low = ent.name.to_ascii_lowercase();
        let quoted = format!("'{}'", ent.path.replace('\'', "'\"'\"'"));
        let cmd = if low.ends_with(".py") {
            format!("python3 -u {}", quoted)
        } else if low.ends_with(".sh") {
            format!("bash {}", quoted)
        } else {
            self.notify("Highlight a .py or .sh file to run with Live Logs.", true);
            return;
        };
        self.spawn_job(cmd, String::new(), ent.name);
        if !matches!(self.left_pane, PaneKind::LiveLogs)
            && !matches!(self.right_pane, PaneKind::LiveLogs)
        {
            self.apply_preset(Preset::Cockpit);
        }
    }

    pub fn spawn_prewarm_all(&self) {
        let urls: Vec<String> = self
            .sessions
            .values()
            .map(|s| s.base_url())
            .filter(|u| !u.is_empty())
            .collect();
        let http = self.http.clone();
        tokio::spawn(async move {
            for u in urls {
                http.prewarm(&u).await;
            }
        });
    }

    pub fn poll_tick(&mut self) {
        self.last_poll_at = Instant::now();
        self.sessions = load_all_sessions(&self.state_dir);
        let slot = self.current_slot.clone();
        let Some(sess) = self.sessions.get(&slot).cloned() else {
            self.is_online = false;
            self.rtt_ms = None;
            self.status_line = format!("Slot {} Standby — F5 Boot / Ctrl+W Workers", slot);
            return;
        };

        if sess.mode == "batch" {
            let bridge = self.bridge.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                match bridge
                    .call("batch_logs", json!({ "slot": slot, "tail_lines": 80, "wait_sec": 2.0 }))
                    .await
                {
                    Ok(v) => {
                        let st = v.get("kaggle_api_status").and_then(|x| x.as_str()).unwrap_or("BATCH").to_string();
                        let logs = v.get("logs").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let _ = tx.send(AppMsg::BatchLogsDone { slot, status: st, logs });
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::MonitorErr { slot, tunnel_down: false, message: format!("Batch: {}", e) });
                    }
                }
            });
            return;
        }

        if let Some(until) = self.poll_backoff_until {
            if Instant::now() < until {
                return;
            }
        }

        let http = self.http.clone();
        let tx = self.tx.clone();
        let since_seq = *self.remote_seq_by_slot.get(&slot).unwrap_or(&0);
        let log_job_id = self.selected_job_id.clone();
        let log_offset = self.log_offset;
        tokio::spawn(async move {
            match http.monitor(&sess, since_seq, &log_job_id, log_offset).await {
                Ok((payload, rtt_ms)) => {
                    let _ = tx.send(AppMsg::MonitorOk { slot, payload, rtt_ms });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::MonitorErr {
                        slot,
                        tunnel_down: e.is_tunnel_down(),
                        message: e.to_string(),
                    });
                }
            }
        });
    }

    pub fn spawn_refresh_endpoint(&self, slot: String) {
        let bridge = self.bridge.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Ok(val) = bridge.call("refresh_endpoint", json!({ "slot": slot })).await {
                if let Ok(res) = serde_json::from_value::<RefreshEndpointRes>(val) {
                    let _ = tx.send(AppMsg::EndpointRefreshed { slot, res });
                }
            }
        });
    }

    pub fn spawn_quota(&self) {
        let bridge = self.bridge.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match bridge.call("quota", json!({})).await {
                Ok(v) => match serde_json::from_value::<QuotaSummary>(v) {
                    Ok(q) => { let _ = tx.send(AppMsg::QuotaOk(q)); }
                    Err(e) => { let _ = tx.send(AppMsg::QuotaErr(e.to_string())); }
                },
                Err(e) => { let _ = tx.send(AppMsg::QuotaErr(e.to_string())); }
            }
        });
    }

    pub fn spawn_fs_list(&mut self, side: Side, dir: String) {
        let Some(sess) = self.active_session() else {
            return;
        };
        if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
            rp.loading = true;
            rp.selected.clear();
        }
        let tid = self.begin_task(format!("List {}", dir));
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match http.fs_list(&sess, &dir).await {
                Ok(entries) => {
                    let _ = tx.send(AppMsg::FsListDone { side, dir, entries });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Toast { text: format!("List failed: {}", e), is_err: true });
                }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn spawn_open_remote_file(&mut self, path: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let tid = self.begin_task(format!("Open {}", path));
        let http = self.http.clone();
        let tx = self.tx.clone();
        let is_smart = is_smart_ext(&path);
        tokio::spawn(async move {
            if is_smart {
                match http.fs_inspect(&sess, &path).await {
                    Ok(res) => { let _ = tx.send(AppMsg::FsInspectDone(res)); }
                    Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Inspect failed: {}", e), is_err: true }); }
                }
            } else {
                match http.fs_read_text(&sess, &path, 65536).await {
                    Ok(res) => { let _ = tx.send(AppMsg::FsReadDone(res.path, res.content)); }
                    Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Open failed: {}", e), is_err: true }); }
                }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn open_local_file_editor(&mut self, path: PathBuf) {
        let raw = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.notify(format!("Read local file failed: {}", e), true);
                return;
            }
        };
        let slice = &raw[..raw.len().min(65536)];
        let txt = String::from_utf8_lossy(slice);
        let lines: Vec<String> = if txt.is_empty() {
            vec![String::new()]
        } else {
            txt.lines().map(|s| s.to_string()).collect()
        };
        self.modal = Some(Modal::FileEditor(EditorModalState {
            is_local: true,
            is_new: false,
            path: TextField::new(path.to_string_lossy().to_string()),
            lines,
            cur_row: 0,
            cur_col: 0,
            sel_anchor: None,
            scroll_row: 0,
            readonly: raw.len() > 65536,
            focus_path: false,
        }));
    }

    pub fn load_local_preview_into_drawer(&mut self, ent: &LocalEntry) {
        let pstr = ent.path.to_string_lossy().to_string();
        if ent.is_dir {
            let count = std::fs::read_dir(&ent.path).map(|rd| rd.count()).unwrap_or(0);
            self.preview_drawer = Some(PreviewDrawerState {
                target_path: pstr.clone(),
                title: ent.name.clone(),
                subtitle: format!("Local Directory · {} item(s)", count),
                lines: vec![
                    format!("Path: {}", pstr),
                    "→ / Enter: Open directory   t: Push to Remote   c: Copy".into(),
                ],
                scroll: 0,
            });
            return;
        }
        let raw = std::fs::read(&ent.path).unwrap_or_default();
        let preview = String::from_utf8_lossy(&raw[..raw.len().min(16384)]);
        let lines: Vec<String> = preview.lines().take(160).map(|s| s.to_string()).collect();
        self.preview_drawer = Some(PreviewDrawerState {
            target_path: pstr,
            title: ent.name.clone(),
            subtitle: format!("Local File · {}", fmt_bytes(ent.size_bytes)),
            lines: if lines.is_empty() { vec!["(empty file)".into()] } else { lines },
            scroll: 0,
        });
    }

    pub fn spawn_remote_preview(&mut self, path: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let http = self.http.clone();
        let tx = self.tx.clone();
        let smart = is_smart_ext(&path);
        tokio::spawn(async move {
            if smart {
                match http.fs_inspect(&sess, &path).await {
                    Ok(info) => {
                        let lines = format_inspect_lines(&info);
                        let _ = tx.send(AppMsg::PreviewDrawerLoaded(PreviewDrawerState {
                            target_path: info.path.clone(),
                            title: info.path,
                            subtitle: format!("{} · {}", info.format.to_uppercase(), fmt_bytes(info.size_bytes)),
                            lines,
                            scroll: 0,
                        }));
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::Toast { text: format!("Preview failed: {}", e), is_err: true });
                    }
                }
            } else {
                match http.fs_read_text(&sess, &path, 16384).await {
                    Ok(res) => {
                        let lines: Vec<String> = res.content.lines().take(160).map(|s| s.to_string()).collect();
                        let _ = tx.send(AppMsg::PreviewDrawerLoaded(PreviewDrawerState {
                            target_path: res.path.clone(),
                            title: res.path,
                            subtitle: format!(
                                "Text · {}{}",
                                fmt_bytes(res.size_bytes),
                                if res.truncated { " (truncated)" } else { "" }
                            ),
                            lines: if lines.is_empty() { vec!["(empty file)".into()] } else { lines },
                            scroll: 0,
                        }));
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::Toast { text: format!("Preview failed: {}", e), is_err: true });
                    }
                }
            }
        });
    }

    pub fn schedule_auto_preview(&mut self, side: Side) {
        if self.preview_drawer.is_none() {
            return;
        }
        match self.pane(side).clone() {
            PaneKind::LocalFiles(lp) => {
                if let Some(ent) = lp.entries.get(lp.cursor) {
                    if ent.name != ".." {
                        self.load_local_preview_into_drawer(ent);
                    }
                }
            }
            PaneKind::RemoteFiles(rp) => {
                if let Some(ent) = rp.entries.get(rp.cursor) {
                    if ent.name == ".." {
                        return;
                    }
                    if ent.is_dir {
                        self.preview_pending = None;
                        self.preview_drawer = Some(PreviewDrawerState {
                            target_path: ent.path.clone(),
                            title: ent.path.clone(),
                            subtitle: "Remote Directory".into(),
                            lines: vec![
                                format!("Path: {}", ent.path),
                                "→ / Enter: Open dir   t: Transfer   u: Analyze DU".into(),
                            ],
                            scroll: 0,
                        });
                    } else {
                        self.preview_pending = Some((
                            side,
                            ent.path.clone(),
                            Instant::now() + Duration::from_millis(180),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    pub fn tick_pending_preview(&mut self) -> bool {
        let Some((side, path, deadline)) = self.preview_pending.clone() else {
            return false;
        };
        if Instant::now() < deadline {
            return false;
        }
        self.preview_pending = None;
        if self.preview_drawer.is_none() || self.active_side != side {
            return false;
        }
        self.spawn_remote_preview(path);
        true
    }

    pub fn open_preview_or_promote(&mut self, side: Side) {
        match self.pane(side).clone() {
            PaneKind::LocalFiles(lp) => {
                let Some(ent) = lp.entries.get(lp.cursor) else {
                    return;
                };
                if ent.is_dir {
                    return;
                }
                let pstr = ent.path.to_string_lossy().to_string();
                if self.preview_drawer.as_ref().map(|d| d.target_path == pstr).unwrap_or(false) {
                    self.open_local_file_editor(ent.path.clone());
                } else {
                    self.load_local_preview_into_drawer(ent);
                }
            }
            PaneKind::RemoteFiles(rp) => {
                let Some(ent) = rp.entries.get(rp.cursor) else {
                    return;
                };
                if ent.is_dir {
                    return;
                }
                if self
                    .preview_drawer
                    .as_ref()
                    .map(|d| d.target_path == ent.path)
                    .unwrap_or(false)
                {
                    self.spawn_open_remote_file(ent.path.clone());
                } else {
                    self.preview_pending = None;
                    self.spawn_remote_preview(ent.path.clone());
                }
            }
            _ => {}
        }
    }

    pub fn toggle_or_load_preview_drawer(&mut self, force_open: bool) {
        if !force_open && self.preview_drawer.is_some() {
            self.preview_drawer = None;
            self.preview_pending = None;
            return;
        }
        match self.active_pane().clone() {
            PaneKind::LocalFiles(lp) => {
                let Some(ent) = lp.entries.get(lp.cursor) else {
                    self.notify("No local file selected.", true);
                    return;
                };
                if ent.name == ".." {
                    self.notify("Highlight a file or folder to preview.", false);
                    return;
                }
                self.load_local_preview_into_drawer(ent);
            }
            PaneKind::RemoteFiles(rp) => {
                let Some(ent) = rp.entries.get(rp.cursor) else {
                    self.notify("No remote item selected.", true);
                    return;
                };
                if ent.name == ".." {
                    self.notify("Highlight a file or folder to preview.", false);
                    return;
                }
                if ent.is_dir {
                    self.preview_drawer = Some(PreviewDrawerState {
                        target_path: ent.path.clone(),
                        title: ent.path.clone(),
                        subtitle: "Remote Directory".into(),
                        lines: vec![
                            format!("Path: {}", ent.path),
                            "→ / Enter: Open directory   t: Transfer   u: DU   a: More".into(),
                        ],
                        scroll: 0,
                    });
                    return;
                }
                self.spawn_remote_preview(ent.path.clone());
            }
            _ => self.notify("Preview Drawer works inside Local or Remote Files panels.", true),
        }
    }

    /// Symmetric 1-key cross-panel transfer (`t`):
    /// - Local -> opposite Remote dir (fs_push)
    /// - Remote -> opposite Local dir (fs_pull)
    /// - Remote -> opposite Remote dir (fs_copy)
    /// - Local -> opposite Local dir (copy)
    pub fn transfer_to_opposite(&mut self) {
        let src_side = self.active_side;
        let dst_side = src_side.opposite();

        enum Op {
            Push(Vec<String>, String),
            Pull(Vec<String>, String),
            RemoteCopy(Vec<(String, String)>),
            LocalCopy(Vec<(PathBuf, PathBuf)>),
            Err(String),
        }

        let op = match (self.pane(src_side), self.pane(dst_side)) {
            (PaneKind::LocalFiles(lp), PaneKind::RemoteFiles(rp)) => {
                let mut paths: Vec<String> = lp.selected.iter().map(|p| p.to_string_lossy().to_string()).collect();
                if paths.is_empty() {
                    if let Some(ent) = lp.entries.get(lp.cursor) {
                        if ent.name != ".." {
                            paths.push(ent.path.to_string_lossy().to_string());
                        }
                    }
                }
                if paths.is_empty() {
                    Op::Err("Select a local file/folder first.".into())
                } else if rp.dir.starts_with("/kaggle/input") {
                    Op::Err("Opposite remote dir /kaggle/input is read-only.".into())
                } else {
                    Op::Push(paths, rp.dir.clone())
                }
            }
            (PaneKind::LocalFiles(src_lp), PaneKind::LocalFiles(dst_lp)) => {
                let mut pairs = Vec::new();
                for p in &src_lp.selected {
                    if let Some(name) = p.file_name() {
                        pairs.push((p.clone(), dst_lp.dir.join(name)));
                    }
                }
                if pairs.is_empty() {
                    if let Some(ent) = src_lp.entries.get(src_lp.cursor) {
                        if ent.name != ".." {
                            pairs.push((ent.path.clone(), dst_lp.dir.join(&ent.name)));
                        }
                    }
                }
                if pairs.is_empty() {
                    Op::Err("Select a local file/folder first.".into())
                } else {
                    Op::LocalCopy(pairs)
                }
            }
            (PaneKind::LocalFiles(lp), _) => {
                let mut paths: Vec<String> = lp.selected.iter().map(|p| p.to_string_lossy().to_string()).collect();
                if paths.is_empty() {
                    if let Some(ent) = lp.entries.get(lp.cursor) {
                        if ent.name != ".." {
                            paths.push(ent.path.to_string_lossy().to_string());
                        }
                    }
                }
                if paths.is_empty() {
                    Op::Err("Select a local file/folder first.".into())
                } else {
                    Op::Push(paths, "/kaggle/working".into())
                }
            }
            (PaneKind::RemoteFiles(src_rp), PaneKind::LocalFiles(dst_lp)) => {
                let mut paths: Vec<String> = src_rp.selected.iter().cloned().collect();
                if paths.is_empty() {
                    if let Some(ent) = src_rp.entries.get(src_rp.cursor) {
                        if ent.name != ".." {
                            paths.push(ent.path.clone());
                        }
                    }
                }
                if paths.is_empty() {
                    Op::Err("Select a remote file/folder first.".into())
                } else {
                    Op::Pull(paths, dst_lp.dir.to_string_lossy().to_string())
                }
            }
            (PaneKind::RemoteFiles(src_rp), PaneKind::RemoteFiles(dst_rp)) => {
                let mut items: Vec<(String, String)> = src_rp
                    .selected
                    .iter()
                    .map(|p| {
                        let name = p.rsplit('/').next().unwrap_or("copy");
                        (p.clone(), format!("{}/{}", dst_rp.dir.trim_end_matches('/'), name))
                    })
                    .collect();
                if items.is_empty() {
                    if let Some(ent) = src_rp.entries.get(src_rp.cursor) {
                        if ent.name != ".." {
                            items.push((
                                ent.path.clone(),
                                format!("{}/{}", dst_rp.dir.trim_end_matches('/'), ent.name),
                            ));
                        }
                    }
                }
                if items.is_empty() {
                    Op::Err("Select a remote item first.".into())
                } else if dst_rp.dir.starts_with("/kaggle/input") {
                    Op::Err("Opposite remote dir /kaggle/input is read-only.".into())
                } else {
                    Op::RemoteCopy(items)
                }
            }
            (PaneKind::RemoteFiles(src_rp), _) => {
                let mut paths: Vec<String> = src_rp.selected.iter().cloned().collect();
                if paths.is_empty() {
                    if let Some(ent) = src_rp.entries.get(src_rp.cursor) {
                        if ent.name != ".." {
                            paths.push(ent.path.clone());
                        }
                    }
                }
                if paths.is_empty() {
                    Op::Err("Select a remote file/folder first.".into())
                } else {
                    Op::Pull(paths, "./kaggle_downloads".into())
                }
            }
            _ => Op::Err("Transfer `t` applies inside Local or Remote Files panels.".into()),
        };

        match op {
            Op::Err(msg) => self.notify(msg, true),
            Op::Push(paths, rdir) => {
                for p in paths {
                    self.spawn_sync(true, p, rdir.clone());
                }
            }
            Op::Pull(paths, ldir) => {
                for p in paths {
                    self.spawn_sync(false, ldir.clone(), p);
                }
            }
            Op::LocalCopy(pairs) => {
                let mut cnt = 0usize;
                for (s, d) in pairs {
                    if s.is_dir() {
                        if copy_dir_recursive(&s, &d).is_ok() {
                            cnt += 1;
                        }
                    } else if std::fs::copy(&s, &d).is_ok() {
                        cnt += 1;
                    }
                }
                self.reload_all_local_panes();
                self.notify(format!("Copied {} local item(s)", cnt), cnt == 0);
            }
            Op::RemoteCopy(pairs) => {
                let Some(sess) = self.active_session() else {
                    self.notify("No active worker session.", true);
                    return;
                };
                let tid = self.begin_task(format!("Copy {} remote", pairs.len()));
                let http = self.http.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let cnt = pairs.len();
                    for (s, d) in pairs {
                        if let Err(e) = http.fs_copy(&sess, &s, &d).await {
                            let _ = tx.send(AppMsg::Toast { text: format!("Copy failed: {}", e), is_err: true });
                            let _ = tx.send(AppMsg::TaskFinished(tid));
                            return;
                        }
                    }
                    let _ = tx.send(AppMsg::FsOpDone {
                        msg: format!("Copied {} item(s) across remote panels", cnt),
                        refresh: true,
                    });
                    let _ = tx.send(AppMsg::TaskFinished(tid));
                });
            }
        }
    }

    pub fn yank_or_cut_files(&mut self, side: Side, op: ClipOp) {
        let clip = match self.pane(side) {
            PaneKind::LocalFiles(lp) => {
                let mut paths: Vec<PathBuf> = lp.selected.iter().cloned().collect();
                if paths.is_empty() {
                    if let Some(ent) = lp.entries.get(lp.cursor) {
                        if ent.name != ".." {
                            paths.push(ent.path.clone());
                        }
                    }
                }
                if paths.is_empty() {
                    self.notify("Highlight or select local item(s) first.", true);
                    return;
                }
                FileClipboard { op, source: ClipSource::Local(paths) }
            }
            PaneKind::RemoteFiles(rp) => {
                let mut paths: Vec<String> = rp.selected.iter().cloned().collect();
                if paths.is_empty() {
                    if let Some(ent) = rp.entries.get(rp.cursor) {
                        if ent.name != ".." {
                            paths.push(ent.path.clone());
                        }
                    }
                }
                if paths.is_empty() {
                    self.notify("Highlight or select remote item(s) first.", true);
                    return;
                }
                if op == ClipOp::Cut && paths.iter().any(|p| p.starts_with("/kaggle/input")) {
                    self.notify("/kaggle/input is read-only (use Copy `c` instead).", true);
                    return;
                }
                FileClipboard { op, source: ClipSource::Remote(paths) }
            }
            _ => {
                self.notify("Clipboard works in Local or Remote Files panels.", true);
                return;
            }
        };
        let msg = format!("Staged {} {} item(s) — navigate & press `p` to paste", clip.count(), clip.label());
        self.file_clipboard = Some(clip);
        self.notify(msg, false);
    }

    pub fn paste_files(&mut self, side: Side) {
        let Some(clip) = self.file_clipboard.clone() else {
            self.notify("Clipboard is empty (press `c` Copy or `x` Cut first).", true);
            return;
        };
        match (self.pane(side).clone(), clip.source) {
            (PaneKind::LocalFiles(dst_lp), ClipSource::Local(srcs)) => {
                let mut cnt = 0usize;
                for src in &srcs {
                    let Some(name) = src.file_name() else { continue };
                    let dst = dst_lp.dir.join(name);
                    if &dst == src {
                        continue;
                    }
                    let ok = match clip.op {
                        ClipOp::Cut => std::fs::rename(src, &dst).is_ok(),
                        ClipOp::Copy => {
                            if src.is_dir() {
                                copy_dir_recursive(src, &dst).is_ok()
                            } else {
                                std::fs::copy(src, &dst).is_ok()
                            }
                        }
                    };
                    if ok {
                        cnt += 1;
                    }
                }
                if clip.op == ClipOp::Cut {
                    self.file_clipboard = None;
                }
                self.reload_all_local_panes();
                self.notify(format!("Pasted {} local item(s)", cnt), cnt == 0);
            }
            (PaneKind::RemoteFiles(dst_rp), ClipSource::Remote(srcs)) => {
                if dst_rp.dir.starts_with("/kaggle/input") {
                    self.notify("/kaggle/input is read-only.", true);
                    return;
                }
                let Some(sess) = self.active_session() else {
                    self.notify("No active worker session.", true);
                    return;
                };
                let dst_dir = dst_rp.dir.trim_end_matches('/').to_string();
                let is_cut = clip.op == ClipOp::Cut;
                if is_cut {
                    self.file_clipboard = None;
                }
                let tid = self.begin_task(format!("Paste {} remote", srcs.len()));
                let http = self.http.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let mut cnt = 0usize;
                    for s in srcs {
                        let name = s.rsplit('/').next().unwrap_or("item");
                        let d = format!("{}/{}", dst_dir, name);
                        if d == s {
                            continue;
                        }
                        let res = if is_cut {
                            http.fs_rename(&sess, &s, &d).await
                        } else {
                            http.fs_copy(&sess, &s, &d).await
                        };
                        match res {
                            Ok(_) => cnt += 1,
                            Err(e) => {
                                let _ = tx.send(AppMsg::Toast { text: format!("Paste failed: {}", e), is_err: true });
                                let _ = tx.send(AppMsg::TaskFinished(tid));
                                return;
                            }
                        }
                    }
                    let _ = tx.send(AppMsg::FsOpDone {
                        msg: format!("Pasted {} remote item(s)", cnt),
                        refresh: true,
                    });
                    let _ = tx.send(AppMsg::TaskFinished(tid));
                });
            }
            (PaneKind::RemoteFiles(dst_rp), ClipSource::Local(srcs)) => {
                if dst_rp.dir.starts_with("/kaggle/input") {
                    self.notify("/kaggle/input is read-only.", true);
                    return;
                }
                let rdir = dst_rp.dir;
                for src in srcs {
                    self.spawn_sync(true, src.to_string_lossy().to_string(), rdir.clone());
                }
                if clip.op == ClipOp::Cut {
                    self.notify("Pushed to Kaggle (local originals preserved safely).", false);
                    self.file_clipboard = None;
                }
            }
            (PaneKind::LocalFiles(dst_lp), ClipSource::Remote(srcs)) => {
                let ldir = dst_lp.dir.to_string_lossy().to_string();
                for src in srcs {
                    self.spawn_sync(false, ldir.clone(), src);
                }
                if clip.op == ClipOp::Cut {
                    self.notify("Pulled to local (remote originals preserved safely).", false);
                    self.file_clipboard = None;
                }
            }
            _ => self.notify("Paste `p` works inside Local or Remote Files panels.", true),
        }
    }

    pub fn spawn_sync(&mut self, is_push: bool, local_path: String, remote_path: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let label = if is_push {
            format!("Push {}", Path::new(&local_path).file_name().and_then(|s| s.to_str()).unwrap_or("files"))
        } else {
            format!("Pull {}", remote_path.rsplit('/').next().unwrap_or("files"))
        };
        let tid = self.begin_task(label);
        let http = self.http.clone();
        let tx = self.tx.clone();
        let slot = self.current_slot.clone();
        self.conduit.trigger_tx("fs_sync", &slot, &local_path);
        tokio::spawn(async move {
            if is_push {
                match http.fs_push(&sess, Path::new(&local_path), &remote_path).await {
                    Ok((cnt, kb)) => {
                        let _ = tx.send(AppMsg::SyncDone {
                            msg: format!("Pushed {} items ({:.1} KB) -> {}", cnt, kb, remote_path),
                            refresh_remote: true,
                            refresh_local: false,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::Toast { text: format!("Push failed: {}", e), is_err: true });
                    }
                }
            } else {
                match http.fs_pull(&sess, &remote_path, Path::new(&local_path)).await {
                    Ok((cnt, kb, dest)) => {
                        let _ = tx.send(AppMsg::SyncDone {
                            msg: format!("Pulled {} files ({:.1} KB) -> {}", cnt, kb, dest.display()),
                            refresh_remote: false,
                            refresh_local: true,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::Toast { text: format!("Pull failed: {}", e), is_err: true });
                    }
                }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn spawn_hub_search(&mut self) {
        let cat = HUB_CATEGORIES[self.hub_cat_idx].0.to_string();
        let q = self.hub_search.value.trim().to_string();
        let tid = self.begin_task(format!("Hub {}", cat));
        let bridge = self.bridge.clone();
        let tx = self.tx.clone();
        self.notify(format!("Searching Kaggle {}...", cat), false);
        tokio::spawn(async move {
            match bridge.call("hub_search", json!({ "category": cat, "query": q })).await {
                Ok(v) => {
                    let items: Vec<HubItem> = serde_json::from_value(v).unwrap_or_default();
                    let _ = tx.send(AppMsg::HubSearchDone(items));
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Toast { text: format!("Hub search failed: {}", e), is_err: true });
                }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn spawn_hub_inspect(&mut self, category: String, item_ref: String) {
        let tid = self.begin_task(format!("Inspect {}", item_ref));
        let bridge = self.bridge.clone();
        let tx = self.tx.clone();
        self.notify(format!("Inspecting {}...", item_ref), false);
        tokio::spawn(async move {
            match bridge.call("hub_inspect", json!({ "category": category, "ref": item_ref })).await {
                Ok(v) => {
                    if let Ok(detail) = serde_json::from_value::<HubDetail>(v) {
                        let _ = tx.send(AppMsg::HubInspectDone(detail));
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Toast { text: format!("Hub inspect failed: {}", e), is_err: true });
                }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn spawn_cluster_probe(&self) {
        let sessions = load_all_sessions(&self.state_dir);
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut set = tokio::task::JoinSet::new();
            for (slot, sess) in sessions {
                let h = http.clone();
                set.spawn(async move {
                    let mode = sess.mode.clone();
                    let acc = sess.accelerator.clone();
                    if mode != "live" || sess.base_url().is_empty() {
                        return ClusterRow {
                            slot,
                            mode: mode.clone(),
                            accelerator: acc,
                            state: if mode == "batch" { "BATCH".into() } else { "OFFLINE".into() },
                            compute: "--".into(),
                            ram: "--".into(),
                            jobs: "--".into(),
                            drain: "--".into(),
                            rtt: "--".into(),
                        };
                    }
                    match h.monitor(&sess, 0, "", 0).await {
                        Ok((p, rtt)) => {
                            let st = &p.status;
                            let compute = if let Some(g0) = st.gpus.first() {
                                format!("{:.0}% · {:.0}/{:.0}MB", g0.util_pct, g0.vram_used_mb, g0.vram_total_mb)
                            } else {
                                format!("CPU {:.0}%", st.cpu_pct)
                            };
                            let ram = format!("{:.1}/{:.1}G", st.ram.used_gb, st.ram.total_gb);
                            let run_cnt = p.jobs.iter().filter(|j| j.status == "running").count();
                            let jobs = format!("{} run / {}", run_cnt, p.jobs.len());
                            let drain_s = st.remaining_before_drain_sec.max(0.0) as u64;
                            let drain = format!("{}h{:02}m", drain_s / 3600, (drain_s % 3600) / 60);
                            ClusterRow {
                                slot,
                                mode,
                                accelerator: acc,
                                state: "ONLINE".into(),
                                compute,
                                ram,
                                jobs,
                                drain,
                                rtt: format!("{:.0} ms", rtt),
                            }
                        }
                        Err(e) => ClusterRow {
                            slot,
                            mode,
                            accelerator: acc,
                            state: format!("ERR ({})", e.to_string().chars().take(14).collect::<String>()),
                            compute: "--".into(),
                            ram: "--".into(),
                            jobs: "--".into(),
                            drain: "--".into(),
                            rtt: "--".into(),
                        },
                    }
                });
            }
            let mut rows = Vec::new();
            while let Some(res) = set.join_next().await {
                if let Ok(r) = res {
                    rows.push(r);
                }
            }
            rows.sort_by(|a, b| a.slot.cmp(&b.slot));
            let _ = tx.send(AppMsg::ClusterRows(rows));
        });
    }

    pub fn spawn_du(&mut self, side: Side, dir: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let tid = self.begin_task(format!("DU {}", dir));
        let http = self.http.clone();
        let tx = self.tx.clone();
        self.notify(format!("Scanning disk usage for {}...", dir), false);
        tokio::spawn(async move {
            match http.fs_du(&sess, &dir).await {
                Ok(res) => { let _ = tx.send(AppMsg::FsDuDone { side, dir, res }); }
                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("DU failed: {}", e), is_err: true }); }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn spawn_find(&mut self, side: Side, root: String, query: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let tid = self.begin_task(format!("Find {}", query));
        let is_grep = query.starts_with('/') && query.len() > 1;
        let (glob, regex) = if is_grep {
            (String::new(), query[1..].to_string())
        } else {
            (query.clone(), String::new())
        };
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match http.fs_find(&sess, &root, &glob, &regex).await {
                Ok(res) => { let _ = tx.send(AppMsg::FsFindDone { side, query, res }); }
                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Find failed: {}", e), is_err: true }); }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    // ── Command Execution ─────────────────────────────────────────────────────

    pub fn submit_prompt(&mut self) {
        let cmd = self.prompt.value.trim().to_string();
        self.prompt.set("");
        if cmd.is_empty() {
            return;
        }
        if self.cmd_history.last().map(|s| s.as_str()) != Some(&cmd) {
            self.cmd_history.push(cmd.clone());
        }
        self.cmd_hist_idx = self.cmd_history.len();
        self.cmd_draft.clear();
        self.run_command(cmd);
    }

    pub fn run_command(&mut self, cmd: String) {
        match cmd.as_str() {
            ":q" | ":quit" | "exit" => {
                self.should_quit = true;
                return;
            }
            ":clear" => {
                self.activities.clear();
                return;
            }
            ":workers" | ":cluster" | ":quota" => {
                self.open_workers_modal(0);
                return;
            }
            ":boot" => {
                self.open_workers_modal(1);
                return;
            }
            ":sync" => {
                self.open_sync_modal();
                return;
            }
            ":flow" => {
                self.conduit.trigger_tx("nvidia-smi", &self.current_slot, "MCP Plasma Pulse");
                self.notify("MCP Plasma Pulse triggered", false);
                return;
            }
            ":du" => {
                self.apply_preset(Preset::DualFiles);
                self.spawn_du(Side::Right, "/kaggle/working".into());
                return;
            }
            _ => {}
        }
        if let Some(rest) = cmd.strip_prefix(":slot ") {
            let s = normalize_slot(rest);
            self.switch_slot(s.clone());
            self.notify(format!("Switched to slot '{}'", s), false);
            return;
        }
        if cmd == ":down" {
            let slot = self.current_slot.clone();
            let bridge = self.bridge.clone();
            let tx = self.tx.clone();
            self.notify(format!("Shutting down slot '{}'...", slot), false);
            tokio::spawn(async move {
                match bridge.call("down", json!({ "slot": slot })).await {
                    Ok(_) => { let _ = tx.send(AppMsg::BootDone(format!("Shut down slot '{}'", slot))); }
                    Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Shutdown failed: {}", e), is_err: true }); }
                }
            });
            return;
        }
        if cmd.starts_with(":up") {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            let mut acc = "NvidiaTeslaT4".to_string();
            let mut slots = self.current_slot.clone();
            let mut ds = String::new();
            let mut mdls = String::new();
            let mut comp = String::new();
            let mut idle_min = 0u32;
            let mut chain = false;
            let mut i = 1;
            while i < parts.len() {
                match parts[i] {
                    "-a" | "--acc" if i + 1 < parts.len() => { acc = parts[i + 1].into(); i += 2; }
                    "-s" | "--slot" if i + 1 < parts.len() => { slots = parts[i + 1].into(); i += 2; }
                    "-d" | "--datasets" if i + 1 < parts.len() => { ds = parts[i + 1].into(); i += 2; }
                    "-m" | "--models" if i + 1 < parts.len() => { mdls = parts[i + 1].into(); i += 2; }
                    "--comp" if i + 1 < parts.len() => { comp = parts[i + 1].into(); i += 2; }
                    "--idle" if i + 1 < parts.len() => { idle_min = parts[i + 1].parse().unwrap_or(0); i += 2; }
                    "--chain" => { chain = true; i += 1; }
                    _ => i += 1,
                }
            }
            self.spawn_boot(acc, slots, ds, mdls, comp, idle_min, chain);
            return;
        }
        if let Some(jid) = cmd.strip_prefix(":kill ") {
            self.spawn_kill_job(jid.trim().to_string());
            return;
        }
        if let Some(rest) = cmd.strip_prefix(":spawn ").or_else(|| cmd.strip_prefix(":run ")) {
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            let (gpus, spawn_cmd) = if tokens.len() >= 3 && (tokens[0] == "--gpus" || tokens[0] == "-g") {
                (tokens[1].to_string(), tokens[2..].join(" "))
            } else {
                (String::new(), rest.trim().to_string())
            };
            self.spawn_job(spawn_cmd, gpus, String::new());
            return;
        }

        let Some(sess) = self.active_session() else {
            self.notify("No active worker session (press F5 or Ctrl+W).", true);
            return;
        };
        let slot = self.current_slot.clone();
        self.running_exec = Some(RunningExec {
            command: cmd.clone(),
            started_at: Instant::now(),
            live_output: String::new(),
            worker_has_live: false,
        });
        let tid = self.begin_task(format!("$ {}", cmd));
        self.conduit.trigger_tx("term_exec", &slot, &cmd);
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match http.term_exec(&sess, &cmd, "default", 120.0).await {
                Ok(res) => { let _ = tx.send(AppMsg::ExecDone { slot, cmd, res }); }
                Err(e) => { let _ = tx.send(AppMsg::ExecErr { slot, cmd, err: e.to_string() }); }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn spawn_job(&mut self, spawn_cmd: String, gpus: String, name: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let slot = self.current_slot.clone();
        let cwd = if self.known_cwd.is_empty() { "/kaggle/working".into() } else { self.known_cwd.clone() };
        self.conduit.trigger_tx("job_spawn", &slot, &spawn_cmd);
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match http.job_spawn(&sess, &spawn_cmd, &gpus, &cwd, &name).await {
                Ok(meta) => {
                    let _ = tx.send(AppMsg::JobsActionDone {
                        msg: format!("Spawned {} (pid={:?})", meta.job_id, meta.pid),
                        selected_job_id: Some(meta.job_id),
                    });
                }
                Err(e) => { let _ = tx.send(AppMsg::ExecErr { slot, cmd: spawn_cmd, err: e.to_string() }); }
            }
        });
    }

    pub fn spawn_kill_job(&mut self, jid: String) {
        let Some(sess) = self.active_session() else {
            self.notify("No active worker session.", true);
            return;
        };
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match http.job_kill(&sess, &jid, "SIGTERM").await {
                Ok(_) => { let _ = tx.send(AppMsg::JobsActionDone { msg: format!("Sent SIGTERM to {}", jid), selected_job_id: None }); }
                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Kill job failed: {}", e), is_err: true }); }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_boot(
        &mut self,
        acc: String,
        slots: String,
        datasets: String,
        models: String,
        competition: String,
        idle_min: u32,
        chain: bool,
    ) {
        let tid = self.begin_task(format!("Boot {}", slots));
        self.conduit.trigger_tx("kaggle_up", &slots, &acc);
        self.notify(format!("Booting slot(s) {} ({})...", slots, acc), false);
        let bridge = self.bridge.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match bridge
                .call(
                    "boot",
                    json!({
                        "accelerator": acc,
                        "slots": slots,
                        "datasets": datasets,
                        "models": models,
                        "competition": competition,
                        "idle_timeout_min": idle_min,
                        "chain_previous_output": chain,
                    }),
                )
                .await
            {
                Ok(_) => { let _ = tx.send(AppMsg::BootDone(format!("Slot(s) {} READY!", slots))); }
                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Boot failed: {}", e), is_err: true }); }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn navigate_remote_up(&mut self, side: Side) {
        let parent_info = match self.pane_mut(side) {
            PaneKind::RemoteFiles(ref mut rp) => {
                let Some(parent) = parent_remote_dir(&rp.dir) else {
                    return;
                };
                let exited = rp
                    .dir
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if let Some(cur) = rp.entries.get(rp.cursor) {
                    if cur.name != ".." {
                        rp.dir_history.insert(rp.dir.clone(), cur.name.clone());
                    }
                }
                rp.pending_select_name = if exited.is_empty() { None } else { Some(exited) };
                rp.loading = true;
                Some(parent)
            }
            _ => None,
        };
        if let Some(parent) = parent_info {
            self.spawn_fs_list(side, parent);
        }
    }

    pub fn navigate_remote_into(&mut self, side: Side, target: String, cur_name: Option<String>) {
        if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
            if let Some(name) = cur_name {
                if name != ".." {
                    rp.dir_history.insert(rp.dir.clone(), name);
                }
            }
            rp.pending_select_name = None;
            rp.loading = true;
        }
        self.spawn_fs_list(side, target);
    }

    pub(super) fn handle_jobs_pane_key(&mut self, key: KeyEvent, is_repeat: bool) {
        match key.code {
            KeyCode::Left => self.active_side = Side::Left,
            KeyCode::Right => self.active_side = Side::Right,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected_job_idx = self.selected_job_idx.saturating_sub(1);
                if let Some(j) = self.jobs.get(self.selected_job_idx) {
                    if j.job_id != self.selected_job_id {
                        self.selected_job_id = j.job_id.clone();
                        self.log_offset = 0;
                        self.raw_log_lines.clear();
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_job_idx + 1 < self.jobs.len() {
                    self.selected_job_idx += 1;
                }
                if let Some(j) = self.jobs.get(self.selected_job_idx) {
                    if j.job_id != self.selected_job_id {
                        self.selected_job_id = j.job_id.clone();
                        self.log_offset = 0;
                        self.raw_log_lines.clear();
                    }
                }
            }
            KeyCode::Char('n') if !is_repeat => {
                self.modal = Some(Modal::SpawnJob(SpawnJobModalState {
                    command: TextField::new(""),
                    gpu_ids: TextField::new(""),
                    name: TextField::new(""),
                    field_idx: 0,
                }));
            }
            KeyCode::Char('x') if !is_repeat => {
                if !self.selected_job_id.is_empty() {
                    self.spawn_kill_job(self.selected_job_id.clone());
                }
            }
            KeyCode::Char('[') => self.selected_gpu_proc_idx = self.selected_gpu_proc_idx.saturating_sub(1),
            KeyCode::Char(']') => {
                if self.selected_gpu_proc_idx + 1 < self.gpu_procs.len() {
                    self.selected_gpu_proc_idx += 1;
                }
            }
            KeyCode::Char('K') if !is_repeat => self.trigger_kill_gpu_proc(),
            _ => {}
        }
    }

    pub(super) fn handle_logs_pane_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.active_side = Side::Left,
            KeyCode::Right => self.active_side = Side::Right,
            KeyCode::Up | KeyCode::PageUp => self.log_scroll = self.log_scroll.saturating_sub(6),
            KeyCode::Down | KeyCode::PageDown => self.log_scroll = self.log_scroll.saturating_add(6),
            KeyCode::Char('/') => self.input_focus = InputFocus::LogFilter,
            KeyCode::Char(' ') => {
                self.logs_paused = !self.logs_paused;
                self.notify(if self.logs_paused { "Logs PAUSED" } else { "Logs RESUMED" }, false);
            }
            KeyCode::Char('y') => {
                let txt = self
                    .filtered_log_lines()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                copy_clipboard(&txt);
                self.notify(format!("Copied {} log chars", txt.len()), false);
            }
            _ => {}
        }
    }

    pub(super) fn handle_hub_pane_key(&mut self, side: Side, key: KeyEvent, is_repeat: bool) {
        if matches!(self.pane(side), PaneKind::HubDetail) {
            match key.code {
                KeyCode::Left => self.active_side = Side::Left,
                KeyCode::Up | KeyCode::Char('k') => self.hub_detail_scroll = self.hub_detail_scroll.saturating_sub(2),
                KeyCode::Down | KeyCode::Char('j') => self.hub_detail_scroll = self.hub_detail_scroll.saturating_add(2),
                KeyCode::PageUp => self.hub_detail_scroll = self.hub_detail_scroll.saturating_sub(10),
                KeyCode::PageDown => self.hub_detail_scroll = self.hub_detail_scroll.saturating_add(10),
                KeyCode::Char('s') if !is_repeat => self.hub_stage_current(),
                KeyCode::Char('d') if !is_repeat => self.hub_download_current(),
                KeyCode::Char('l') if !is_repeat => self.hub_fetch_logs_current(),
                KeyCode::Char('D') if !is_repeat => self.hub_delete_kernel_current(),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Left => {
                self.hub_cat_idx = if self.hub_cat_idx == 0 {
                    HUB_CATEGORIES.len() - 1
                } else {
                    self.hub_cat_idx - 1
                };
                self.spawn_hub_search();
            }
            KeyCode::Right => {
                self.hub_cat_idx = (self.hub_cat_idx + 1) % HUB_CATEGORIES.len();
                self.spawn_hub_search();
            }
            KeyCode::Up | KeyCode::Char('k') => self.hub_cursor = self.hub_cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.hub_cursor + 1 < self.hub_items.len() {
                    self.hub_cursor += 1;
                }
            }
            KeyCode::PageUp => self.hub_detail_scroll = self.hub_detail_scroll.saturating_sub(6),
            KeyCode::PageDown => self.hub_detail_scroll = self.hub_detail_scroll.saturating_add(6),
            KeyCode::Char('/') => self.input_focus = InputFocus::HubSearch,
            KeyCode::Enter => {
                if let Some(item) = self.hub_items.get(self.hub_cursor).cloned() {
                    self.spawn_hub_inspect(item.category, item.item_ref);
                }
            }
            KeyCode::Char('s') if !is_repeat => self.hub_stage_current(),
            KeyCode::Char('d') if !is_repeat => self.hub_download_current(),
            KeyCode::Char('l') if !is_repeat => self.hub_fetch_logs_current(),
            KeyCode::Char('D') if !is_repeat => self.hub_delete_kernel_current(),
            _ => {}
        }
    }

    pub(super) fn hub_stage_current(&mut self) {
        if let Some(ref d) = self.hub_detail {
            if let Some(ref kind) = d.stage_kind {
                let val = if d.stage_value.is_empty() { d.item_ref.clone() } else { d.stage_value.clone() };
                match kind.as_str() {
                    "dataset" if !self.staged_datasets.contains(&val) => self.staged_datasets.push(val.clone()),
                    "model" if !self.staged_models.contains(&val) => self.staged_models.push(val.clone()),
                    "competition" => self.staged_competition = val.clone(),
                    _ => {}
                }
                self.notify(format!("Staged {} '{}'", kind, val), false);
            }
        }
    }

    pub(super) fn hub_download_current(&mut self) {
        if let Some(d) = self.hub_detail.clone() {
            if d.can_download {
                let tid = self.begin_task(format!("DL {}", d.item_ref));
                let bridge = self.bridge.clone();
                let tx = self.tx.clone();
                let cat = d.category;
                let iref = d.item_ref;
                self.notify(format!("Downloading {}...", iref), false);
                tokio::spawn(async move {
                    match bridge.call("hub_download", json!({ "category": cat, "ref": iref })).await {
                        Ok(v) => {
                            let dest = v.get("dest").and_then(|x| x.as_str()).unwrap_or("./kaggle_downloads");
                            let _ = tx.send(AppMsg::Toast { text: format!("Downloaded -> {}", dest), is_err: false });
                        }
                        Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Download failed: {}", e), is_err: true }); }
                    }
                    let _ = tx.send(AppMsg::TaskFinished(tid));
                });
            }
        }
    }

    pub(super) fn hub_fetch_logs_current(&mut self) {
        if let Some(d) = self.hub_detail.clone() {
            if d.category == "my_kernels" {
                let tid = self.begin_task(format!("Logs {}", d.item_ref));
                let bridge = self.bridge.clone();
                let tx = self.tx.clone();
                let kref = d.item_ref;
                self.notify("Fetching kernel logs...", false);
                tokio::spawn(async move {
                    match bridge.call("hub_kernel_logs", json!({ "kernel_ref": kref })).await {
                        Ok(v) => {
                            let txt = v.get("logs").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let _ = tx.send(AppMsg::HubLogsDone(txt));
                        }
                        Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Logs failed: {}", e), is_err: true }); }
                    }
                    let _ = tx.send(AppMsg::TaskFinished(tid));
                });
            }
        }
    }

    fn hub_delete_kernel_current(&mut self) {
        if let Some(d) = self.hub_detail.clone() {
            if d.category == "my_kernels" {
                let now = Instant::now();
                if let Some((ref armed, ts)) = self.hub_delete_arm {
                    if armed == &d.item_ref && now.duration_since(ts) <= Duration::from_secs(4) {
                        self.hub_delete_arm = None;
                        let bridge = self.bridge.clone();
                        let tx = self.tx.clone();
                        let kref = d.item_ref;
                        tokio::spawn(async move {
                            match bridge.call("hub_kernel_delete", json!({ "kernel_ref": kref })).await {
                                Ok(_) => { let _ = tx.send(AppMsg::HubDeleteDone(kref)); }
                                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Delete kernel failed: {}", e), is_err: true }); }
                            }
                        });
                        return;
                    }
                }
                self.hub_delete_arm = Some((d.item_ref, now));
                self.notify("Press Shift+D again within 4s to DELETE kernel", false);
            }
        }
    }

    pub fn trigger_submit_local_csv(&mut self) {
        let local = match self.active_pane() {
            PaneKind::LocalFiles(lp) => lp.entries.get(lp.cursor).cloned(),
            _ => None,
        };
        let Some(ent) = local else {
            self.notify("Highlight a local .csv file first.", true);
            return;
        };
        let p = ent.path.to_string_lossy().to_string();
        if ent.is_dir || !p.to_ascii_lowercase().ends_with(".csv") {
            self.notify("Selected local entry must be a .csv file.", true);
            return;
        }
        let comp = self.default_competition();
        self.modal = Some(Modal::SubmitCompetition(SubmitCompModalState {
            path: p,
            is_remote: false,
            competition: TextField::new(comp),
            message: TextField::new("Submitted via kgsh"),
            field_idx: 0,
        }));
    }

    pub fn trigger_kill_gpu_proc(&mut self) {
        let Some(gp) = self.gpu_procs.get(self.selected_gpu_proc_idx).cloned() else {
            self.notify("No GPU process selected.", true);
            return;
        };
        let now = Instant::now();
        if let Some((armed_pid, ts)) = self.kill_gpu_arm {
            if armed_pid == gp.pid && now.duration_since(ts) <= Duration::from_secs(3) {
                self.kill_gpu_arm = None;
                let Some(sess) = self.active_session() else { return; };
                let http = self.http.clone();
                let tx = self.tx.clone();
                let pid = gp.pid;
                tokio::spawn(async move {
                    match http.proc_kill(&sess, pid, "SIGTERM").await {
                        Ok(_) => { let _ = tx.send(AppMsg::FsOpDone { msg: format!("Sent SIGTERM to GPU PID {}", pid), refresh: false }); }
                        Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Kill PID failed: {}", e), is_err: true }); }
                    }
                });
                return;
            }
        }
        self.kill_gpu_arm = Some((gp.pid, now));
        self.notify(format!("Press Shift+K again within 3s to kill PID {}", gp.pid), false);
    }

    pub fn trigger_batch_archive(&mut self) {
        let paths: Vec<String> = match self.active_pane() {
            PaneKind::RemoteFiles(rp) => rp.selected.iter().cloned().collect(),
            _ => Vec::new(),
        };
        if paths.is_empty() {
            self.notify("Press Space on remote files/dirs first to select.", true);
            return;
        }
        let Some(sess) = self.active_session() else { return; };
        let tid = self.begin_task(format!("Archive {} items", paths.len()));
        let http = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match http.fs_archive(&sess, &paths, "").await {
                Ok(v) => {
                    let out = v.get("path").and_then(|x| x.as_str()).unwrap_or("bundle.tar.gz");
                    let _ = tx.send(AppMsg::FsOpDone {
                        msg: format!("Archived {} items -> {}", paths.len(), out),
                        refresh: true,
                    });
                }
                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Archive failed: {}", e), is_err: true }); }
            }
            let _ = tx.send(AppMsg::TaskFinished(tid));
        });
    }

    pub fn trigger_delete_selected(&mut self) {
        let side = self.active_side;
        let now = Instant::now();

        if let PaneKind::LocalFiles(_) = self.pane(side) {
            let (sel_paths, cur_entry) = match self.pane(side) {
                PaneKind::LocalFiles(lp) => (
                    lp.selected.iter().cloned().collect::<Vec<_>>(),
                    lp.entries.get(lp.cursor).filter(|e| e.name != "..").cloned(),
                ),
                _ => return,
            };
            if !sel_paths.is_empty() {
                if let Some((ref tag, ts)) = self.delete_arm {
                    if tag == "__local_batch__" && now.duration_since(ts) <= Duration::from_secs(4) {
                        self.delete_arm = None;
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.selected.clear();
                        }
                        let mut cnt = 0usize;
                        for p in &sel_paths {
                            let ok = if p.is_dir() { std::fs::remove_dir_all(p).is_ok() } else { std::fs::remove_file(p).is_ok() };
                            if ok { cnt += 1; }
                        }
                        self.reload_all_local_panes();
                        self.notify(format!("Deleted {} local item(s)", cnt), cnt == 0);
                        return;
                    }
                }
                self.delete_arm = Some(("__local_batch__".into(), now));
                self.notify(format!("Press d again within 4s to remove {} local item(s)", sel_paths.len()), false);
                return;
            }
            let Some(ent) = cur_entry else { return; };
            let key_path = format!("local:{}", ent.path.display());
            if let Some((ref armed, ts)) = self.delete_arm {
                if armed == &key_path && now.duration_since(ts) <= Duration::from_secs(4) {
                    self.delete_arm = None;
                    let res = if ent.is_dir { std::fs::remove_dir_all(&ent.path) } else { std::fs::remove_file(&ent.path) };
                    match res {
                        Ok(_) => {
                            self.reload_all_local_panes();
                            self.notify(format!("Deleted local {}", ent.name), false);
                        }
                        Err(e) => self.notify(format!("Delete failed: {}", e), true),
                    }
                    return;
                }
            }
            self.delete_arm = Some((key_path, now));
            self.notify(format!("Press d again within 4s to remove local {}", ent.name), false);
            return;
        }

        let (sel_paths, cur_entry) = match self.pane(side) {
            PaneKind::RemoteFiles(rp) => (
                rp.selected.iter().cloned().collect::<Vec<_>>(),
                rp.entries.get(rp.cursor).filter(|e| e.name != "..").cloned(),
            ),
            _ => return,
        };
        if !sel_paths.is_empty() {
            if let Some((ref tag, ts)) = self.delete_arm {
                if tag == "__batch__" && now.duration_since(ts) <= Duration::from_secs(4) {
                    self.delete_arm = None;
                    if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                        rp.selected.clear();
                    }
                    let Some(sess) = self.active_session() else { return; };
                    let tid = self.begin_task(format!("Delete {} items", sel_paths.len()));
                    let http = self.http.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        let mut cnt = 0;
                        for p in &sel_paths {
                            if http.fs_delete(&sess, p).await.is_ok() {
                                cnt += 1;
                            }
                        }
                        let _ = tx.send(AppMsg::FsOpDone { msg: format!("Deleted {} item(s)", cnt), refresh: true });
                        let _ = tx.send(AppMsg::TaskFinished(tid));
                    });
                    return;
                }
            }
            self.delete_arm = Some(("__batch__".into(), now));
            self.notify(format!("Press d again within 4s to remove {} selected item(s)", sel_paths.len()), false);
            return;
        }

        let Some(ent) = cur_entry else { return; };
        if ent.path.starts_with("/kaggle/input") {
            self.notify("/kaggle/input is read-only.", true);
            return;
        }
        if let Some((ref armed, ts)) = self.delete_arm {
            if armed == &ent.path && now.duration_since(ts) <= Duration::from_secs(4) {
                self.delete_arm = None;
                let Some(sess) = self.active_session() else { return; };
                let tid = self.begin_task(format!("Delete {}", ent.name));
                let http = self.http.clone();
                let tx = self.tx.clone();
                let p = ent.path;
                tokio::spawn(async move {
                    match http.fs_delete(&sess, &p).await {
                        Ok(_) => { let _ = tx.send(AppMsg::FsOpDone { msg: format!("Deleted {}", p), refresh: true }); }
                        Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Delete failed: {}", e), is_err: true }); }
                    }
                    let _ = tx.send(AppMsg::TaskFinished(tid));
                });
                return;
            }
        }
        self.delete_arm = Some((ent.path, now));
        self.notify(format!("Press d again within 4s to remove {}", ent.name), false);
    }

    pub fn execute_context_action(&mut self, id: &str) {
        let side = self.active_side;
        match id {
            "run_file" => self.spawn_run_remote_file(side),
            "transfer" => self.transfer_to_opposite(),
            "preview" => self.toggle_or_load_preview_drawer(false),
            "open" => match self.pane(side).clone() {
                PaneKind::RemoteFiles(rp) => {
                    if let Some(e) = rp.entries.get(rp.cursor) {
                        if e.name == ".." {
                            self.navigate_remote_up(side);
                        } else if e.is_dir {
                            self.navigate_remote_into(side, e.path.clone(), Some(e.name.clone()));
                        } else {
                            self.spawn_open_remote_file(e.path.clone());
                        }
                    }
                }
                PaneKind::LocalFiles(lp) => {
                    if let Some(e) = lp.entries.get(lp.cursor) {
                        if e.name == ".." {
                            if let PaneKind::LocalFiles(ref mut m_lp) = self.pane_mut(side) {
                                m_lp.navigate_up();
                            }
                        } else if e.is_dir {
                            let p = e.path.clone();
                            if let PaneKind::LocalFiles(ref mut m_lp) = self.pane_mut(side) {
                                m_lp.navigate_into(p);
                            }
                        } else {
                            self.open_local_file_editor(e.path.clone());
                        }
                    }
                }
                _ => {}
            },
            "clip_copy" => self.yank_or_cut_files(side, ClipOp::Copy),
            "clip_cut" => self.yank_or_cut_files(side, ClipOp::Cut),
            "clip_paste" => self.paste_files(side),
            "new" => self.open_new_file_modal(),
            "mkdir" => self.open_mkdir_modal(side),
            "rename" => self.open_path_action(false),
            "duplicate" => self.open_path_action(true),
            "du" => {
                let dir = self.active_remote_dir_or_default();
                self.spawn_du(side, dir);
            }
            "sort" => match self.pane_mut(side) {
                PaneKind::RemoteFiles(ref mut rp) => {
                    rp.sort = rp.sort.next();
                    rp.rebuild_entries();
                }
                PaneKind::LocalFiles(ref mut lp) => {
                    lp.sort = lp.sort.next();
                    lp.reload();
                }
                _ => {}
            },
            "hidden" => match self.pane_mut(side) {
                PaneKind::RemoteFiles(ref mut rp) => {
                    rp.show_hidden = !rp.show_hidden;
                    rp.rebuild_entries();
                }
                PaneKind::LocalFiles(ref mut lp) => {
                    lp.show_hidden = !lp.show_hidden;
                    lp.reload();
                }
                _ => {}
            },
            "publish" => self.open_publish_dataset_modal(),
            "archive" => self.trigger_batch_archive(),
            "submit" => {
                let ent = match self.pane(side) {
                    PaneKind::RemoteFiles(rp) => rp.entries.get(rp.cursor).cloned(),
                    _ => None,
                };
                if let Some(e) = ent {
                    if e.path.to_ascii_lowercase().ends_with(".csv") {
                        let comp = self.default_competition();
                        self.modal = Some(Modal::SubmitCompetition(SubmitCompModalState {
                            path: e.path,
                            is_remote: true,
                            competition: TextField::new(comp),
                            message: TextField::new("Submitted via kgsh"),
                            field_idx: 0,
                        }));
                        return;
                    }
                }
                self.notify("Highlight a .csv file first.", true);
            }
            "sync" => self.open_sync_modal(),
            "copypath" => {
                let p = match self.pane(side) {
                    PaneKind::RemoteFiles(rp) => rp
                        .entries
                        .get(rp.cursor)
                        .filter(|e| e.name != "..")
                        .map(|e| e.path.clone())
                        .unwrap_or_else(|| rp.dir.clone()),
                    PaneKind::LocalFiles(lp) => lp
                        .entries
                        .get(lp.cursor)
                        .filter(|e| e.name != "..")
                        .map(|e| e.path.to_string_lossy().to_string())
                        .unwrap_or_else(|| lp.dir.to_string_lossy().to_string()),
                    _ => "/kaggle/working".into(),
                };
                copy_clipboard(&p);
                self.notify(format!("Copied {}", p), false);
            }
            "delete" => self.trigger_delete_selected(),
            "submit_local" => self.trigger_submit_local_csv(),
            "reload_local" => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.reload();
                }
            }
            "enter_input" => self.input_focus = InputFocus::ShellPrompt,
            "toggle_tel" => self.show_telemetry = !self.show_telemetry,
            "clear_feed" => self.activities.clear(),
            "flow" => self.conduit.trigger_tx("nvidia-smi", &self.current_slot, "MCP Plasma Pulse"),
            "workers" => self.open_workers_modal(0),
            "spawn_job" => {
                self.modal = Some(Modal::SpawnJob(SpawnJobModalState {
                    command: TextField::new(""),
                    gpu_ids: TextField::new(""),
                    name: TextField::new(""),
                    field_idx: 0,
                }));
            }
            "kill_job" => {
                if !self.selected_job_id.is_empty() {
                    self.spawn_kill_job(self.selected_job_id.clone());
                }
            }
            "kill_gpu" => self.trigger_kill_gpu_proc(),
            "pause_logs" => self.logs_paused = !self.logs_paused,
            "filter_logs" => self.input_focus = InputFocus::LogFilter,
            "copy_logs" => {
                let txt = self.filtered_log_lines().into_iter().cloned().collect::<Vec<_>>().join("\n");
                copy_clipboard(&txt);
                self.notify(format!("Copied {} log chars", txt.len()), false);
            }
            "hub_search" => self.input_focus = InputFocus::HubSearch,
            "hub_inspect" => {
                if let Some(item) = self.hub_items.get(self.hub_cursor).cloned() {
                    self.spawn_hub_inspect(item.category, item.item_ref);
                }
            }
            "hub_stage" => self.hub_stage_current(),
            "hub_dl" => self.hub_download_current(),
            "hub_logs" => self.hub_fetch_logs_current(),
            "hub_del" => self.hub_delete_kernel_current(),
            _ => {}
        }
    }

    pub(super) fn execute_palette_cmd(&mut self, cmd: &str) {
        match cmd {
            "cockpit" => self.apply_preset(Preset::Cockpit),
            "dual_files" => self.apply_preset(Preset::DualFiles),
            "jobs" => self.apply_preset(Preset::Jobs),
            "hub" => self.apply_preset(Preset::Hub),
            "workers" => self.open_workers_modal(0),
            "boot" => self.open_workers_modal(1),
            "swap" => self.modal = Some(Modal::SwapPane(0)),
            "zoom" => self.zoomed = !self.zoomed,
            "telemetry" => self.show_telemetry = !self.show_telemetry,
            "preview" => self.toggle_or_load_preview_drawer(false),
            "cpu" => self.run_command(format!(":up -a cpu -s {}", self.current_slot)),
            "t4" => self.run_command(format!(":up -a NvidiaTeslaT4 -s {}", self.current_slot)),
            "sync" => self.open_sync_modal(),
            "du" => {
                let d = self.active_remote_dir_or_default();
                let side = self.active_side;
                self.spawn_du(side, d);
            }
            "publish" => self.open_publish_dataset_modal(),
            "newfile" => self.open_new_file_modal(),
            "flow" => self.run_command(":flow".into()),
            "down" => self.run_command(":down".into()),
            "help" => self.modal = Some(Modal::Help),
            _ => {}
        }
    }

}
