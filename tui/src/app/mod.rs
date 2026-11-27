//! Universal Dual-Panel Workbench (`LEFT | RIGHT`) with Yazi/TermSCP-inspired File UX, `NAV`/`INPUT` modes & Context Actions.

use ratatui::layout::Rect;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::{HubDetail, HubItem, PythonBridge, QuotaSummary};
use crate::state::{
    load_all_sessions, load_profiles, mcp_flow_mtime_ns, normalize_slot, read_local_activities,
    read_mcp_flow_events, ActiveSession, ActivityEvent,
};
use crate::worker::{GpuProcess, InspectResult, JobInfo, WorkerHttpClient, WorkerStatus};

mod actions;
mod input;
mod mouse;
mod selection;
#[cfg(test)]
mod tests;
mod text;
mod types;

pub use text::{EditorModalState, TextField};
pub use types::*;

use regex::Regex;

pub struct App {
    pub state_dir: PathBuf,
    pub http: WorkerHttpClient,
    pub bridge: PythonBridge,
    pub tx: UnboundedSender<AppMsg>,
    pub should_quit: bool,

    // Dual-Panel core
    pub preset: Preset,
    pub left_pane: PaneKind,
    pub right_pane: PaneKind,
    pub active_side: Side,
    pub input_focus: InputFocus,
    pub zoomed: bool,
    pub show_telemetry: bool,
    pub preview_drawer: Option<PreviewDrawerState>,
    pub preview_pending: Option<(Side, String, Instant)>,
    pub file_clipboard: Option<FileClipboard>,
    pub active_tasks: Vec<(u64, String)>,
    pub next_task_id: u64,
    pub last_click: Option<(Side, usize, Instant)>,
    pub screen_buf: Option<ratatui::buffer::Buffer>,
    pub drag_anchor: Option<(u16, u16, Rect, SelectionKind)>,
    pub last_click_anchor: Option<(u16, u16, Rect, SelectionKind)>,
    pub selection: Option<TextSelection>,
    pub editor_mouse_selecting: bool,
    pub mouse_capture: bool,
    pub mouse_capture_changed: bool,
    pub running_exec: Option<RunningExec>,
    pub last_poll_at: Instant,

    // Telemetry & Session
    pub current_slot: String,
    pub sessions: BTreeMap<String, ActiveSession>,
    pub is_online: bool,
    pub rtt_ms: Option<f64>,
    pub status_line: String,
    pub worker_status: WorkerStatus,
    pub osc_history: VecDeque<f64>,
    pub conduit: ConduitState,
    pub quota: Option<QuotaSummary>,
    pub staged_datasets: Vec<String>,
    pub staged_models: Vec<String>,
    pub staged_competition: String,

    // Shell state
    pub prompt: TextField,
    pub cmd_history: Vec<String>,
    pub cmd_hist_idx: usize,
    pub cmd_draft: String,
    pub known_cwd: String,
    pub activities: VecDeque<ActivityEvent>,
    pub activity_scroll: usize,
    pub remote_seq_by_slot: BTreeMap<String, u64>,
    pub local_act_ts: f64,
    pub flow_since_ts: f64,
    pub last_flow_mtime_ns: u128,
    pub optimistic_dedup: VecDeque<(String, String)>,

    // Jobs & Logs state
    pub jobs: Vec<JobInfo>,
    pub selected_job_idx: usize,
    pub selected_job_id: String,
    pub log_offset: u64,
    pub raw_log_lines: VecDeque<String>,
    pub log_filter: TextField,
    pub logs_paused: bool,
    pub log_scroll: usize,
    pub gpu_procs: Vec<GpuProcess>,
    pub selected_gpu_proc_idx: usize,
    pub kill_gpu_arm: Option<(i64, Instant)>,

    // Hub state
    pub hub_cat_idx: usize,
    pub hub_search: TextField,
    pub hub_items: Vec<HubItem>,
    pub hub_cursor: usize,
    pub hub_detail: Option<HubDetail>,
    pub hub_detail_scroll: usize,
    pub hub_delete_arm: Option<(String, Instant)>,

    // Guards & Timers
    pub yank_buffer: String,
    #[cfg(not(test))]
    pub clipboard: Option<arboard::Clipboard>,
    pub poll_in_flight: HashSet<String>,
    pub delete_arm: Option<(String, Instant)>,
    pub poll_fail_count: u32,
    pub poll_backoff_until: Option<Instant>,
    pub frame_tick: u64,
    pub modal: Option<Modal>,
    pub toast: Option<Toast>,
    pub rects: UiRects,
}

impl App {
    pub fn new(
        state_dir: PathBuf,
        initial_slot: String,
        mut bridge: PythonBridge,
        tx: UnboundedSender<AppMsg>,
    ) -> Self {
        bridge.state_dir = state_dir.clone();
        let sessions = load_all_sessions(&state_dir);
        let mut app = Self {
            state_dir: state_dir.clone(),
            http: WorkerHttpClient::new(),
            bridge,
            tx,
            should_quit: false,
            preset: Preset::Cockpit,
            left_pane: PaneKind::Shell,
            right_pane: PaneKind::LiveLogs,
            active_side: Side::Left,
            input_focus: InputFocus::ShellPrompt,
            zoomed: false,
            show_telemetry: true,
            preview_drawer: None,
            preview_pending: None,
            file_clipboard: None,
            active_tasks: Vec::new(),
            next_task_id: 1,
            last_click: None,
            screen_buf: None,
            drag_anchor: None,
            last_click_anchor: None,
            selection: None,
            editor_mouse_selecting: false,
            mouse_capture: true,
            mouse_capture_changed: false,
            running_exec: None,
            last_poll_at: Instant::now() - Duration::from_secs(5),
            current_slot: normalize_slot(&initial_slot),
            sessions,
            is_online: false,
            rtt_ms: None,
            status_line: "Standby — Press F5 to Boot or Ctrl+W for Workers".into(),
            worker_status: WorkerStatus::default(),
            osc_history: VecDeque::from(vec![0.0; 60]),
            conduit: ConduitState::default(),
            quota: None,
            staged_datasets: Vec::new(),
            staged_models: Vec::new(),
            staged_competition: String::new(),
            prompt: TextField::new(""),
            cmd_history: Vec::new(),
            cmd_hist_idx: 0,
            cmd_draft: String::new(),
            known_cwd: "/kaggle/working".into(),
            activities: VecDeque::with_capacity(240),
            activity_scroll: 0,
            remote_seq_by_slot: BTreeMap::new(),
            local_act_ts: 0.0,
            flow_since_ts: current_unix_sec() - 4.0,
            last_flow_mtime_ns: 0,
            optimistic_dedup: VecDeque::with_capacity(40),
            jobs: Vec::new(),
            selected_job_idx: 0,
            selected_job_id: String::new(),
            log_offset: 0,
            raw_log_lines: VecDeque::with_capacity(1500),
            log_filter: TextField::new(""),
            logs_paused: false,
            log_scroll: 0,
            gpu_procs: Vec::new(),
            selected_gpu_proc_idx: 0,
            kill_gpu_arm: None,
            hub_cat_idx: 0,
            hub_search: TextField::new(""),
            hub_items: Vec::new(),
            hub_cursor: 0,
            hub_detail: None,
            hub_detail_scroll: 0,
            hub_delete_arm: None,
            yank_buffer: String::new(),
            #[cfg(not(test))]
            clipboard: None,
            poll_in_flight: HashSet::new(),
            delete_arm: None,
            poll_fail_count: 0,
            poll_backoff_until: None,
            frame_tick: 0,
            modal: None,
            toast: None,
            rects: UiRects::default(),
        };
        app.flush_local_activities();
        app
    }

    pub fn begin_task(&mut self, label: impl Into<String>) -> u64 {
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.wrapping_add(1).max(1);
        self.active_tasks.push((id, label.into()));
        id
    }

    pub fn end_task(&mut self, id: u64) {
        self.active_tasks.retain(|(tid, _)| *tid != id);
    }

    pub fn capture_screen_snapshot(&mut self, buf: &ratatui::buffer::Buffer) {
        self.screen_buf = Some(buf.clone());
    }

    pub fn toggle_mouse_capture(&mut self) {
        self.mouse_capture = !self.mouse_capture;
        self.mouse_capture_changed = true;
        self.drag_anchor = None;
        self.last_click_anchor = None;
        self.editor_mouse_selecting = false;
        if !self.mouse_capture {
            self.selection = None;
            self.notify("Native terminal select ON (F8 to restore TUI mouse)", false);
        } else {
            self.notify("TUI mouse & drag-select ON", false);
        }
    }

    pub fn has_live_work(&self) -> bool {
        self.running_exec.is_some() || self.jobs.iter().any(|j| j.status == "running")
    }

    pub fn should_poll_now(&self) -> bool {
        let interval = if self.has_live_work() {
            Duration::from_millis(500)
        } else {
            Duration::from_millis(1400)
        };
        self.last_poll_at.elapsed() >= interval
    }

    pub fn pane(&self, side: Side) -> &PaneKind {
        match side {
            Side::Left => &self.left_pane,
            Side::Right => &self.right_pane,
        }
    }

    pub fn pane_mut(&mut self, side: Side) -> &mut PaneKind {
        match side {
            Side::Left => &mut self.left_pane,
            Side::Right => &mut self.right_pane,
        }
    }

    pub fn active_pane(&self) -> &PaneKind {
        self.pane(self.active_side)
    }

    pub fn apply_preset(&mut self, preset: Preset) {
        self.preset = preset;
        self.zoomed = false;
        self.selection = None;
        self.last_click_anchor = None;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match preset {
            Preset::Cockpit => {
                self.left_pane = PaneKind::Shell;
                self.right_pane = PaneKind::LiveLogs;
                self.active_side = Side::Left;
                self.input_focus = InputFocus::ShellPrompt;
                self.show_telemetry = true;
            }
            Preset::DualFiles => {
                self.left_pane = PaneKind::LocalFiles(LocalPaneState::new(cwd));
                self.right_pane = PaneKind::RemoteFiles(RemotePaneState::new("/kaggle/working"));
                self.active_side = Side::Right;
                self.input_focus = InputFocus::None;
                self.show_telemetry = false;
                if self.active_session().is_some() {
                    self.spawn_fs_list(Side::Right, "/kaggle/working".into());
                }
            }
            Preset::Jobs => {
                self.left_pane = PaneKind::JobsAndGpu;
                self.right_pane = PaneKind::LiveLogs;
                self.active_side = Side::Left;
                self.input_focus = InputFocus::None;
                self.show_telemetry = false;
            }
            Preset::Hub => {
                self.left_pane = PaneKind::HubList;
                self.right_pane = PaneKind::HubDetail;
                self.active_side = Side::Left;
                self.input_focus = InputFocus::None;
                self.show_telemetry = false;
                if self.hub_items.is_empty() {
                    self.spawn_hub_search();
                }
            }
        }
    }

    pub fn assign_pane(&mut self, side: Side, option_id: &str) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let new_pane = match option_id {
            "shell" => {
                if self.active_side == side {
                    self.input_focus = InputFocus::ShellPrompt;
                }
                PaneKind::Shell
            }
            "local" => PaneKind::LocalFiles(LocalPaneState::new(cwd)),
            "remote_tmp" => PaneKind::RemoteFiles(RemotePaneState::new("/tmp")),
            "remote_input" => PaneKind::RemoteFiles(RemotePaneState::new("/kaggle/input")),
            "remote_working" => PaneKind::RemoteFiles(RemotePaneState::new("/kaggle/working")),
            "jobs" => PaneKind::JobsAndGpu,
            "logs" => PaneKind::LiveLogs,
            "hub_list" => {
                if self.hub_items.is_empty() {
                    self.spawn_hub_search();
                }
                PaneKind::HubList
            }
            "hub_detail" => PaneKind::HubDetail,
            _ => return,
        };
        let remote_to_fetch = match &new_pane {
            PaneKind::RemoteFiles(rp) => Some(rp.dir.clone()),
            _ => None,
        };
        *self.pane_mut(side) = new_pane;
        if let Some(dir) = remote_to_fetch {
            if self.active_session().is_some() {
                self.spawn_fs_list(side, dir);
            }
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, is_err: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            is_err,
            expires_at: Instant::now() + Duration::from_secs(if is_err { 5 } else { 3 }),
        });
    }

    pub fn active_session(&self) -> Option<ActiveSession> {
        self.sessions.get(&self.current_slot).cloned()
    }

    pub fn flush_local_activities(&mut self) {
        let items = read_local_activities(&self.state_dir, &self.current_slot, self.local_act_ts);
        for ev in items {
            if ev.ts > self.local_act_ts {
                self.local_act_ts = ev.ts;
            }
            self.push_activity(ev);
        }
    }

    pub fn push_activity(&mut self, ev: ActivityEvent) {
        let sig = (ev.caller.to_ascii_lowercase(), ev.summary.clone());
        if let Some(pos) = self.optimistic_dedup.iter().position(|x| x == &sig) {
            self.optimistic_dedup.remove(pos);
            return;
        }
        if self.activities.len() >= 220 {
            self.activities.pop_front();
        }
        self.activities.push_back(ev);
    }

    pub fn filtered_log_lines(&self) -> Vec<&String> {
        let q = self.log_filter.value.trim();
        if q.is_empty() {
            return self.raw_log_lines.iter().collect();
        }
        let pat = Regex::new(&format!("(?i){}", q)).ok();
        self.raw_log_lines
            .iter()
            .filter(|ln| match &pat {
                Some(rx) => rx.is_match(ln),
                None => ln.to_ascii_lowercase().contains(&q.to_ascii_lowercase()),
            })
            .collect()
    }

    pub fn active_remote_dir_or_default(&self) -> String {
        if let PaneKind::RemoteFiles(ref r) = self.active_pane() {
            return r.dir.clone();
        }
        if let PaneKind::RemoteFiles(ref r) = self.pane(self.active_side.opposite()) {
            return r.dir.clone();
        }
        "/kaggle/working".into()
    }

    pub fn refresh_all_remote_panes(&mut self) {
        for side in [Side::Left, Side::Right] {
            if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                rp.find_active = false;
                if rp.pending_select_name.is_none() {
                    if let Some(cur) = rp.entries.get(rp.cursor) {
                        if cur.name != ".." {
                            rp.pending_select_name = Some(cur.name.clone());
                        }
                    }
                }
                let dir = rp.dir.clone();
                self.spawn_fs_list(side, dir);
            }
        }
    }

    pub fn reload_all_local_panes(&mut self) {
        for side in [Side::Left, Side::Right] {
            if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                lp.reload();
            }
        }
    }

    pub fn action_menu_items(&self) -> Vec<(&'static str, &'static str)> {
        match self.active_pane() {
            PaneKind::RemoteFiles(_) => vec![
                ("run_file", "X · Run Script with Live Logs (.py/.sh)"),
                ("transfer", "t · Transfer to Opposite Panel (Pull/Copy)"),
                ("preview", "v · Toggle Live Preview Drawer"),
                ("open", "Enter · Open Full Editor / Smart Inspect"),
                ("clip_copy", "c · Copy Selected to Clipboard"),
                ("clip_cut", "x · Cut Selected to Clipboard"),
                ("clip_paste", "p · Paste Clipboard Here"),
                ("new", "n · New Remote File / Paste Text"),
                ("mkdir", "N · Create New Directory"),
                ("rename", "R · Rename / Move Path"),
                ("duplicate", "Duplicate Copy"),
                ("du", "u · Analyze Disk Usage (DU)"),
                ("sort", "s · Cycle Sort (Name / Size / Time)"),
                ("hidden", ". · Toggle Hidden Dotfiles"),
                ("publish", "Publish Directory as Kaggle Dataset"),
                ("archive", "Archive Space-Selected Items (.tar.gz)"),
                ("submit", "Submit .csv to Kaggle Competition"),
                ("sync", "Open Push / Pull Sync Modal"),
                ("copypath", "y · Copy Remote Path"),
                ("delete", "d · Delete Selected Item(s)"),
            ],
            PaneKind::LocalFiles(_) => vec![
                ("transfer", "t · Push Selected to Opposite Panel"),
                ("preview", "v · Preview Local File in Drawer"),
                ("open", "Enter · Open Local File Editor / Enter Dir"),
                ("clip_copy", "c · Copy Selected to Clipboard"),
                ("clip_cut", "x · Cut Selected to Clipboard"),
                ("clip_paste", "p · Paste Clipboard Here"),
                ("mkdir", "N · Create Local Directory"),
                ("rename", "R · Rename / Move Local Item"),
                ("duplicate", "Duplicate Local Item"),
                ("sort", "s · Cycle Sort (Name / Size / Time)"),
                ("hidden", ". · Toggle Hidden Dotfiles"),
                ("submit_local", "Submit local .csv to Kaggle Competition"),
                ("sync", "Open Push / Pull Sync Modal"),
                ("copypath", "y · Copy Local Path"),
                ("delete", "d · Delete Selected Local Item(s)"),
                ("reload_local", "r · Reload Local Directory"),
            ],
            PaneKind::Shell => vec![
                ("enter_input", "i · Enter INPUT Mode to type command"),
                ("toggle_tel", "T · Toggle Telemetry & Oscilloscope Strip"),
                ("clear_feed", "Clear MCP & Shell Stream"),
                ("flow", "Trigger MCP Plasma Pulse"),
                ("workers", "Ctrl+W · Workers, Cluster & Quota"),
            ],
            PaneKind::JobsAndGpu => vec![
                ("spawn_job", "n · Spawn Background Job"),
                ("kill_job", "x · Kill Selected Job (SIGTERM)"),
                ("kill_gpu", "Shift+K · Kill Selected GPU PID"),
            ],
            PaneKind::LiveLogs => vec![
                ("pause_logs", "Space · Pause / Resume Log Scroll"),
                ("filter_logs", "/ · Filter Logs (Regex)"),
                ("copy_logs", "y · Copy Visible Filtered Logs"),
            ],
            PaneKind::HubList | PaneKind::HubDetail => vec![
                ("hub_search", "/ · Search Kaggle Hub"),
                ("hub_inspect", "Enter · Inspect Selected Item"),
                ("hub_stage", "s · Stage Item for Next Boot (F5)"),
                ("hub_dl", "d · Download Item Locally"),
                ("hub_logs", "l · Fetch Kernel Logs"),
                ("hub_del", "Shift+D · Delete Kernel"),
            ],
        }
    }

    // ── Background Task Spawners ──────────────────────────────────────────────

    // ── Handle Async Messages ─────────────────────────────────────────────────

    pub fn handle_msg(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::TaskFinished(tid) => {
                self.end_task(tid);
            }
            AppMsg::MonitorOk { slot, payload, rtt_ms } => {
                self.poll_in_flight.remove(&slot);
                if slot != self.current_slot {
                    return;
                }
                self.is_online = true;
                self.rtt_ms = Some(rtt_ms);
                self.poll_fail_count = 0;
                self.poll_backoff_until = None;
                self.status_line = format!("ONLINE · Slot {} · {:.0}ms", slot, rtt_ms);
                self.worker_status = payload.status;
                self.gpu_procs = self.worker_status.gpu_processes.clone();
                if self.selected_gpu_proc_idx >= self.gpu_procs.len() {
                    self.selected_gpu_proc_idx = self.gpu_procs.len().saturating_sub(1);
                }
                let load = self
                    .worker_status
                    .gpus
                    .iter()
                    .map(|g| g.util_pct)
                    .fold(self.worker_status.cpu_pct, f64::max);
                if self.osc_history.len() >= 60 {
                    self.osc_history.pop_front();
                }
                self.osc_history.push_back(load);

                let mut jobs = payload.jobs;
                jobs.sort_by(|a, b| b.started_at.partial_cmp(&a.started_at).unwrap_or(std::cmp::Ordering::Equal));
                self.jobs = jobs;
                if self.selected_job_id.is_empty() {
                    if let Some(first) = self.jobs.first() {
                        self.selected_job_id = first.job_id.clone();
                        self.log_offset = 0;
                    }
                }
                if self.selected_job_idx >= self.jobs.len() {
                    self.selected_job_idx = self.jobs.len().saturating_sub(1);
                }

                let mut cur_seq = *self.remote_seq_by_slot.get(&slot).unwrap_or(&0);
                for ev in payload.events {
                    if let Some(s) = ev.seq {
                        if s <= cur_seq { continue; }
                        cur_seq = s;
                    }
                    self.push_activity(ev);
                }
                cur_seq = cur_seq.max(payload.latest_seq);
                self.remote_seq_by_slot.insert(slot, cur_seq);

                if let Some(lc) = payload.log_chunk {
                    if lc.replace {
                        self.raw_log_lines.clear();
                        self.log_offset = lc.next_offset;
                    } else if lc.next_offset > self.log_offset {
                        self.log_offset = lc.next_offset;
                    }
                    for ln in lc.chunk.lines() {
                        if self.raw_log_lines.len() >= 1500 {
                            self.raw_log_lines.pop_front();
                        }
                        self.raw_log_lines.push_back(ln.to_string());
                    }
                    if !self.logs_paused {
                        self.log_scroll = usize::MAX;
                    }
                }

                let fallback_live = if payload.pty_sessions_live.is_empty() {
                    payload.pty_live
                } else {
                    None
                };
                let tui_live = payload.pty_sessions_live.get("tui").cloned().or(fallback_live);
                if let Some(pl) = tui_live {
                    if pl.running {
                        let started = self
                            .running_exec
                            .as_ref()
                            .filter(|r| r.command == pl.command)
                            .map(|r| r.started_at)
                            .unwrap_or_else(|| {
                                Instant::now() - Duration::from_secs_f64(pl.elapsed_sec.max(0.0))
                            });
                        self.running_exec = Some(RunningExec {
                            command: pl.command,
                            started_at: started,
                            live_output: pl.output,
                            worker_has_live: true,
                        });
                    } else if self.running_exec.as_ref().map(|r| r.worker_has_live).unwrap_or(false)
                        && !self.active_tasks.iter().any(|(_, l)| l.starts_with("$ "))
                    {
                        self.running_exec = None;
                    }
                } else if self.running_exec.as_ref().map(|r| r.worker_has_live).unwrap_or(false)
                    && !self.active_tasks.iter().any(|(_, l)| l.starts_with("$ "))
                {
                    self.running_exec = None;
                }
            }
            AppMsg::MonitorErr { slot, tunnel_down, message } => {
                self.poll_in_flight.remove(&slot);
                if slot != self.current_slot {
                    return;
                }
                self.is_online = false;
                self.rtt_ms = None;
                self.poll_fail_count += 1;
                let secs = (self.poll_fail_count * 2).min(10) as u64;
                self.poll_backoff_until = Some(Instant::now() + Duration::from_secs(secs));
                self.status_line = format!("Connecting: {}", message);
                if tunnel_down && self.poll_fail_count == 1 {
                    self.spawn_refresh_endpoint(slot);
                }
            }
            AppMsg::EndpointRefreshed { slot, res } => {
                self.sessions = load_all_sessions(&self.state_dir);
                if slot == self.current_slot && res.stopped {
                    self.is_online = false;
                    self.status_line = res.message.clone();
                    self.notify(res.message, true);
                } else if res.updated {
                    self.poll_backoff_until = None;
                    self.poll_tick();
                }
            }
            AppMsg::BatchLogsDone { slot, status, logs } => {
                self.poll_in_flight.remove(&slot);
                if slot != self.current_slot {
                    return;
                }
                self.status_line = format!("Batch · {}", status);
                self.raw_log_lines.clear();
                for ln in logs.lines() {
                    self.raw_log_lines.push_back(ln.to_string());
                }
            }
            AppMsg::QuotaOk(q) => {
                self.quota = Some(q);
            }
            AppMsg::QuotaErr(e) => {
                self.notify(format!("Quota warning: {}", e), true);
            }
            AppMsg::BootDone(msg) => {
                self.sessions = load_all_sessions(&self.state_dir);
                self.poll_fail_count = 0;
                self.poll_backoff_until = None;
                self.conduit.trigger_rx("kaggle_up", &self.current_slot, "READY", 0.0);
                self.notify(msg, false);
                self.flush_local_activities();
                self.poll_tick();
            }
            AppMsg::ExecDone { slot, cmd, res } => {
                self.running_exec = None;
                self.known_cwd = if res.cwd.is_empty() { self.known_cwd.clone() } else { res.cwd.clone() };
                self.conduit.trigger_rx("term_exec", &slot, &cmd, res.duration_sec);
                let summary = format!("$ {}", cmd);
                self.optimistic_dedup.push_back(("tui".into(), summary.clone()));
                self.push_activity(ActivityEvent {
                    ts: current_unix_sec(),
                    slot,
                    caller: "tui".into(),
                    kind: "term_exec".into(),
                    summary,
                    detail: res.output,
                    exit_code: Some(res.exit_code),
                    duration_sec: Some(res.duration_sec),
                    seq: None,
                });
                self.poll_tick();
                self.refresh_all_remote_panes();
            }
            AppMsg::ExecErr { slot, cmd, err } => {
                self.running_exec = None;
                self.conduit.trigger_err("command", &slot, &err);
                self.notify(format!("{}: {}", cmd, err), true);
            }
            AppMsg::JobsActionDone { msg, selected_job_id } => {
                if let Some(jid) = selected_job_id {
                    self.selected_job_id = jid;
                    self.log_offset = 0;
                    self.raw_log_lines.clear();
                }
                self.notify(msg, false);
                self.poll_tick();
            }
            AppMsg::FsListDone { side, dir, entries } => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    rp.loading = false;
                    rp.dir = dir;
                    rp.raw_entries = entries;
                    rp.scroll = 0;
                    rp.rebuild_entries();
                }
                self.schedule_auto_preview(side);
            }
            AppMsg::PreviewDrawerLoaded(drawer) => {
                self.preview_drawer = Some(drawer);
            }
            AppMsg::FsReadDone {
                path,
                content,
                truncated,
                is_binary,
                size_bytes,
            } => {
                let (readonly, readonly_reason) = if path.starts_with("/kaggle/input") {
                    (true, "Read-Only (/kaggle/input)".to_string())
                } else if is_binary {
                    (true, format!("Read-Only (binary · {})", fmt_bytes(size_bytes)))
                } else if truncated {
                    (
                        true,
                        format!(
                            "Read-Only (truncated > 256 KB of {} — pull locally to edit)",
                            fmt_bytes(size_bytes)
                        ),
                    )
                } else {
                    (false, String::new())
                };
                let lines: Vec<String> = if content.is_empty() {
                    vec![String::new()]
                } else {
                    content.lines().map(|s| s.to_string()).collect()
                };
                self.modal = Some(Modal::FileEditor(EditorModalState {
                    is_local: false,
                    is_new: false,
                    path: TextField::new(path),
                    lines,
                    readonly,
                    readonly_reason,
                    ..Default::default()
                }));
            }
            AppMsg::FsSaveDone { path, ok, err } => {
                if ok {
                    if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                        if m.path.value.trim() == path {
                            m.saving = false;
                            m.dirty = false;
                            m.confirm_discard = false;
                            m.is_new = false;
                        }
                    }
                    self.notify(format!("Saved {}", path), false);
                    self.refresh_all_remote_panes();
                } else {
                    if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                        m.saving = false;
                    }
                    self.notify(format!("Save failed: {}", err), true);
                }
            }
            AppMsg::FsInspectDone(info) => {
                self.modal = Some(Modal::SmartInspect(SmartInspectModalState { info, scroll: 0 }));
            }
            AppMsg::FsDuDone { side, dir, res } => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    for ch in &res.children {
                        rp.du_pct_by_path.insert(ch.path.clone(), (ch.pct, ch.size_bytes));
                    }
                    rp.du_warn =
                        dir.starts_with("/kaggle/working") && res.total_bytes > (15.6 * 1073741824.0) as u64;
                    let top: Vec<String> = res
                        .children
                        .iter()
                        .take(3)
                        .map(|c| format!("{} ({:.0}%)", c.name, c.pct))
                        .collect();
                    rp.du_banner = format!(
                        "DU {}: {}{} · {}",
                        dir,
                        fmt_bytes(res.total_bytes),
                        if res.partial { " (partial)" } else { "" },
                        top.join(", ")
                    );
                }
            }
            AppMsg::FsFindDone { side, query, res } => {
                let empty = res.matches.is_empty();
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    rp.find_active = true;
                    rp.find_matches = res.matches;
                    rp.find_cursor = 0;
                }
                if empty {
                    self.notify(format!("No matches for '{}'", query), false);
                }
            }
            AppMsg::FsOpDone { msg, refresh } => {
                self.notify(msg, false);
                if refresh {
                    self.refresh_all_remote_panes();
                }
            }
            AppMsg::FsPublishDone { dataset_ref, url, stage_after } => {
                if stage_after && !dataset_ref.is_empty() && !self.staged_datasets.contains(&dataset_ref) {
                    self.staged_datasets.push(dataset_ref.clone());
                }
                self.notify(format!("Published {} ({})", dataset_ref, url), false);
            }
            AppMsg::SyncDone { msg, refresh_remote, refresh_local } => {
                self.conduit.trigger_rx("fs_sync", &self.current_slot, "done", 0.0);
                self.notify(msg, false);
                if refresh_remote {
                    self.refresh_all_remote_panes();
                }
                if refresh_local {
                    self.reload_all_local_panes();
                }
            }
            AppMsg::HubSearchDone(items) => {
                self.hub_items = items;
                self.hub_cursor = 0;
            }
            AppMsg::HubInspectDone(detail) => {
                self.hub_detail = Some(detail);
                self.hub_detail_scroll = 0;
            }
            AppMsg::HubLogsDone(txt) => {
                if let Some(ref mut d) = self.hub_detail {
                    d.text.push_str("\n\n── Last Kernel Logs ──\n");
                    d.text.push_str(&txt);
                }
            }
            AppMsg::HubDeleteDone(item_ref) => {
                self.hub_detail = None;
                self.notify(format!("Deleted kernel {}", item_ref), false);
                self.spawn_hub_search();
            }
            AppMsg::SubmitCsvDone { competition, result } => {
                self.notify(format!("Submitted to {}: {}", competition, result), false);
                self.apply_preset(Preset::Hub);
                self.spawn_hub_inspect("competitions".into(), competition);
            }
            AppMsg::ClusterRows(rows) => {
                if let Some(Modal::Workers(ref mut w)) = self.modal {
                    w.cluster_rows = rows;
                    w.cluster_loading = false;
                    if w.cluster_selected >= w.cluster_rows.len() {
                        w.cluster_selected = w.cluster_rows.len().saturating_sub(1);
                    }
                }
            }
            AppMsg::Toast { text, is_err } => {
                self.notify(text, is_err);
            }
        }
    }

    pub fn poll_local_flow_bus(&mut self) {
        let mtime = mcp_flow_mtime_ns(&self.state_dir);
        if mtime != 0 && mtime != self.last_flow_mtime_ns {
            self.last_flow_mtime_ns = mtime;
            let evs = read_mcp_flow_events(&self.state_dir, self.flow_since_ts);
            for ev in evs {
                if ev.ts > self.flow_since_ts {
                    self.flow_since_ts = ev.ts;
                }
                if ev.phase == "start" {
                    self.conduit.trigger_tx(&ev.tool, &ev.slot, &ev.summary);
                } else if ev.phase == "done" {
                    self.conduit
                        .trigger_rx(&ev.tool, &ev.slot, &ev.summary, ev.duration_sec.unwrap_or(0.0));
                } else if ev.phase == "error" {
                    self.conduit.trigger_err(&ev.tool, &ev.slot, &ev.error);
                }
            }
            self.flush_local_activities();
        }
    }

    // ── Modals Openers ────────────────────────────────────────────────────────

    pub fn open_workers_modal(&mut self, tab: usize) {
        let profiles = load_profiles(&self.state_dir);
        self.modal = Some(Modal::Workers(WorkersModalState {
            tab,
            cluster_rows: Vec::new(),
            cluster_selected: 0,
            cluster_loading: true,
            profiles,
            profile_idx: 0,
            profile_name: TextField::new(""),
            acc_idx: 0,
            slots: TextField::new(self.current_slot.clone()),
            datasets: TextField::new(self.staged_datasets.join(", ")),
            models: TextField::new(self.staged_models.join(", ")),
            competition: TextField::new(self.staged_competition.clone()),
            idle_min: TextField::new("0"),
            chain_prev: false,
            field_idx: 0,
        }));
        self.spawn_cluster_probe();
        self.spawn_quota();
    }

    pub fn open_sync_modal(&mut self) {
        let remote = self.active_remote_dir_or_default();
        self.modal = Some(Modal::Sync(SyncModalState {
            is_push: true,
            local_path: TextField::new("."),
            remote_path: TextField::new(remote),
            field_idx: 0,
        }));
    }

    pub fn open_new_file_modal(&mut self) {
        if let PaneKind::LocalFiles(ref lp) = self.active_pane() {
            let new_path = lp.dir.join("untitled.py").to_string_lossy().to_string();
            self.modal = Some(Modal::FileEditor(EditorModalState {
                is_local: true,
                is_new: true,
                path: TextField::new(new_path),
                lines: vec![String::new()],
                focus_path: true,
                ..Default::default()
            }));
            return;
        }
        let rdir = self.active_remote_dir_or_default();
        let base_dir = if rdir.starts_with("/kaggle/input") {
            "/kaggle/working"
        } else {
            &rdir
        };
        self.modal = Some(Modal::FileEditor(EditorModalState {
            is_local: false,
            is_new: true,
            path: TextField::new(format!("{}/untitled.py", base_dir.trim_end_matches('/'))),
            lines: vec![String::new()],
            focus_path: true,
            ..Default::default()
        }));
    }

    pub fn open_mkdir_modal(&mut self, side: Side) {
        match self.pane(side) {
            PaneKind::LocalFiles(lp) => {
                let dst = lp.dir.join("new_folder").to_string_lossy().to_string();
                self.modal = Some(Modal::PathAction(PathActionModalState {
                    is_local: true,
                    kind: PathActionKind::Mkdir,
                    src_path: lp.dir.to_string_lossy().to_string(),
                    dst_path: TextField::new(dst),
                    readonly_src: false,
                }));
            }
            PaneKind::RemoteFiles(rp) => {
                let base = if rp.dir.starts_with("/kaggle/input") {
                    "/kaggle/working"
                } else {
                    rp.dir.trim_end_matches('/')
                };
                self.modal = Some(Modal::PathAction(PathActionModalState {
                    is_local: false,
                    kind: PathActionKind::Mkdir,
                    src_path: rp.dir.clone(),
                    dst_path: TextField::new(format!("{}/new_folder", base)),
                    readonly_src: false,
                }));
            }
            _ => self.notify("Mkdir works inside Local or Remote Files panels.", true),
        }
    }

    pub fn open_path_action(&mut self, is_duplicate: bool) {
        match self.active_pane().clone() {
            PaneKind::LocalFiles(lp) => {
                let Some(ent) = lp.entries.get(lp.cursor).filter(|e| e.name != "..") else {
                    self.notify("Highlight a local file/folder first.", true);
                    return;
                };
                let src_str = ent.path.to_string_lossy().to_string();
                let dst = if is_duplicate {
                    if let Some((stem, ext)) = src_str.rsplit_once('.') {
                        format!("{}_copy.{}", stem, ext)
                    } else {
                        format!("{}_copy", src_str)
                    }
                } else {
                    src_str.clone()
                };
                self.modal = Some(Modal::PathAction(PathActionModalState {
                    is_local: true,
                    kind: if is_duplicate { PathActionKind::Copy } else { PathActionKind::Rename },
                    src_path: src_str,
                    dst_path: TextField::new(dst),
                    readonly_src: false,
                }));
            }
            PaneKind::RemoteFiles(rp) => {
                let Some(ent) = rp.entries.get(rp.cursor).filter(|e| e.name != "..") else {
                    self.notify("Highlight a remote file/folder first.", true);
                    return;
                };
                let ro = ent.path.starts_with("/kaggle/input");
                let dst = if ro {
                    format!("/kaggle/working/{}", ent.name)
                } else if is_duplicate {
                    if let Some((stem, ext)) = ent.path.rsplit_once('.') {
                        format!("{}_copy.{}", stem, ext)
                    } else {
                        format!("{}_copy", ent.path.trim_end_matches('/'))
                    }
                } else {
                    ent.path.clone()
                };
                self.modal = Some(Modal::PathAction(PathActionModalState {
                    is_local: false,
                    kind: if is_duplicate || ro { PathActionKind::Copy } else { PathActionKind::Rename },
                    src_path: ent.path.clone(),
                    dst_path: TextField::new(dst),
                    readonly_src: ro,
                }));
            }
            _ => self.notify("Highlight a file/folder first.", true),
        }
    }

    pub fn open_publish_dataset_modal(&mut self) {
        let dir = match self.active_pane() {
            PaneKind::RemoteFiles(rp) => rp
                .entries
                .get(rp.cursor)
                .filter(|e| e.name != ".." && e.is_dir && !e.path.starts_with("/kaggle/input"))
                .map(|e| e.path.clone())
                .unwrap_or_else(|| {
                    if rp.dir.starts_with("/kaggle/input") {
                        "/kaggle/working".into()
                    } else {
                        rp.dir.clone()
                    }
                }),
            _ => "/kaggle/working".into(),
        };
        let raw_name = Path::new(&dir).file_name().and_then(|s| s.to_str()).unwrap_or("kaggle-output");
        let slug: String = raw_name
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let clean = if slug.is_empty() { "kaggle-output".into() } else { slug };
        self.modal = Some(Modal::PublishDataset(PublishDatasetModalState {
            source_dir: TextField::new(dir),
            slug: TextField::new(clean.clone()),
            title: TextField::new(clean),
            notes: TextField::new("Checkpoint published via kgsh"),
            public: false,
            stage_after: true,
            field_idx: 0,
        }));
    }

    pub fn switch_slot(&mut self, slot: String) {
        self.current_slot = normalize_slot(&slot);
        self.selected_job_id.clear();
        self.log_offset = 0;
        self.raw_log_lines.clear();
        self.jobs.clear();
        self.gpu_procs.clear();
        self.poll_in_flight.clear();
        self.poll_fail_count = 0;
        self.poll_backoff_until = None;
        self.known_cwd = "/kaggle/working".into();
        for side in [Side::Left, Side::Right] {
            if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                rp.entries.clear();
                rp.raw_entries.clear();
            }
        }
        self.flush_local_activities();
        self.poll_tick();
    }

    pub fn default_competition(&self) -> String {
        if let Some(s) = self.active_session() {
            if !s.competition.trim().is_empty() {
                return s.competition.split(',').next().unwrap_or("").trim().to_string();
            }
        }
        self.staged_competition.clone()
    }

    pub fn set_clipboard_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.yank_buffer = text.to_string();
        let osc52 = build_osc52_sequence(text);
        #[cfg(test)]
        let _ = (osc52, std::any::TypeId::of::<arboard::Clipboard>());
        #[cfg(not(test))]
        {
            if let Some(seq) = osc52 {
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = out.write_all(seq.as_bytes());
                let _ = out.flush();
            }
            if self.clipboard.is_none() {
                self.clipboard = arboard::Clipboard::new().ok();
            }
            if let Some(ref mut cb) = self.clipboard {
                let _ = cb.set_text(text.to_string());
            }
        }
    }

    pub fn get_clipboard_text(&mut self) -> Option<String> {
        #[cfg(not(test))]
        {
            if self.clipboard.is_none() {
                self.clipboard = arboard::Clipboard::new().ok();
            }
            if let Some(ref mut cb) = self.clipboard {
                if let Ok(txt) = cb.get_text() {
                    if !txt.is_empty() {
                        return Some(txt);
                    }
                }
            }
        }
        if self.yank_buffer.is_empty() {
            None
        } else {
            Some(self.yank_buffer.clone())
        }
    }
}

pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

pub fn is_smart_ext(path: &str) -> bool {
    let lp = path.to_ascii_lowercase();
    lp.ends_with(".tar.gz")
        || [".csv", ".tsv", ".parquet", ".zip", ".tar", ".tgz", ".safetensors", ".pt", ".pth", ".bin"]
            .iter()
            .any(|ext| lp.ends_with(ext))
}

pub(super) fn format_inspect_lines(info: &InspectResult) -> Vec<String> {
    let mut lines = Vec::new();
    if info.kind == "table" {
        if !info.columns.is_empty() {
            lines.push(info.columns.join(","));
        }
        for r in info.rows.iter().take(30) {
            lines.push(r.join(","));
        }
    } else if info.kind == "archive" {
        for e in &info.entries {
            lines.push(format!(
                "{} {:>9} {}",
                if e.is_dir { "[DIR]" } else { "[FILE]" },
                fmt_bytes(e.size_bytes),
                e.name
            ));
        }
    } else if info.kind == "tensors" {
        for t in &info.tensors {
            lines.push(format!("{:<6} {:?}  {}", t.dtype, t.shape, t.name));
        }
    } else if !info.note.is_empty() {
        lines.push(info.note.clone());
    }
    if lines.is_empty() {
        lines.push("(No previewable rows/entries)".into());
    }
    lines
}

pub fn parent_remote_dir(dir: &str) -> Option<String> {
    let clean = dir.trim_end_matches('/');
    if matches!(clean, "" | "/kaggle" | "/kaggle/working" | "/kaggle/input" | "/tmp") {
        return None;
    }
    clean.rsplit_once('/').map(|(p, _)| {
        if p.is_empty() { "/tmp".into() } else { p.to_string() }
    })
}

pub(super) fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

pub fn filtered_palette_items(query: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    let all = vec![
        ("Preset F1: Cockpit (Shell | Logs)", "Shell + MCP stream + Slim telemetry", "cockpit"),
        ("Preset F2: Dual Files (Local | Remote)", "WinSCP + Yazi 2-column file workbench", "dual_files"),
        ("Preset F3: Jobs & GPU VRAM | Logs", "Jobs, Live Logs & nvidia-smi processes", "jobs"),
        ("Preset F4: Kaggle Hub | Inspector", "Datasets, Competitions, Models & Kernels", "hub"),
        ("Workers: Cluster & Quota (Ctrl+W)", "Switch/stop slots + weekly quota", "workers"),
        ("Workers: Boot New VM / Profiles (F5)", "Launch GPU/TPU/CPU + saved presets", "boot"),
        ("Swap Active Panel Content (m)", "Assign Shell, Local, Remote /tmp, Jobs, Hub...", "swap"),
        ("Toggle Panel Zoom 100% (z)", "Maximize active panel or restore Dual-Panel", "zoom"),
        ("Toggle Preview Drawer (v)", "Inspect selected file below Dual-Panel", "preview"),
        ("Toggle Telemetry Strip (Shift+T)", "Show/hide MCP conduit & oscilloscope", "telemetry"),
        ("Analyze Disk Usage (:du)", "Scan largest folders on Kaggle", "du"),
        ("Publish Remote Dir as Kaggle Dataset", "Push folder from VM straight to Kaggle", "publish"),
        ("Sync Push / Pull Modal", "Transfer files between local & Kaggle", "sync"),
        ("New File / Paste Clipboard", "Create remote file on worker", "newfile"),
        ("Boot Free Cloud CPU Worker", "0-quota CPU node on current slot", "cpu"),
        ("Boot Dual-T4 GPU Worker", "NvidiaTeslaT4 (30 GB VRAM) on current slot", "t4"),
        ("Shut Down Active Slot (:down)", "Stop current Kaggle VM cleanly", "down"),
        ("Keyboard Shortcuts Help (?)", "Show cheatsheet for active panel", "help"),
    ];
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|(t, h, _)| t.to_ascii_lowercase().contains(&q) || h.to_ascii_lowercase().contains(&q))
        .collect()
}

pub fn build_osc52_sequence(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let b64 = crate::worker::b64_encode(text.as_bytes());
    if b64.is_empty() || b64.len() > 65536 {
        return None;
    }
    if !b64
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return None;
    }
    Some(format!("\x1b]52;c;{}\x07", b64))
}

#[cfg(not(test))]
pub fn paste_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

#[cfg(test)]
pub fn paste_clipboard() -> Option<String> {
    None
}

pub fn fmt_bytes(b: u64) -> String {
    let f = b as f64;
    if b < 1024 {
        format!("{} B", b)
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", f / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn current_unix_sec() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

