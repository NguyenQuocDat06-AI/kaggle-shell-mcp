//! Calm Minimalist Dual-Panel Ratatui UI (`LEFT | RIGHT`), Graphite + 1 Accent + Semantic Status.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

mod modals;
mod panes;

use modals::render_modal;
use panes::{render_pane, render_preview_drawer};

use crate::app::{
    current_unix_sec, ensure_scroll, filtered_palette_items, fmt_bytes, App, EditorModalState,
    InputFocus, Modal, PaletteModalState, PaneKind, PathActionKind, PathActionModalState, Preset,
    PublishDatasetModalState, SelectionKind, Side, SmartInspectModalState, SpawnJobModalState,
    SortMode, SubmitCompModalState, SyncModalState, TextField, UiRects, WorkersModalState,
    ACCELERATORS, HUB_CATEGORIES, SWAP_PANE_OPTIONS,
};
use crate::file_icons::{
    du_micro_bar, file_badge, fmt_mtime, highlight_preview_line, truncate_end, truncate_middle,
    ACCENT, BG, BORDER, BORDER_ACTIVE, ERR, MARKED_BG, MUTED, OK, SEL_BG, SEL_GHOST_BG, SURFACE,
    TEXT, WARN,
};

pub(super) const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
pub(super) const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub(super) fn bar_spans(pct: f64, width: usize) -> Vec<Span<'static>> {
    let clamped = pct.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    let col = if clamped < 65.0 { ACCENT } else if clamped < 88.0 { WARN } else { ERR };
    vec![
        Span::styled("━".repeat(filled), Style::default().fg(col)),
        Span::styled("─".repeat(empty), Style::default().fg(BORDER)),
    ]
}

#[allow(clippy::too_many_arguments)]
pub(super) fn text_btn(
    rects: &mut UiRects,
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    y: u16,
    max_x: u16,
    id: &str,
    label: &str,
    style: Style,
) -> bool {
    let w = label.chars().count() as u16;
    if *x + w > max_x {
        return false;
    }
    rects.buttons.push((Rect::new(*x, y, w, 1), id.to_string()));
    spans.push(Span::styled(label.to_string(), style));
    *x = x.saturating_add(w);
    if *x + 2 <= max_x {
        spans.push(Span::raw("  "));
        *x = x.saturating_add(2);
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) fn key_hint_btn(
    rects: &mut UiRects,
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    y: u16,
    max_x: u16,
    id: &str,
    key: &str,
    desc: &str,
    key_col: Color,
) -> bool {
    let txt = format!("{} {}", key, desc);
    let w = txt.chars().count() as u16;
    if *x + w > max_x {
        return false;
    }
    rects.buttons.push((Rect::new(*x, y, w, 1), id.to_string()));
    spans.push(Span::styled(
        key.to_string(),
        Style::default().fg(key_col).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(format!(" {}", desc), Style::default().fg(MUTED)));
    *x = x.saturating_add(w);
    if *x + 2 <= max_x {
        spans.push(Span::raw("  "));
        *x = x.saturating_add(2);
    }
    true
}

pub fn render(f: &mut Frame, app: &mut App) {
    app.rects.buttons.clear();
    app.rects.nav_presets.clear();
    app.rects.modal_rect = Rect::default();
    app.rects.modal_list_rect = Rect::default();
    app.rects.editor_path_rect = Rect::default();
    app.rects.editor_body_rect = Rect::default();
    app.rects.editor_code_rect = Rect::default();
    app.rects.inspect_content_rect = Rect::default();
    app.rects.preview_body_rect = Rect::default();
    app.rects.preview_code_rect = Rect::default();
    app.rects.shell_content_rect = Rect::default();
    app.rects.logs_content_rect = Rect::default();
    app.rects.hub_detail_rect = Rect::default();
    app.rects.prompt_rect = Rect::default();

    let size = f.area();
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(TEXT)), size);

    let mut top_constraints = vec![Constraint::Length(1)];
    if app.show_telemetry {
        top_constraints.push(Constraint::Length(3));
    }
    top_constraints.push(Constraint::Min(8));
    top_constraints.push(Constraint::Length(1));

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints(top_constraints)
        .split(size);

    render_top_nav(f, app, root[0]);
    let stage_idx = if app.show_telemetry {
        render_slim_telemetry(f, app, root[1]);
        2
    } else {
        1
    };
    let footer_idx = stage_idx + 1;

    let dual_area = if app.preview_drawer.is_some() {
        let vsplit = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(root[stage_idx]);
        app.rects.drawer_rect = vsplit[1];
        vsplit[0]
    } else {
        app.rects.drawer_rect = Rect::default();
        root[stage_idx]
    };

    if app.zoomed {
        match app.active_side {
            Side::Left => {
                app.rects.left_rect = dual_area;
                app.rects.right_rect = Rect::default();
                render_pane(f, app, Side::Left, dual_area);
            }
            Side::Right => {
                app.rects.left_rect = Rect::default();
                app.rects.right_rect = dual_area;
                render_pane(f, app, Side::Right, dual_area);
            }
        }
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(dual_area);
        app.rects.left_rect = cols[0];
        app.rects.right_rect = cols[1];
        render_pane(f, app, Side::Left, cols[0]);
        render_pane(f, app, Side::Right, cols[1]);
    }

    if app.preview_drawer.is_some() {
        let dr = app.rects.drawer_rect;
        render_preview_drawer(f, app, dr);
    }

    render_footer(f, app, root[footer_idx]);

    if app.modal.is_some() {
        render_modal(f, app, size);
    }
    if let Some(ref toast) = app.toast {
        render_toast(f, &toast.text, toast.is_err, size);
    }
}

pub fn render_selection_overlay(f: &mut Frame, app: &App) {
    if matches!(app.modal, Some(Modal::FileEditor(_))) {
        return;
    }
    let Some(sel) = app.selection else {
        return;
    };
    let ((sx, sy), (ex, ey)) = if (sel.start.1, sel.start.0) <= (sel.end.1, sel.end.0) {
        (sel.start, sel.end)
    } else {
        (sel.end, sel.start)
    };
    let buf = f.buffer_mut();
    let max_y = buf.area().bottom().saturating_sub(1);
    let max_x = buf.area().right().saturating_sub(1);
    for row in sy..=ey.min(max_y) {
        let mut cs = if row == sy { sx } else { sel.bounds.x };
        let ce = if row == ey {
            ex
        } else {
            sel.bounds.right().saturating_sub(1)
        }
        .min(max_x);
        if sel.kind == SelectionKind::ShellStream {
            let gw = App::shell_gutter_width(buf, sel.bounds.x, row, max_x);
            cs = cs.max(sel.bounds.x.saturating_add(gw));
        }
        for col in cs..=ce {
            if let Some(cell) = buf.cell_mut((col, row)) {
                cell.set_bg(ACCENT).set_fg(Color::Black);
            }
        }
    }
}

fn render_top_nav(f: &mut Frame, app: &mut App, area: Rect) {
    let mut x = area.x;
    let y = area.y;
    let max_x = area.x + area.width;

    let brand = " kgsh ";
    let sep = "│ ";
    let mut spans = vec![
        Span::styled(brand, Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
        Span::styled(sep, Style::default().fg(BORDER)),
    ];
    x = x.saturating_add((brand.chars().count() + sep.chars().count()) as u16);

    let presets = [
        (Preset::Cockpit, "F1 Cockpit"),
        (Preset::DualFiles, "F2 Files"),
        (Preset::Jobs, "F3 Jobs"),
        (Preset::Hub, "F4 Hub"),
    ];
    for (p, lbl) in presets {
        let active = app.preset == p;
        let w = lbl.chars().count() as u16;
        if x + w > max_x {
            break;
        }
        app.rects.nav_presets.push((p, Rect::new(x, y, w, 1)));
        let st = if active {
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(MUTED)
        };
        spans.push(Span::styled(lbl, st));
        x = x.saturating_add(w);
        if x + 2 <= max_x {
            spans.push(Span::raw("  "));
            x = x.saturating_add(2);
        }
    }

    let acc = app.active_session().map(|s| s.accelerator).unwrap_or_else(|| "idle".into());
    let dot_col = if app.is_online { OK } else { WARN };
    let rtt = app.rtt_ms.map(|r| format!(" {:.0}ms", r)).unwrap_or_default();
    let gpu_q = app
        .quota
        .as_ref()
        .map(|q| format!(" · GPU {:.1}h", q.gpu.remaining_hours))
        .unwrap_or_default();
    let worker_txt = format!("slot={} {}{}{}", app.current_slot, acc, rtt, gpu_q);
    let worker_full_w = 2 + worker_txt.chars().count() as u16; // "● " + worker_txt
    let mouse_lbl = if app.mouse_capture { "F8 Select" } else { "F8 Mouse:OFF" };
    let mouse_w = mouse_lbl.chars().count() as u16;
    let actions_w = 3 + 10 + 2 + mouse_w + 2 + 2 + 2 + 1; // " │ F5 Workers  <mouse_lbl>  ^K  ?"

    let left_used = x.saturating_sub(area.x);
    let right_full = worker_full_w + actions_w;
    if left_used + right_full + 1 < area.width {
        let gap = area.width - left_used - right_full;
        spans.push(Span::raw(" ".repeat(gap as usize)));
        x = x.saturating_add(gap);
        app.rects.nav_workers = Rect::new(x, y, worker_full_w, 1);
        spans.push(Span::styled("● ", Style::default().fg(dot_col)));
        spans.push(Span::styled(worker_txt, Style::default().fg(TEXT)));
        x = x.saturating_add(worker_full_w);

        spans.push(Span::styled(" │ ", Style::default().fg(BORDER)));
        x = x.saturating_add(3);

        text_btn(&mut app.rects, &mut spans, &mut x, y, max_x, "nav-boot", "F5 Workers", Style::default().fg(MUTED));
        let mouse_st = if app.mouse_capture {
            Style::default().fg(MUTED)
        } else {
            Style::default().fg(WARN).add_modifier(Modifier::BOLD)
        };
        text_btn(&mut app.rects, &mut spans, &mut x, y, max_x, "nav-mouse", mouse_lbl, mouse_st);
        text_btn(&mut app.rects, &mut spans, &mut x, y, max_x, "nav-palette", "^K", Style::default().fg(MUTED));

        let hw = 1u16;
        if x + hw <= max_x {
            app.rects.nav_help = Rect::new(x, y, hw, 1);
            spans.push(Span::styled("?", Style::default().fg(MUTED)));
        } else {
            app.rects.nav_help = Rect::default();
        }
    } else if x + worker_full_w < max_x {
        spans.push(Span::raw(" "));
        x = x.saturating_add(1);
        app.rects.nav_workers = Rect::new(x, y, worker_full_w, 1);
        spans.push(Span::styled("● ", Style::default().fg(dot_col)));
        spans.push(Span::styled(worker_txt, Style::default().fg(TEXT)));
        app.rects.nav_help = Rect::default();
    } else {
        app.rects.nav_workers = Rect::default();
        app.rects.nav_help = Rect::default();
    }

    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(SURFACE)), area);
}

fn render_slim_telemetry(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    let c = &app.conduit;
    let (state_txt, state_col) = match c.state_kind {
        "tx" => (format!("-> {}", c.active_tool), ACCENT),
        "rx" => (format!("<- {}", c.active_tool), OK),
        "err" => (format!("err {}", c.active_tool), ERR),
        _ => ("idle".into(), MUTED),
    };
    let st = &app.worker_status;
    let compute = if st.gpus.is_empty() {
        format!("CPU {:.1}%", st.cpu_pct)
    } else {
        let g = &st.gpus[0];
        format!(
            "{}x GPU0 {:.0}% ({:.1}/{:.1}G)",
            st.gpus.len(),
            g.util_pct,
            g.vram_used_mb / 1024.0,
            g.vram_total_mb / 1024.0
        )
    };
    let drain_s = st.remaining_before_drain_sec.max(0.0) as u64;
    let run_jobs = app.jobs.iter().filter(|j| j.status == "running").count();
    let line1 = Line::from(vec![
        Span::styled(" MCP ", Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
        Span::styled(state_txt, Style::default().fg(state_col)),
        Span::styled(" │ ", Style::default().fg(BORDER)),
        Span::styled(compute, Style::default().fg(TEXT)),
        Span::styled(
            format!(
                " · RAM {:.1}/{:.1}G · Disk {:.1}/{:.1}G · /tmp {:.0}G free · Drain {}h{:02}m · Jobs {}",
                st.ram.used_gb,
                st.ram.total_gb,
                st.disk_working.used_gb,
                st.disk_working.total_gb,
                st.disk_tmp.free_gb,
                drain_s / 3600,
                (drain_s % 3600) / 60,
                run_jobs
            ),
            Style::default().fg(MUTED),
        ),
    ]);
    f.render_widget(Paragraph::new(line1).style(Style::default().bg(BG)), rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(rows[1]);
    let w = cols[0].width.saturating_sub(2) as usize;
    let mut sp_spans = vec![Span::raw(" ")];
    let samples: Vec<f64> = app
        .osc_history
        .iter()
        .rev()
        .take(w.max(1))
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    for val in samples {
        let pct = val.clamp(0.0, 100.0);
        let idx = ((pct / 100.0) * 7.0).round() as usize;
        let col = if pct <= 0.5 { BORDER } else if pct < 80.0 { ACCENT } else { WARN };
        sp_spans.push(Span::styled(SPARK[idx].to_string(), Style::default().fg(col)));
    }
    f.render_widget(Paragraph::new(Line::from(sp_spans)).style(Style::default().bg(BG)), cols[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("  {}", app.status_line),
            Style::default().fg(if app.is_online { OK } else { WARN }),
        ))
        .style(Style::default().bg(BG)),
        cols[1],
    );

    f.render_widget(
        Paragraph::new("─".repeat(rows[2].width as usize)).style(Style::default().fg(BORDER).bg(BG)),
        rows[2],
    );
}

fn render_footer(f: &mut Frame, app: &mut App, area: Rect) {
    let in_input = app.input_focus != InputFocus::None;
    let mut x = area.x;
    let y = area.y;
    let max_x = area.x + area.width;
    let mut spans = Vec::new();

    let (m_txt, m_col) = if in_input { (" INPUT ", OK) } else { (" NAV ", MUTED) };
    spans.push(Span::styled(m_txt, Style::default().fg(m_col).add_modifier(Modifier::BOLD)));
    x = x.saturating_add(m_txt.chars().count() as u16);

    if let Some(ref clip) = app.file_clipboard {
        let tag = format!("· {} {} ", clip.count(), clip.label());
        let w = tag.chars().count() as u16;
        if x + w + 3 < max_x {
            spans.push(Span::styled(tag, Style::default().fg(WARN).add_modifier(Modifier::BOLD)));
            x = x.saturating_add(w);
        }
    }

    if let Some((_, lbl)) = app.active_tasks.last() {
        let spin = SPINNER[(app.frame_tick as usize) % SPINNER.len()];
        let tag = format!("· {} {} ", spin, truncate_end(lbl, 16));
        let w = tag.chars().count() as u16;
        if x + w + 3 < max_x {
            spans.push(Span::styled(tag, Style::default().fg(ACCENT)));
            x = x.saturating_add(w);
        }
    }

    if x + 3 < max_x {
        spans.push(Span::styled("│ ", Style::default().fg(BORDER)));
        x = x.saturating_add(2);
    }

    let opp_is_remote = matches!(app.pane(app.active_side.opposite()), PaneKind::RemoteFiles(_));
    let opp_is_local = matches!(app.pane(app.active_side.opposite()), PaneKind::LocalFiles(_));
    let del_armed_local = app
        .delete_arm
        .as_ref()
        .map(|(k, ts)| (k.starts_with("local:") || k == "__local_batch__") && ts.elapsed().as_secs() <= 4)
        .unwrap_or(false);
    let del_armed_remote = app
        .delete_arm
        .as_ref()
        .map(|(k, ts)| !k.starts_with("local:") && k != "__local_batch__" && ts.elapsed().as_secs() <= 4)
        .unwrap_or(false);

    let hints: Vec<(&str, &str, &str, Color)> = if in_input {
        vec![
            ("foot-help", "Enter", "apply", TEXT),
            ("foot-help", "Esc", "nav", TEXT),
        ]
    } else {
        match app.active_pane() {
            PaneKind::Shell => {
                let mut v = vec![
                    ("foot-shell-input", "i", "type", TEXT),
                    ("foot-shell-copy", "y", "copy", TEXT),
                ];
                if app.running_exec.is_some() {
                    v.push(("foot-shell-stop", "^C", "stop", ERR));
                }
                v.extend([
                    ("foot-pane", "Tab/←→", "pane", TEXT),
                    ("foot-zoom", "z", "zoom", TEXT),
                    ("foot-swap", "m", "swap", TEXT),
                    ("foot-tel", "T", "telemetry", TEXT),
                    ("foot-help", "?", "help", TEXT),
                ]);
                v
            }
            PaneKind::LocalFiles(_) => {
                let tr = if opp_is_remote { "push→" } else { "copy→" };
                let (dk, dd, dc) = if del_armed_local {
                    ("d", "CONFIRM!", ERR)
                } else {
                    ("d", "del", TEXT)
                };
                vec![
                    ("foot-up", "←", "up", TEXT),
                    ("foot-open", "→", "open", TEXT),
                    ("foot-sel", "Spc", "sel", TEXT),
                    ("foot-transfer", "t", tr, TEXT),
                    ("foot-clip-copy", "c/x/p", "clip", TEXT),
                    ("foot-mkdir", "N", "dir", TEXT),
                    ("foot-rename", "R", "ren", TEXT),
                    ("foot-del", dk, dd, dc),
                    ("foot-preview", "v", "prev", TEXT),
                    ("foot-find", "/", "filter", TEXT),
                    ("foot-dot", ".", "hidden", TEXT),
                    ("foot-actions", "a", "more", TEXT),
                    ("foot-pane", "Tab", "pane", TEXT),
                ]
            }
            PaneKind::RemoteFiles(_) => {
                let tr = if opp_is_local { "←pull" } else if opp_is_remote { "copy→" } else { "pull" };
                let (dk, dd, dc) = if del_armed_remote {
                    ("d", "CONFIRM!", ERR)
                } else {
                    ("d", "del", TEXT)
                };
                vec![
                    ("foot-up", "←", "up", TEXT),
                    ("foot-open", "→", "open", TEXT),
                    ("foot-sel", "Spc", "sel", TEXT),
                    ("foot-transfer", "t", tr, TEXT),
                    ("foot-clip-copy", "c/x/p", "clip", TEXT),
                    ("foot-new", "n", "file", TEXT),
                    ("foot-mkdir", "N", "dir", TEXT),
                    ("foot-rename", "R", "ren", TEXT),
                    ("foot-del", dk, dd, dc),
                    ("foot-preview", "v", "prev", TEXT),
                    ("foot-find", "/", "find", TEXT),
                    ("foot-du", "u", "du", TEXT),
                    ("foot-run-file", "X", "run", TEXT),
                    ("foot-dot", ".", "hidden", TEXT),
                    ("foot-actions", "a", "more", TEXT),
                    ("foot-pane", "Tab", "pane", TEXT),
                ]
            }
            PaneKind::JobsAndGpu => vec![
                ("foot-spawn-job", "n", "spawn", TEXT),
                ("foot-kill-job", "x", "kill-job", TEXT),
                ("foot-kill-gpu", "K", "kill-gpu", TEXT),
                ("foot-pane", "←→/Tab", "pane", TEXT),
                ("foot-help", "?", "help", TEXT),
            ],
            PaneKind::LiveLogs => vec![
                ("foot-log-pause", "Spc", "pause", TEXT),
                ("foot-find", "/", "filter", TEXT),
                ("foot-log-copy", "y", "copy", TEXT),
                ("foot-pane", "←→/Tab", "pane", TEXT),
                ("foot-zoom", "z", "zoom", TEXT),
            ],
            PaneKind::HubList | PaneKind::HubDetail => vec![
                ("foot-hub-stage", "s", "stage", TEXT),
                ("foot-hub-dl", "d", "download", TEXT),
                ("foot-hub-logs", "l", "logs", TEXT),
                ("foot-pane", "Tab", "pane", TEXT),
                ("foot-help", "?", "help", TEXT),
            ],
        }
    };

    for (id, k, d, col) in hints {
        if !key_hint_btn(&mut app.rects, &mut spans, &mut x, y, max_x, id, k, d, col) {
            break;
        }
    }

    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(SURFACE)), area);
}

pub(super) fn char_slice(s: &str, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    s.chars().skip(start).take(end - start).collect()
}

pub(super) fn text_field_spans(tf: &TextField, normal_st: Style) -> Vec<Span<'static>> {
    let Some((s, e)) = tf.selection_range() else {
        return vec![Span::styled(tf.value.clone(), normal_st)];
    };
    let len = tf.value.chars().count();
    let s = s.min(len);
    let e = e.min(len);
    let mut spans = Vec::new();
    if s > 0 {
        spans.push(Span::styled(char_slice(&tf.value, 0, s), normal_st));
    }
    if e > s {
        spans.push(Span::styled(
            char_slice(&tf.value, s, e),
            Style::default().bg(ACCENT).fg(Color::Black),
        ));
    }
    if e < len {
        spans.push(Span::styled(char_slice(&tf.value, e, len), normal_st));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), normal_st));
    }
    spans
}

pub(super) fn field_line(label: &str, value: &str, active: bool) -> Line<'static> {
    let st = if active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    let val_st = if active {
        Style::default().fg(Color::White).bg(SEL_BG)
    } else {
        Style::default().fg(TEXT).bg(SURFACE)
    };
    Line::from(vec![
        Span::styled(format!("{:<20}: ", label), st),
        Span::styled(format!(" {} ", value), val_st),
    ])
}

pub(super) fn field_line_tf(label: &str, tf: &TextField, active: bool) -> Line<'static> {
    let st = if active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    let val_st = if active {
        Style::default().fg(Color::White).bg(SEL_BG)
    } else {
        Style::default().fg(TEXT).bg(SURFACE)
    };
    let mut spans = vec![
        Span::styled(format!("{:<20}: ", label), st),
        Span::styled(" ", val_st),
    ];
    spans.extend(text_field_spans(tf, val_st));
    spans.push(Span::styled(" ", val_st));
    Line::from(spans)
}

fn render_toast(f: &mut Frame, text: &str, is_err: bool, screen: Rect) {
    let w = (text.chars().count() as u16 + 4).min(screen.width.saturating_sub(4)).max(24);
    let y = screen.height.saturating_sub(4);
    let r = Rect::new(screen.width.saturating_sub(w + 2), y, w, 3);
    f.render_widget(Clear, r);
    let col = if is_err { ERR } else { BORDER_ACTIVE };
    let blk = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(col))
        .style(Style::default().bg(SURFACE).fg(TEXT));
    f.render_widget(Paragraph::new(text.to_string()).block(blk).wrap(Wrap { trim: true }), r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio::sync::mpsc;

    use crate::app::{AppMsg, ClipOp, ClipSource, FileClipboard, PathActionKind, PathActionModalState, PreviewDrawerState, TextField};
    use crate::bridge::{HubDetail, PythonBridge};
    use crate::worker::{DuChild, DuResult, FsEntry, GpuInfo, GpuProcess, JobInfo, MonitorPayload, WorkerStatus};

    #[tokio::test]
    async fn test_universal_dual_panel_presets_swap_zoom_drawer_render() {
        let dir = std::env::temp_dir().join(format!("kgsh_dual_ui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("local_train.py"), "def main():\n    return 1\n").unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);

        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        // Preset 1: Cockpit (Shell | LiveLogs)
        terminal.draw(|f| render(f, &mut app)).unwrap();

        app.handle_msg(AppMsg::MonitorOk {
            slot: "1".into(),
            rtt_ms: 42.0,
            payload: MonitorPayload {
                status: WorkerStatus {
                    uptime_sec: 120.0,
                    remaining_before_drain_sec: 41000.0,
                    gpus: vec![GpuInfo {
                        index: 0,
                        name: "Tesla T4".into(),
                        util_pct: 77.0,
                        vram_used_mb: 10240.0,
                        vram_total_mb: 15360.0,
                        temp_c: 62.0,
                    }],
                    gpu_processes: vec![GpuProcess {
                        pid: 4242,
                        gpu_index: "0".into(),
                        process_name: "python3".into(),
                        used_mb: 8192,
                    }],
                    ..Default::default()
                },
                jobs: vec![JobInfo {
                    job_id: "job_1".into(),
                    name: "train".into(),
                    gpu_ids: "0".into(),
                    status: "running".into(),
                    runtime_sec: 30.0,
                    log_size_kb: 10.0,
                    started_at: 1.0,
                    pid: Some(4242),
                }],
                ..Default::default()
            },
        });
        terminal.draw(|f| render(f, &mut app)).unwrap();

        // Preset 2: DualFiles with Local + Remote, DU micro-bars, multi-select, clipboard badge, task spinner, filter & Preview Drawer
        app.apply_preset(Preset::DualFiles);
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.filter_input.set("local");
            lp.reload();
            if let Some(ent) = lp.entries.iter().find(|e| e.name != "..") {
                lp.selected.insert(ent.path.clone());
            }
        }
        app.handle_msg(AppMsg::FsListDone {
            side: Side::Right,
            dir: "/kaggle/working/exp1".into(),
            entries: vec![
                FsEntry {
                    name: "checkpoints".into(),
                    path: "/kaggle/working/exp1/checkpoints".into(),
                    is_dir: true,
                    size_bytes: 0,
                    mtime: 100.0,
                },
                FsEntry {
                    name: "sub.csv".into(),
                    path: "/kaggle/working/exp1/sub.csv".into(),
                    is_dir: false,
                    size_bytes: 512,
                    mtime: 100.0,
                },
            ],
        });
        app.handle_msg(AppMsg::FsDuDone {
            side: Side::Right,
            dir: "/kaggle/working/exp1".into(),
            res: DuResult {
                path: "/kaggle/working/exp1".into(),
                total_bytes: 1024,
                partial: false,
                children: vec![DuChild {
                    name: "checkpoints".into(),
                    path: "/kaggle/working/exp1/checkpoints".into(),
                    is_dir: true,
                    size_bytes: 768,
                    pct: 75.0,
                }],
            },
        });
        app.file_clipboard = Some(FileClipboard {
            op: ClipOp::Copy,
            source: ClipSource::Remote(vec!["/kaggle/working/exp1/sub.csv".into()]),
        });
        let tid = app.begin_task("Push sub.csv");
        app.preview_drawer = Some(PreviewDrawerState {
            target_path: "/kaggle/working/exp1/sub.csv".into(),
            title: "/kaggle/working/exp1/sub.csv".into(),
            subtitle: "CSV · 512 B".into(),
            lines: vec!["id,target".into(), "1,0.9".into()],
            scroll: 0,
        });
        terminal.draw(|f| render(f, &mut app)).unwrap();

        // Narrow viewport (88x24)
        let small = TestBackend::new(88, 24);
        let mut small_term = Terminal::new(small).unwrap();
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.end_task(tid);

        // Remote<->Remote swap
        app.assign_pane(Side::Left, "remote_tmp");
        terminal.draw(|f| render(f, &mut app)).unwrap();

        // Preset 3: Jobs | Logs + Zoom
        app.preview_drawer = None;
        app.apply_preset(Preset::Jobs);
        terminal.draw(|f| render(f, &mut app)).unwrap();
        app.zoomed = true;
        terminal.draw(|f| render(f, &mut app)).unwrap();
        app.zoomed = false;

        // Preset 4: Hub | Detail + Modals
        app.apply_preset(Preset::Hub);
        app.hub_detail = Some(HubDetail {
            category: "datasets".into(),
            item_ref: "alice/ds".into(),
            text: "train.parquet 10 MB".into(),
            stage_kind: Some("dataset".into()),
            stage_value: "alice/ds".into(),
            can_download: true,
        });
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.modal = Some(Modal::PathAction(PathActionModalState {
            is_local: true,
            kind: PathActionKind::Mkdir,
            src_path: dir.to_string_lossy().to_string(),
            dst_path: TextField::new("new_folder"),
            readonly_src: false,
        }));
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.modal = Some(Modal::ActionMenu(0));
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.modal = Some(Modal::SwapPane(0));
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.open_workers_modal(0);
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.open_workers_modal(1);
        small_term.draw(|f| render(f, &mut app)).unwrap();
        app.modal = Some(Modal::Help);
        small_term.draw(|f| render(f, &mut app)).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn svg_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    fn color_to_hex(c: Color, default_hex: &str) -> String {
        match c {
            Color::Rgb(r, g, b) => format!("#{:02x}{:02x}{:02x}", r, g, b),
            Color::Black => "#0d0f12".into(),
            Color::White => "#f0f4f8".into(),
            Color::Red => "#d95f5f".into(),
            Color::Green => "#3fbf7f".into(),
            Color::Yellow => "#d4a656".into(),
            Color::Blue | Color::Cyan => "#63a4d8".into(),
            Color::Gray => "#868d98".into(),
            Color::DarkGray => "#262b33".into(),
            _ => default_hex.into(),
        }
    }

    fn buffer_to_svg(buf: &ratatui::buffer::Buffer, title: &str) -> String {
        let area = *buf.area();
        let cw = 8.4_f64;
        let ch = 17.0_f64;
        let pad_x = 14.0_f64;
        let title_h = 30.0_f64;
        let pad_y = 10.0_f64;
        let width = (area.width as f64 * cw + pad_x * 2.0).ceil();
        let height = (area.height as f64 * ch + title_h + pad_y * 2.0).ceil();

        let mut rects = String::new();
        let mut texts = String::new();

        for row in 0..area.height {
            let py = title_h + pad_y + row as f64 * ch;
            for col in 0..area.width {
                let Some(cell) = buf.cell((col, row)) else { continue; };
                let px = pad_x + col as f64 * cw;
                let bg_hex = color_to_hex(cell.bg, "#0d0f12");
                if bg_hex != "#0d0f12" {
                    rects.push_str(&format!(
                        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{}\"/>",
                        px,
                        py,
                        cw + 0.2,
                        ch + 0.2,
                        bg_hex
                    ));
                }
                let sym = cell.symbol();
                if sym.is_empty() || sym == " " {
                    continue;
                }
                let fg_hex = color_to_hex(cell.fg, "#d4d7dd");
                let weight = if cell.modifier.contains(Modifier::BOLD) {
                    " font-weight=\"700\""
                } else {
                    ""
                };
                let deco = if cell.modifier.contains(Modifier::UNDERLINED) {
                    " text-decoration=\"underline\""
                } else {
                    ""
                };
                texts.push_str(&format!(
                    "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\"{}{}>{}</text>",
                    px,
                    py + 13.0,
                    fg_hex,
                    weight,
                    deco,
                    svg_escape(sym)
                ));
            }
        }

        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" viewBox=\"0 0 {:.0} {:.0}\">\n\
             <rect width=\"100%\" height=\"100%\" rx=\"10\" fill=\"#0d0f12\" stroke=\"#262b33\" stroke-width=\"1.5\"/>\n\
             <rect x=\"1\" y=\"1\" width=\"{:.0}\" height=\"29\" rx=\"9\" fill=\"#15181e\"/>\n\
             <circle cx=\"18\" cy=\"15\" r=\"5\" fill=\"#3f4652\"/>\n\
             <circle cx=\"34\" cy=\"15\" r=\"5\" fill=\"#3f4652\"/>\n\
             <circle cx=\"50\" cy=\"15\" r=\"5\" fill=\"#3f4652\"/>\n\
             <text x=\"{:.1}\" y=\"19\" text-anchor=\"middle\" fill=\"#868d98\" font-family=\"'JetBrains Mono','Cascadia Mono','Consolas',monospace\" font-size=\"12\">{}</text>\n\
             <g>\n{}\n</g>\n\
             <g font-family=\"'JetBrains Mono','Cascadia Mono','Consolas',monospace\" font-size=\"13.5\">\n{}\n</g>\n\
             </svg>\n",
            width,
            height,
            width,
            height,
            width - 2.0,
            width / 2.0,
            svg_escape(title),
            rects,
            texts
        )
    }

    #[tokio::test]
    #[ignore]
    async fn generate_doc_screenshots() {
        use crate::app::{ClipOp, ClipSource, FileClipboard, RunningExec};
        use crate::bridge::{QuotaBucket, QuotaSummary};
        use crate::state::{ActiveSession, ActivityEvent};
        use crate::worker::{DiskInfo, DuChild, DuResult, FsEntry, GpuInfo, GpuProcess, JobInfo, MonitorPayload, RamInfo, WorkerStatus};
        use std::time::{Duration, Instant};

        let assets_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("docs")
            .join("assets");
        std::fs::create_dir_all(&assets_dir).unwrap();

        let dir = std::env::temp_dir().join(format!("kgsh_docs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("kaggle_project");
        std::fs::create_dir_all(proj.join("configs")).unwrap();
        std::fs::create_dir_all(proj.join("notebooks")).unwrap();
        std::fs::write(
            proj.join("train.py"),
            "import torch\nfor epoch in range(1, 20):\n    loss = train_step(epoch)\n    print(f\"epoch={epoch} loss={loss:.4f}\")\n",
        )
        .unwrap();
        std::fs::write(proj.join("eval.py"), "def evaluate():\n    return {\"acc\": 0.942}\n").unwrap();
        std::fs::write(proj.join("sub.csv"), "id,target\n1,0.91\n").unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);
        app.sessions.insert(
            "1".into(),
            ActiveSession {
                session_id: "sess_demo_1".into(),
                worker_token: "tok".into(),
                tunnel_url: "https://demo-1.trycloudflare.com".into(),
                ts_url: String::new(),
                slot: "1".into(),
                mode: "live".into(),
                accelerator: "NvidiaTeslaT4".into(),
                kernel_ref: "kaggle_user/kaggle-mcp-worker".into(),
                version_number: Some(12),
                created_at: 1_730_000_000.0,
                ..Default::default()
            },
        );
        app.quota = Some(QuotaSummary {
            username: "kaggle_user".into(),
            gpu: QuotaBucket { used_hours: 3.6, remaining_hours: 26.4, total_hours: 30.0, used_pct: 12.0, ..Default::default() },
            tpu: QuotaBucket { used_hours: 0.0, remaining_hours: 20.0, total_hours: 20.0, used_pct: 0.0, ..Default::default() },
            quota_refresh_time: "Sat Nov 28".into(),
        });
        app.osc_history = vec![
            12.0, 18.0, 22.0, 35.0, 62.0, 74.0, 81.0, 79.0, 85.0, 88.0, 82.0, 76.0,
            84.0, 91.0, 89.0, 86.0, 84.0, 78.0, 83.0, 87.0, 85.0, 80.0, 84.0, 86.0,
            88.0, 83.0, 81.0, 84.0, 89.0, 87.0, 85.0, 84.0, 82.0, 86.0, 84.0, 88.0,
        ]
        .into();

        let monitor_payload = MonitorPayload {
            status: WorkerStatus {
                uptime_sec: 1480.0,
                remaining_before_drain_sec: 39200.0,
                cpu_pct: 34.2,
                ram: RamInfo { used_gb: 6.8, total_gb: 31.4 },
                disk_working: DiskInfo { used_gb: 4.2, free_gb: 15.3, total_gb: 19.5 },
                disk_tmp: DiskInfo { used_gb: 12.0, free_gb: 198.0, total_gb: 210.0 },
                gpus: vec![
                    GpuInfo { index: 0, name: "Tesla T4".into(), util_pct: 84.0, vram_used_mb: 12902.0, vram_total_mb: 15360.0, temp_c: 68.0 },
                    GpuInfo { index: 1, name: "Tesla T4".into(), util_pct: 79.0, vram_used_mb: 11800.0, vram_total_mb: 15360.0, temp_c: 65.0 },
                ],
                gpu_processes: vec![GpuProcess { pid: 4821, gpu_index: "0".into(), process_name: "python3".into(), used_mb: 12600 }],
                ..Default::default()
            },
            jobs: vec![JobInfo {
                job_id: "job_lora_01".into(),
                name: "train_lora".into(),
                gpu_ids: "0,1".into(),
                status: "running".into(),
                runtime_sec: 412.5,
                log_size_kb: 48.2,
                started_at: 1000.0,
                pid: Some(4821),
            }],
            latest_seq: 4,
            events: vec![],
            log_chunk: None,
            pty_live: None,
        };
        app.handle_msg(AppMsg::MonitorOk { slot: "1".into(), rtt_ms: 88.0, payload: monitor_payload });
        app.selected_job_id = "job_lora_01".into();
        app.activities.clear();
        app.push_activity(ActivityEvent {
            ts: 1.0,
            slot: "1".into(),
            caller: "mcp".into(),
            kind: "term_exec".into(),
            summary: "$ nvidia-smi --query-gpu=name,memory.used --format=csv".into(),
            detail: "Tesla T4, 12600 MiB\nTesla T4, 11500 MiB".into(),
            exit_code: Some(0),
            duration_sec: Some(0.32),
            seq: Some(1),
        });
        app.push_activity(ActivityEvent {
            ts: 2.0,
            slot: "1".into(),
            caller: "tui".into(),
            kind: "fs_sync".into(),
            summary: "fs_write -> /kaggle/working/train.py (1.4 KB)".into(),
            detail: String::new(),
            exit_code: None,
            duration_sec: None,
            seq: Some(2),
        });
        app.running_exec = Some(RunningExec {
            command: "python3 -u train.py --epochs 20".into(),
            started_at: Instant::now() - Duration::from_secs(14),
            live_output: "epoch=1 step=50/200 loss=0.8421 lr=3.0e-4\nepoch=1 step=100/200 loss=0.6194 lr=2.9e-4\nepoch=1 step=150/200 loss=0.4830 lr=2.7e-4".into(),
            worker_has_live: true,
        });
        app.raw_log_lines = [
            "[13:40:02] Rank 0 initialized: 2x Tesla T4 (30.0 GB VRAM)",
            "[13:40:08] Loaded dataset /kaggle/input/comp-data (184,200 rows)",
            "[13:42:15] Epoch 1/10 | train_loss=0.412 | val_auc=0.9142",
            "[13:44:29] Epoch 2/10 | train_loss=0.298 | val_auc=0.9318",
            "[13:46:41] Saved checkpoint -> /kaggle/working/checkpoints/ep2.pt",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        app.conduit.trigger_rx("term_exec", "1", "ok", 0.32);

        let mut terminal = Terminal::new(TestBackend::new(132, 34)).unwrap();

        // 1. Cockpit SVG
        terminal
            .draw(|f| {
                render(f, &mut app);
                render_selection_overlay(f, &app);
            })
            .unwrap();
        std::fs::write(
            assets_dir.join("cockpit.svg"),
            buffer_to_svg(terminal.backend().buffer(), "kgsh ui — F1 Cockpit (Live Shell + GPU Telemetry + Job Logs)"),
        )
        .unwrap();

        // 2. Dual Files + Preview Drawer SVG
        app.running_exec = None;
        app.apply_preset(Preset::DualFiles);
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.navigate_into(proj.clone());
            if let Some(ent) = lp.entries.iter().find(|e| e.name == "train.py").cloned() {
                lp.selected.insert(ent.path);
            }
        }
        app.handle_msg(AppMsg::FsListDone {
            side: Side::Right,
            dir: "/kaggle/working".into(),
            entries: vec![
                FsEntry { name: "checkpoints".into(), path: "/kaggle/working/checkpoints".into(), is_dir: true, size_bytes: 0, mtime: 100.0 },
                FsEntry { name: "mcp_logs".into(), path: "/kaggle/working/mcp_logs".into(), is_dir: true, size_bytes: 0, mtime: 100.0 },
                FsEntry { name: "train.py".into(), path: "/kaggle/working/train.py".into(), is_dir: false, size_bytes: 1420, mtime: 100.0 },
                FsEntry { name: "submission.csv".into(), path: "/kaggle/working/submission.csv".into(), is_dir: false, size_bytes: 49152, mtime: 100.0 },
            ],
        });
        app.handle_msg(AppMsg::FsDuDone {
            side: Side::Right,
            dir: "/kaggle/working".into(),
            res: DuResult {
                path: "/kaggle/working".into(),
                total_bytes: 4_500_000_000,
                partial: false,
                children: vec![
                    DuChild { name: "checkpoints".into(), path: "/kaggle/working/checkpoints".into(), is_dir: true, size_bytes: 3_600_000_000, pct: 80.0 },
                    DuChild { name: "mcp_logs".into(), path: "/kaggle/working/mcp_logs".into(), is_dir: true, size_bytes: 900_000_000, pct: 20.0 },
                ],
            },
        });
        app.file_clipboard = Some(FileClipboard {
            op: ClipOp::Copy,
            source: ClipSource::Remote(vec!["/kaggle/working/train.py".into()]),
        });
        app.preview_drawer = Some(PreviewDrawerState {
            target_path: "/kaggle/working/train.py".into(),
            title: "/kaggle/working/train.py".into(),
            subtitle: "PY · 1.4 KB".into(),
            lines: vec![
                "import torch".into(),
                "for epoch in range(1, 20):".into(),
                "    loss = train_step(epoch)".into(),
                "    print(f\"epoch={epoch} loss={loss:.4f}\")".into(),
            ],
            scroll: 0,
        });
        terminal
            .draw(|f| {
                render(f, &mut app);
                render_selection_overlay(f, &app);
            })
            .unwrap();
        std::fs::write(
            assets_dir.join("dual_files.svg"),
            buffer_to_svg(terminal.backend().buffer(), "kgsh ui — F2 Dual-Panel Files + DU% + Code Preview Drawer"),
        )
        .unwrap();

        // 3. Remote File Editor with clean logical selection
        app.preview_drawer = None;
        app.modal = Some(Modal::FileEditor(EditorModalState {
            is_local: false,
            is_new: false,
            path: TextField::new("/kaggle/working/train.py"),
            lines: vec![
                "import torch".into(),
                "for epoch in range(1, 20):".into(),
                "    loss = train_step(epoch)".into(),
                "    print(f\"epoch={epoch} loss={loss:.4f}\")".into(),
            ],
            cur_row: 3,
            cur_col: 42,
            sel_anchor: Some((1, 0)),
            scroll_row: 0,
            readonly: false,
            focus_path: false,
        }));
        terminal
            .draw(|f| {
                render(f, &mut app);
                render_selection_overlay(f, &app);
            })
            .unwrap();
        std::fs::write(
            assets_dir.join("editor_selection.svg"),
            buffer_to_svg(terminal.backend().buffer(), "kgsh ui — Remote File Editor (Clean Shift / Ctrl+Shift & Drag Selection)"),
        )
        .unwrap();

        // 4. Workers, Cluster & Quota Modal
        app.open_workers_modal(0);
        if let Some(Modal::Workers(ref mut w)) = app.modal {
            w.cluster_rows = vec![
                crate::app::ClusterRow {
                    slot: "1".into(),
                    mode: "live".into(),
                    accelerator: "NvidiaTeslaT4".into(),
                    state: "ONLINE".into(),
                    compute: "2xT4 24.1/30G".into(),
                    ram: "6.8/31.4G".into(),
                    jobs: "1 run".into(),
                    drain: "10h53m".into(),
                    rtt: "88ms".into(),
                },
                crate::app::ClusterRow {
                    slot: "2".into(),
                    mode: "live".into(),
                    accelerator: "NvidiaTeslaT4".into(),
                    state: "ONLINE".into(),
                    compute: "2xT4 14.0/30G".into(),
                    ram: "4.2/31.4G".into(),
                    jobs: "1 run".into(),
                    drain: "11h18m".into(),
                    rtt: "92ms".into(),
                },
            ];
        }
        terminal
            .draw(|f| {
                render(f, &mut app);
                render_selection_overlay(f, &app);
            })
            .unwrap();
        std::fs::write(
            assets_dir.join("workers_quota.svg"),
            buffer_to_svg(terminal.backend().buffer(), "kgsh ui — Ctrl+W / F5 Multi-Slot Workers & Weekly Quota"),
        )
        .unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
