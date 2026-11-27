use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};
use ratatui::layout::Rect;

use super::{parent_remote_dir, EditorModalState, TextField};
use crate::bridge::{HubDetail, HubItem, QuotaSummary, RefreshEndpointRes};
use crate::state::BootProfile;
use crate::worker::{DuResult, FindMatch, FindResult, FsEntry, InspectResult, MonitorPayload, TermExecResult};

pub const SLASH_COMPLETIONS: &[&str] = &[
    ":up ", ":spawn ", ":run ", ":slot ", ":kill ", ":down", ":clear", ":du", ":sync", ":flow", ":q",
];

pub const ACCELERATORS: &[(&str, &str)] = &[
    ("NvidiaTeslaT4", "Dual NVIDIA Tesla T4 (2x15GB = 30GB VRAM)"),
    ("cpu", "Free Cloud CPU Node (4 vCPU · 31GB RAM · 0 Quota)"),
    ("NvidiaL4", "NVIDIA L4 (24 GB VRAM)"),
    ("TpuV5E8", "Google Cloud TPU v5e-8 (8 XLA · 20h Pool)"),
    ("NvidiaL4x4", "Competition 4x L4 (96 GB VRAM)"),
    ("NvidiaRtxPro6000", "Competition RTX Pro 6000 (96 GB VRAM)"),
];

pub const HUB_CATEGORIES: &[(&str, &str)] = &[
    ("datasets", "Datasets"),
    ("my_datasets", "My Datasets"),
    ("competitions", "Competitions"),
    ("models", "Models"),
    ("my_kernels", "My Kernels"),
];

pub const SWAP_PANE_OPTIONS: &[(&str, &str)] = &[
    ("shell", "Shell & MCP Stream"),
    ("local", "Local Files"),
    ("remote_working", "Remote: /kaggle/working (19.5G)"),
    ("remote_tmp", "Remote: /tmp (Scratch)"),
    ("remote_input", "Remote: /kaggle/input (Datasets)"),
    ("jobs", "Background Jobs & GPU VRAM"),
    ("logs", "Live Job Logs"),
    ("hub_list", "Kaggle Hub Search"),
    ("hub_detail", "Kaggle Hub Inspector"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Cockpit,
    DualFiles,
    Jobs,
    Hub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    None,
    ShellPrompt,
    LogFilter,
    LocalFilter(Side),
    RemoteFind(Side),
    HubSearch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Size,
    Time,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            SortMode::Name => SortMode::Size,
            SortMode::Size => SortMode::Time,
            SortMode::Time => SortMode::Name,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "Name",
            SortMode::Size => "Size",
            SortMode::Time => "Time",
        }
    }
}

pub fn ensure_scroll(cursor: usize, scroll: &mut usize, view_h: usize) {
    if view_h == 0 {
        *scroll = 0;
        return;
    }
    if cursor < *scroll {
        *scroll = cursor;
    } else if cursor >= *scroll + view_h {
        *scroll = cursor + 1 - view_h;
    }
}

#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub mtime: f64,
}

#[derive(Debug, Clone)]
pub struct LocalPaneState {
    pub dir: PathBuf,
    pub entries: Vec<LocalEntry>,
    pub cursor: usize,
    pub scroll: usize,
    pub sort: SortMode,
    pub sort_desc: bool,
    pub show_hidden: bool,
    pub filter_input: TextField,
    pub selected: HashSet<PathBuf>,
    pub dir_history: HashMap<PathBuf, String>,
    pub total_files: usize,
    pub total_dirs: usize,
    pub total_bytes: u64,
}

impl LocalPaneState {
    pub fn new(dir: PathBuf) -> Self {
        let mut s = Self {
            dir,
            entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            sort: SortMode::Name,
            sort_desc: false,
            show_hidden: false,
            filter_input: TextField::new(""),
            selected: HashSet::new(),
            dir_history: HashMap::new(),
            total_files: 0,
            total_dirs: 0,
            total_bytes: 0,
        };
        s.reload();
        s
    }

    pub fn reload(&mut self) {
        let prev_name = self.entries.get(self.cursor).map(|e| e.name.clone());
        let mut items = Vec::new();
        if let Some(parent) = self.dir.parent() {
            items.push(LocalEntry {
                name: "..".into(),
                path: parent.to_path_buf(),
                is_dir: true,
                size_bytes: 0,
                mtime: 0.0,
            });
        }
        let query = self.filter_input.value.trim().to_ascii_lowercase();
        let mut total_dirs = 0usize;
        let mut total_files = 0usize;
        let mut total_bytes = 0u64;

        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            let mut entries = Vec::new();
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if !self.show_hidden && name.starts_with('.') {
                    continue;
                }
                if !query.is_empty() && !name.to_ascii_lowercase().contains(&query) {
                    continue;
                }
                let meta = e.metadata().ok();
                let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size_bytes = if is_dir {
                    0
                } else {
                    meta.as_ref().map(|m| m.len()).unwrap_or(0)
                };
                let mtime = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                if is_dir {
                    total_dirs += 1;
                } else {
                    total_files += 1;
                    total_bytes = total_bytes.saturating_add(size_bytes);
                }
                entries.push(LocalEntry {
                    name,
                    path: p,
                    is_dir,
                    size_bytes,
                    mtime,
                });
            }
            let desc = self.sort_desc;
            match self.sort {
                SortMode::Name => entries.sort_by(|a, b| {
                    let ord = a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase());
                    b.is_dir.cmp(&a.is_dir).then_with(|| if desc { ord.reverse() } else { ord })
                }),
                SortMode::Size => entries.sort_by(|a, b| {
                    let ord = a.size_bytes.cmp(&b.size_bytes);
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| if desc { ord.reverse() } else { ord })
                        .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
                }),
                SortMode::Time => entries.sort_by(|a, b| {
                    let ord = a.mtime.partial_cmp(&b.mtime).unwrap_or(std::cmp::Ordering::Equal);
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| if desc { ord.reverse() } else { ord })
                        .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
                }),
            }
            items.extend(entries.into_iter().take(400));
        }

        self.total_dirs = total_dirs;
        self.total_files = total_files;
        self.total_bytes = total_bytes;
        self.entries = items;
        if let Some(ref pn) = prev_name {
            if let Some(pos) = self.entries.iter().position(|e| &e.name == pn) {
                self.cursor = pos;
                return;
            }
        }
        if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len().saturating_sub(1);
        }
    }

    pub fn select_by_name(&mut self, name: &str) {
        if let Some(pos) = self.entries.iter().position(|e| e.name == name) {
            self.cursor = pos;
        }
    }

    pub fn navigate_into(&mut self, target_dir: PathBuf) {
        if let Some(cur) = self.entries.get(self.cursor) {
            if cur.name != ".." {
                self.dir_history.insert(self.dir.clone(), cur.name.clone());
            }
        }
        self.dir = target_dir;
        self.selected.clear();
        self.filter_input.set("");
        self.cursor = 0;
        self.scroll = 0;
        self.reload();
        if let Some(saved) = self.dir_history.get(&self.dir).cloned() {
            self.select_by_name(&saved);
        }
    }

    pub fn navigate_up(&mut self) -> bool {
        let Some(parent) = self.dir.parent().map(|p| p.to_path_buf()) else {
            return false;
        };
        let child_name = self
            .dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        if let Some(cur) = self.entries.get(self.cursor) {
            if cur.name != ".." {
                self.dir_history.insert(self.dir.clone(), cur.name.clone());
            }
        }
        self.dir = parent;
        self.selected.clear();
        self.filter_input.set("");
        self.cursor = 0;
        self.scroll = 0;
        self.reload();
        if let Some(name) = child_name {
            self.select_by_name(&name);
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct RemotePaneState {
    pub dir: String,
    pub entries: Vec<FsEntry>,
    pub raw_entries: Vec<FsEntry>,
    pub cursor: usize,
    pub scroll: usize,
    pub sort: SortMode,
    pub sort_desc: bool,
    pub show_hidden: bool,
    pub loading: bool,
    pub selected: HashSet<String>,
    pub dir_history: HashMap<String, String>,
    pub pending_select_name: Option<String>,
    pub find_input: TextField,
    pub find_active: bool,
    pub find_matches: Vec<FindMatch>,
    pub find_cursor: usize,
    pub du_banner: String,
    pub du_warn: bool,
    pub du_pct_by_path: BTreeMap<String, (f64, u64)>,
    pub total_files: usize,
    pub total_dirs: usize,
    pub total_bytes: u64,
}

impl RemotePaneState {
    pub fn new(dir: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            entries: Vec::new(),
            raw_entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            sort: SortMode::Name,
            sort_desc: false,
            show_hidden: false,
            loading: false,
            selected: HashSet::new(),
            dir_history: HashMap::new(),
            pending_select_name: None,
            find_input: TextField::new(""),
            find_active: false,
            find_matches: Vec::new(),
            find_cursor: 0,
            du_banner: String::new(),
            du_warn: false,
            du_pct_by_path: BTreeMap::new(),
            total_files: 0,
            total_dirs: 0,
            total_bytes: 0,
        }
    }

    pub fn rebuild_entries(&mut self) {
        let mut items: Vec<FsEntry> = self
            .raw_entries
            .iter()
            .filter(|e| e.name != "..")
            .filter(|e| self.show_hidden || !e.name.starts_with('.'))
            .cloned()
            .collect();

        let mut total_dirs = 0usize;
        let mut total_files = 0usize;
        let mut total_bytes = 0u64;
        for e in &items {
            if e.is_dir {
                total_dirs += 1;
            } else {
                total_files += 1;
                total_bytes = total_bytes.saturating_add(e.size_bytes);
            }
        }
        self.total_dirs = total_dirs;
        self.total_files = total_files;
        self.total_bytes = total_bytes;

        let desc = self.sort_desc;
        match self.sort {
            SortMode::Name => items.sort_by(|a, b| {
                let ord = a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase());
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| if desc { ord.reverse() } else { ord })
            }),
            SortMode::Size => items.sort_by(|a, b| {
                let ord = a.size_bytes.cmp(&b.size_bytes);
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| if desc { ord.reverse() } else { ord })
                    .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
            }),
            SortMode::Time => items.sort_by(|a, b| {
                let ord = a.mtime.partial_cmp(&b.mtime).unwrap_or(std::cmp::Ordering::Equal);
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| if desc { ord.reverse() } else { ord })
                    .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
            }),
        }

        let mut final_list = Vec::with_capacity(items.len() + 1);
        if let Some(parent) = parent_remote_dir(&self.dir) {
            final_list.push(FsEntry {
                name: "..".into(),
                path: parent,
                is_dir: true,
                size_bytes: 0,
                mtime: 0.0,
            });
        }
        final_list.extend(items);
        self.entries = final_list;

        let wanted = self
            .pending_select_name
            .take()
            .or_else(|| self.dir_history.get(&self.dir).cloned());
        if let Some(name) = wanted {
            if let Some(pos) = self.entries.iter().position(|e| e.name == name) {
                self.cursor = pos;
                return;
            }
        }
        if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len().saturating_sub(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOp {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub enum ClipSource {
    Local(Vec<PathBuf>),
    Remote(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct FileClipboard {
    pub op: ClipOp,
    pub source: ClipSource,
}

impl FileClipboard {
    pub fn count(&self) -> usize {
        match &self.source {
            ClipSource::Local(v) => v.len(),
            ClipSource::Remote(v) => v.len(),
        }
    }
    pub fn label(&self) -> &'static str {
        match self.op {
            ClipOp::Copy => "copy",
            ClipOp::Cut => "cut",
        }
    }
}

#[derive(Debug, Clone)]
pub enum PaneKind {
    Shell,
    LocalFiles(LocalPaneState),
    RemoteFiles(RemotePaneState),
    JobsAndGpu,
    LiveLogs,
    HubList,
    HubDetail,
}

#[derive(Debug, Clone)]
pub struct PreviewDrawerState {
    pub target_path: String,
    pub title: String,
    pub subtitle: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

#[derive(Debug, Clone)]
pub struct ClusterRow {
    pub slot: String,
    pub mode: String,
    pub accelerator: String,
    pub state: String,
    pub compute: String,
    pub ram: String,
    pub jobs: String,
    pub drain: String,
    pub rtt: String,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub is_err: bool,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct ConduitState {
    pub phase: f64,
    pub state_kind: &'static str,
    pub active_tool: String,
    pub active_slot: String,
    pub summary: String,
    pub beam_pos: f64,
    pub hold_until: Option<Instant>,
}

impl Default for ConduitState {
    fn default() -> Self {
        Self {
            phase: 0.0,
            state_kind: "idle",
            active_tool: String::new(),
            active_slot: String::new(),
            summary: String::new(),
            beam_pos: 0.0,
            hold_until: None,
        }
    }
}

impl ConduitState {
    pub fn trigger_tx(&mut self, tool: &str, slot: &str, summary: &str) {
        self.state_kind = "tx";
        self.active_tool = tool.to_string();
        self.active_slot = slot.to_string();
        self.summary = summary.chars().take(42).collect();
        self.beam_pos = 0.0;
        self.hold_until = Some(Instant::now() + Duration::from_millis(3600));
    }
    pub fn trigger_rx(&mut self, tool: &str, slot: &str, summary: &str, dur_sec: f64) {
        self.state_kind = "rx";
        self.active_tool = tool.to_string();
        self.active_slot = slot.to_string();
        self.summary = format!(
            "{} ({:.2}s)",
            summary.chars().take(32).collect::<String>(),
            dur_sec
        );
        self.beam_pos = 1.0;
        self.hold_until = Some(Instant::now() + Duration::from_millis(3600));
    }
    pub fn trigger_err(&mut self, tool: &str, slot: &str, err: &str) {
        self.state_kind = "err";
        self.active_tool = tool.to_string();
        self.active_slot = slot.to_string();
        self.summary = err.chars().take(42).collect();
        self.hold_until = Some(Instant::now() + Duration::from_millis(4000));
    }
    pub fn step(&mut self) {
        self.phase = (self.phase + 0.16) % (std::f64::consts::TAU * 8.0);
        if self.state_kind == "tx" {
            self.beam_pos += 0.08;
            if self.beam_pos > 1.0 {
                self.beam_pos = 0.0;
            }
        } else if self.state_kind == "rx" {
            self.beam_pos -= 0.08;
            if self.beam_pos < 0.0 {
                self.beam_pos = 1.0;
            }
        }
        if let Some(until) = self.hold_until {
            if Instant::now() >= until {
                self.state_kind = "idle";
                self.hold_until = None;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkersModalState {
    pub tab: usize,
    pub cluster_rows: Vec<ClusterRow>,
    pub cluster_selected: usize,
    pub cluster_loading: bool,
    pub profiles: Vec<BootProfile>,
    pub profile_idx: usize,
    pub profile_name: TextField,
    pub acc_idx: usize,
    pub slots: TextField,
    pub datasets: TextField,
    pub models: TextField,
    pub competition: TextField,
    pub idle_min: TextField,
    pub chain_prev: bool,
    pub field_idx: usize,
}

#[derive(Debug, Clone)]
pub struct SpawnJobModalState {
    pub command: TextField,
    pub gpu_ids: TextField,
    pub name: TextField,
    pub field_idx: usize,
}

#[derive(Debug, Clone)]
pub struct SyncModalState {
    pub is_push: bool,
    pub local_path: TextField,
    pub remote_path: TextField,
    pub field_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathActionKind {
    Rename,
    Copy,
    Mkdir,
}

#[derive(Debug, Clone)]
pub struct PathActionModalState {
    pub is_local: bool,
    pub kind: PathActionKind,
    pub src_path: String,
    pub dst_path: TextField,
    pub readonly_src: bool,
}

#[derive(Debug, Clone)]
pub struct SmartInspectModalState {
    pub info: InspectResult,
    pub scroll: usize,
}

#[derive(Debug, Clone)]
pub struct PublishDatasetModalState {
    pub source_dir: TextField,
    pub slug: TextField,
    pub title: TextField,
    pub notes: TextField,
    pub public: bool,
    pub stage_after: bool,
    pub field_idx: usize,
}

#[derive(Debug, Clone)]
pub struct SubmitCompModalState {
    pub path: String,
    pub is_remote: bool,
    pub competition: TextField,
    pub message: TextField,
    pub field_idx: usize,
}

#[derive(Debug, Clone)]
pub struct PaletteModalState {
    pub query: TextField,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub enum Modal {
    Workers(WorkersModalState),
    SwapPane(usize),
    ActionMenu(usize),
    SpawnJob(SpawnJobModalState),
    Sync(SyncModalState),
    FileEditor(EditorModalState),
    PathAction(PathActionModalState),
    SmartInspect(SmartInspectModalState),
    PublishDataset(PublishDatasetModalState),
    SubmitCompetition(SubmitCompModalState),
    CommandPalette(PaletteModalState),
    Help,
}

#[derive(Debug, Default)]
pub struct UiRects {
    pub nav_presets: Vec<(Preset, Rect)>,
    pub nav_workers: Rect,
    pub nav_help: Rect,
    pub left_rect: Rect,
    pub right_rect: Rect,
    pub left_list_rect: Rect,
    pub right_list_rect: Rect,
    pub drawer_rect: Rect,
    pub preview_body_rect: Rect,
    pub preview_code_rect: Rect,
    pub shell_content_rect: Rect,
    pub logs_content_rect: Rect,
    pub hub_detail_rect: Rect,
    pub prompt_rect: Rect,
    pub modal_rect: Rect,
    pub modal_list_rect: Rect,
    pub editor_path_rect: Rect,
    pub editor_body_rect: Rect,
    pub editor_code_rect: Rect,
    pub inspect_content_rect: Rect,
    pub buttons: Vec<(Rect, String)>,
}

pub enum AppMsg {
    MonitorOk {
        slot: String,
        payload: MonitorPayload,
        rtt_ms: f64,
    },
    MonitorErr {
        slot: String,
        tunnel_down: bool,
        message: String,
    },
    EndpointRefreshed {
        slot: String,
        res: RefreshEndpointRes,
    },
    BatchLogsDone {
        slot: String,
        status: String,
        logs: String,
    },
    QuotaOk(QuotaSummary),
    QuotaErr(String),
    BootDone(String),
    ExecDone {
        slot: String,
        cmd: String,
        res: TermExecResult,
    },
    ExecErr {
        slot: String,
        cmd: String,
        err: String,
    },
    JobsActionDone {
        msg: String,
        selected_job_id: Option<String>,
    },
    FsListDone {
        side: Side,
        dir: String,
        entries: Vec<FsEntry>,
    },
    PreviewDrawerLoaded(PreviewDrawerState),
    FsReadDone(String, String),
    FsInspectDone(InspectResult),
    FsDuDone {
        side: Side,
        dir: String,
        res: DuResult,
    },
    FsFindDone {
        side: Side,
        query: String,
        res: FindResult,
    },
    FsOpDone {
        msg: String,
        refresh: bool,
    },
    FsPublishDone {
        dataset_ref: String,
        url: String,
        stage_after: bool,
    },
    SyncDone {
        msg: String,
        refresh_remote: bool,
        refresh_local: bool,
    },
    HubSearchDone(Vec<HubItem>),
    HubInspectDone(HubDetail),
    HubLogsDone(String),
    HubDeleteDone(String),
    SubmitCsvDone {
        competition: String,
        result: String,
    },
    ClusterRows(Vec<ClusterRow>),
    TaskFinished(u64),
    Toast {
        text: String,
        is_err: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Plain,
    ShellStream,
    PreviewCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub start: (u16, u16),
    pub end: (u16, u16),
    pub bounds: Rect,
    pub dragging: bool,
    pub kind: SelectionKind,
    pub line_mode: bool,
}

#[derive(Debug, Clone)]
pub struct RunningExec {
    pub command: String,
    pub started_at: Instant,
    pub live_output: String,
    pub worker_has_live: bool,
}

