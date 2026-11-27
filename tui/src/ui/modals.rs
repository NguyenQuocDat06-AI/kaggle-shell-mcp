use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};
use ratatui::Frame;

use super::*;

fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
}

fn modal_box(f: &mut Frame, rects: &mut UiRects, area: Rect, title: &str) -> Rect {
    rects.modal_rect = area;
    f.render_widget(Clear, area);
    let blk = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(BORDER_ACTIVE))
        .style(Style::default().bg(BG).fg(TEXT));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    inner
}

pub(super) fn render_modal(f: &mut Frame, app: &mut App, screen: Rect) {
    if let Some(Modal::FileEditor(ref mut m)) = app.modal {
        let area = centered_rect(86, 82, screen);
        let view_h = area.height.saturating_sub(4).max(1) as usize;
        if !app.editor_mouse_selecting {
            m.ensure_scroll(view_h);
        }
    }
    let Some(modal) = app.modal.clone() else { return; };
    let act_items = app.action_menu_items();
    let act_pane = app.active_pane().clone();
    match modal {
        Modal::Workers(m) => render_workers_modal(f, app, &m, centered_rect(86, 78, screen)),
        Modal::SwapPane(idx) => render_list_modal(
            f,
            &mut app.rects,
            "Swap Active Panel Content (m)",
            SWAP_PANE_OPTIONS.iter().map(|(_, l)| *l).collect(),
            idx,
            centered_rect(54, 52, screen),
        ),
        Modal::ActionMenu(idx) => render_list_modal(
            f,
            &mut app.rects,
            "Context Actions (a)",
            act_items.into_iter().map(|(_, l)| l).collect(),
            idx,
            centered_rect(60, 66, screen),
        ),
        Modal::SpawnJob(m) => render_spawn_job_modal(f, &mut app.rects, &m, centered_rect(66, 38, screen)),
        Modal::Sync(m) => render_sync_modal(f, &mut app.rects, &m, centered_rect(64, 42, screen)),
        Modal::FileEditor(m) => render_editor_modal(f, &mut app.rects, &m, centered_rect(86, 82, screen)),
        Modal::PathAction(m) => render_path_action_modal(f, &mut app.rects, &m, centered_rect(72, 38, screen)),
        Modal::SmartInspect(m) => render_smart_inspect_modal(f, &mut app.rects, &m, centered_rect(90, 84, screen)),
        Modal::PublishDataset(m) => render_publish_modal(f, &mut app.rects, &m, centered_rect(70, 56, screen)),
        Modal::SubmitCompetition(m) => render_submit_modal(f, &mut app.rects, &m, centered_rect(68, 40, screen)),
        Modal::CommandPalette(m) => render_palette_modal(f, &mut app.rects, &m, centered_rect(72, 68, screen)),
        Modal::Help => render_help_modal(f, &mut app.rects, &act_pane, centered_rect(74, 72, screen)),
    }
}

fn render_list_modal(
    f: &mut Frame,
    rects: &mut UiRects,
    title: &str,
    labels: Vec<&str>,
    selected: usize,
    area: Rect,
) {
    let inner = modal_box(f, rects, area, title);
    rects.modal_list_rect = inner;
    let lines: Vec<Line> = labels
        .into_iter()
        .enumerate()
        .map(|(i, lbl)| {
            let st = if i == selected {
                Style::default().bg(SEL_BG).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            Line::from(Span::styled(format!("  {}", lbl), st))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_workers_modal(f: &mut Frame, app: &mut App, m: &WorkersModalState, area: Rect) {
    let inner = modal_box(f, &mut app.rects, area, "Workers, Cluster & Quota (Ctrl+W / F5)");
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(4)])
        .split(inner);

    let mut tx = chunks[0].x + 1;
    let ty = chunks[0].y;
    let tmax = chunks[0].x + chunks[0].width;
    let mut tab_spans = vec![Span::raw(" ")];
    text_btn(
        &mut app.rects,
        &mut tab_spans,
        &mut tx,
        ty,
        tmax,
        "workers-tab-0",
        "1 Cluster & Quota",
        if m.tab == 0 {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(MUTED)
        },
    );
    text_btn(
        &mut app.rects,
        &mut tab_spans,
        &mut tx,
        ty,
        tmax,
        "workers-tab-1",
        "2 Boot & Profiles",
        if m.tab == 1 {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(MUTED)
        },
    );
    tab_spans.push(Span::styled("(Tab switches · Click tab)", Style::default().fg(MUTED)));
    f.render_widget(Paragraph::new(Line::from(tab_spans)), chunks[0]);

    if m.tab == 0 {
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(4), Constraint::Length(1)])
            .split(chunks[1]);

        let q_lines = if let Some(ref q) = app.quota {
            let mut g = vec![Span::styled(format!(" {} · GPU ", q.username), Style::default().fg(TEXT))];
            g.extend(bar_spans(q.gpu.used_pct, 20));
            g.push(Span::raw(format!(" {:.2}h/{:.0}h", q.gpu.remaining_hours, q.gpu.total_hours)));
            let mut t = vec![Span::styled(" TPU ", Style::default().fg(TEXT))];
            t.extend(bar_spans(q.tpu.used_pct, 20));
            t.push(Span::raw(format!(" {:.2}h/{:.0}h  (Reset: {})", q.tpu.remaining_hours, q.tpu.total_hours, q.quota_refresh_time)));
            vec![Line::raw(""), Line::from(g), Line::from(t)]
        } else {
            vec![Line::raw(""), Line::from(Span::styled(" Loading weekly quota...", Style::default().fg(MUTED)))]
        };
        f.render_widget(Paragraph::new(q_lines), v[0]);

        app.rects.modal_list_rect = v[1];
        let hdr = Row::new(["Slot", "Mode", "Accelerator", "State", "Compute / VRAM", "RAM", "Jobs", "Drain", "RTT"])
            .style(Style::default().fg(TEXT).bg(SURFACE).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = m
            .cluster_rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let st = if i == m.cluster_selected { Style::default().bg(SEL_BG).fg(Color::White) } else { Style::default() };
                Row::new([
                    Cell::from(if r.slot == app.current_slot { format!("* {}", r.slot) } else { r.slot.clone() }),
                    Cell::from(r.mode.clone()),
                    Cell::from(r.accelerator.clone()),
                    Cell::from(r.state.clone()),
                    Cell::from(r.compute.clone()),
                    Cell::from(r.ram.clone()),
                    Cell::from(r.jobs.clone()),
                    Cell::from(r.drain.clone()),
                    Cell::from(r.rtt.clone()),
                ])
                .style(st)
            })
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(7), Constraint::Length(6), Constraint::Length(14),
                    Constraint::Length(9), Constraint::Percentage(22), Constraint::Length(10),
                    Constraint::Length(10), Constraint::Length(8), Constraint::Length(8),
                ],
            )
            .header(hdr),
            v[1],
        );
        f.render_widget(
            Paragraph::new(" Enter Switch Slot   r Refresh   x Stop Slot   Tab Boot New   Esc Close")
                .style(Style::default().fg(MUTED)),
            v[2],
        );
    } else {
        let prof_lbl = if m.profile_idx == 0 {
            "(none — ←/→ select saved profile)".into()
        } else {
            m.profiles.get(m.profile_idx - 1).map(|p| format!("{} ({})", p.name, p.accelerator)).unwrap_or_default()
        };
        let lines = vec![
            Line::raw(""),
            field_line("Saved Profile (←/→)", &prof_lbl, m.field_idx == 0),
            field_line_tf("Save As Name (^S/^D)", &m.profile_name, m.field_idx == 1),
            field_line("Accelerator (←/→)", ACCELERATORS[m.acc_idx].1, m.field_idx == 2),
            field_line_tf("Target Slot(s)", &m.slots, m.field_idx == 3),
            field_line_tf("Datasets (csv)", &m.datasets, m.field_idx == 4),
            field_line_tf("Models (csv)", &m.models, m.field_idx == 5),
            field_line_tf("Competition Slug", &m.competition, m.field_idx == 6),
            field_line_tf("Idle AutoStop (min)", &m.idle_min, m.field_idx == 7),
            Line::from(format!("Chain Previous Output: [{}] (Space on Accelerator)", if m.chain_prev { "x" } else { " " })),
            Line::raw(""),
            Line::from(Span::styled(" ↑↓ Field   Tab Switch Tab   ^S Save Profile   ^D Del   Enter Launch   Esc Close", Style::default().fg(MUTED))),
        ];
        f.render_widget(Paragraph::new(lines), chunks[1]);
    }
}

fn render_spawn_job_modal(f: &mut Frame, rects: &mut UiRects, m: &SpawnJobModalState, area: Rect) {
    let inner = modal_box(f, rects, area, "Spawn Background Job (n)");
    let lines = vec![
        field_line_tf("Command", &m.command, m.field_idx == 0),
        field_line_tf("GPU IDs (e.g. 0,1)", &m.gpu_ids, m.field_idx == 1),
        field_line_tf("Job Name (opt)", &m.name, m.field_idx == 2),
        Line::raw(""),
        Line::from(Span::styled(" Tab Next Field   Enter Start Job   Esc Cancel", Style::default().fg(MUTED))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_sync_modal(f: &mut Frame, rects: &mut UiRects, m: &SyncModalState, area: Rect) {
    let inner = modal_box(f, rects, area, "Sync Files Push / Pull");
    let dir_txt = if m.is_push { "PUSH (Local -> Kaggle)" } else { "PULL (Kaggle -> Local)" };
    let lines = vec![
        field_line("Direction (←/→)", dir_txt, m.field_idx == 0),
        field_line_tf("Local Path", &m.local_path, m.field_idx == 1),
        field_line_tf("Remote Path", &m.remote_path, m.field_idx == 2),
        Line::raw(""),
        Line::from(Span::styled(" Enter Transfer   Esc Cancel", Style::default().fg(MUTED))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_editor_modal(f: &mut Frame, rects: &mut UiRects, m: &EditorModalState, area: Rect) {
    let scope = if m.is_local { "Local" } else { "Remote" };
    let title = if m.is_new {
        format!("New {} File", scope)
    } else {
        format!("{} File Editor", scope)
    };
    let inner = modal_box(f, rects, area, &title);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
        .split(inner);
    rects.editor_path_rect = chunks[0];
    rects.editor_body_rect = chunks[1];
    rects.editor_code_rect = Rect::new(
        chunks[1].x.saturating_add(7),
        chunks[1].y,
        chunks[1].width.saturating_sub(7),
        chunks[1].height,
    );
    f.render_widget(Paragraph::new(field_line_tf("Path", &m.path, m.focus_path)), chunks[0]);
    let h = chunks[1].height as usize;
    let start = m.scroll_row;
    let sel_range = m.selection_range();
    let sel_st = Style::default().bg(ACCENT).fg(Color::Black);
    let lines: Vec<Line> = m
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(h)
        .map(|(idx, l)| {
            let num = Span::styled(format!("{:>4} │ ", idx + 1), Style::default().fg(MUTED));
            let base_st = if !m.focus_path && idx == m.cur_row {
                Style::default().bg(SURFACE).fg(Color::White)
            } else {
                Style::default().fg(TEXT)
            };
            let mut spans = vec![num];
            if let Some(((sr, sc), (er, ec))) = sel_range {
                if idx >= sr && idx <= er {
                    let len = l.chars().count();
                    let s = if idx == sr { sc } else { 0 }.min(len);
                    let e = if idx == er { ec } else { len }.min(len);
                    if s > 0 {
                        spans.push(Span::styled(char_slice(l, 0, s), base_st));
                    }
                    if e > s {
                        spans.push(Span::styled(char_slice(l, s, e), sel_st));
                    }
                    if idx < er && e == len {
                        spans.push(Span::styled(" ", sel_st));
                    }
                    if e < len {
                        spans.push(Span::styled(char_slice(l, e, len), base_st));
                    }
                    return Line::from(spans);
                }
            }
            spans.push(Span::styled(l.clone(), base_st));
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), chunks[1]);

    if m.focus_path {
        let cx = chunks[0].x.saturating_add(23 + m.path.cursor as u16).min(chunks[0].x + chunks[0].width.saturating_sub(1));
        f.set_cursor_position((cx, chunks[0].y));
    } else if !m.readonly && m.cur_row >= start && (m.cur_row - start) < h {
        let cy = chunks[1].y + (m.cur_row - start) as u16;
        let cx = chunks[1].x.saturating_add(7 + m.cur_col as u16).min(chunks[1].x + chunks[1].width.saturating_sub(1));
        f.set_cursor_position((cx, cy));
    }

    let hint = if m.readonly {
        " Read-Only · Shift/Drag Select · ^A All · ^C Copy · Esc Close"
    } else {
        " Shift/Drag Select · ^A All · ^C Copy · ^X Cut · ^V Paste · ^S Save · Esc Close"
    };
    f.render_widget(Paragraph::new(hint).style(Style::default().fg(MUTED)), chunks[2]);
}

fn render_path_action_modal(f: &mut Frame, rects: &mut UiRects, m: &PathActionModalState, area: Rect) {
    let scope = if m.is_local { "Local" } else { "Remote" };
    let (title, op_lbl) = match m.kind {
        PathActionKind::Mkdir => (format!("New {} Directory (N)", scope), "CREATE DIRECTORY"),
        PathActionKind::Copy => (format!("Duplicate {} Item", scope), "DUPLICATE COPY"),
        PathActionKind::Rename => (format!("Rename / Move {} Item (R)", scope), "RENAME / MOVE"),
    };
    let inner = modal_box(f, rects, area, &title);
    let lines = vec![
        field_line("Operation", op_lbl, false),
        field_line("Source / Base", &m.src_path, false),
        field_line_tf("Destination", &m.dst_path, true),
        Line::raw(""),
        Line::from(Span::styled(
            if m.kind == PathActionKind::Mkdir || m.readonly_src {
                " Enter Execute   Esc Cancel"
            } else {
                " Tab Toggle Rename/Copy   Enter Execute   Esc Cancel"
            },
            Style::default().fg(MUTED),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    if inner.height >= 3 {
        let cx = inner.x.saturating_add(23 + m.dst_path.cursor as u16).min(inner.x + inner.width.saturating_sub(1));
        f.set_cursor_position((cx, inner.y + 2));
    }
}

fn render_smart_inspect_modal(f: &mut Frame, rects: &mut UiRects, m: &SmartInspectModalState, area: Rect) {
    let inner = modal_box(f, rects, area, &format!("Inspect: {}", m.info.path));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
        .split(inner);
    rects.inspect_content_rect = chunks[1];
    f.render_widget(
        Paragraph::new(format!(" Format: {} · Size: {}", m.info.format, fmt_bytes(m.info.size_bytes))).style(Style::default().fg(MUTED)),
        chunks[0],
    );
    if m.info.kind == "table" {
        let cols = if m.info.columns.is_empty() { vec!["col_1".into()] } else { m.info.columns.clone() };
        let n_cols = cols.len().min(10);
        let hdr = Row::new(cols.iter().take(n_cols).cloned().collect::<Vec<_>>()).style(Style::default().fg(TEXT).bg(SURFACE).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = m
            .info
            .rows
            .iter()
            .skip(m.scroll)
            .map(|r| Row::new((0..n_cols).map(|i| Cell::from(r.get(i).cloned().unwrap_or_default())).collect::<Vec<_>>()))
            .collect();
        f.render_widget(Table::new(rows, vec![Constraint::Ratio(1, n_cols as u32); n_cols]).header(hdr), chunks[1]);
    } else {
        let mut lines = Vec::new();
        for e in &m.info.entries {
            lines.push(Line::from(format!("{} {:>9} {}", if e.is_dir { "[DIR]" } else { "[FILE]" }, fmt_bytes(e.size_bytes), e.name)));
        }
        for t in &m.info.tensors {
            lines.push(Line::from(format!("{:<6} {:?} {}", t.dtype, t.shape, t.name)));
        }
        if lines.is_empty() {
            lines.push(Line::from(m.info.note.clone()));
        }
        f.render_widget(Paragraph::new(lines).scroll((m.scroll as u16, 0)), chunks[1]);
    }
    let mut keys = String::from(" ↑↓ Scroll   p Pull");
    if m.info.kind == "archive" { keys.push_str("   e Extract"); }
    if m.info.path.to_ascii_lowercase().ends_with(".csv") { keys.push_str("   s Submit CSV"); }
    keys.push_str("   Esc Close");
    f.render_widget(Paragraph::new(keys).style(Style::default().fg(MUTED)), chunks[2]);
}

fn render_publish_modal(f: &mut Frame, rects: &mut UiRects, m: &PublishDatasetModalState, area: Rect) {
    let inner = modal_box(f, rects, area, "Publish Dir as Kaggle Dataset");
    let lines = vec![
        field_line_tf("Source Dir", &m.source_dir, m.field_idx == 0),
        field_line_tf("Slug", &m.slug, m.field_idx == 1),
        field_line_tf("Title", &m.title, m.field_idx == 2),
        field_line_tf("Notes", &m.notes, m.field_idx == 3),
        Line::from(format!("Public: [{}] (Space)", if m.public { "x" } else { " " })),
        Line::from(format!("Stage for Boot: [{}] (Space)", if m.stage_after { "x" } else { " " })),
        Line::raw(""),
        Line::from(Span::styled(" Enter Publish   Esc Cancel", Style::default().fg(MUTED))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_submit_modal(f: &mut Frame, rects: &mut UiRects, m: &SubmitCompModalState, area: Rect) {
    let inner = modal_box(f, rects, area, "Submit Prediction CSV");
    let lines = vec![
        field_line("CSV File", &m.path, false),
        field_line_tf("Competition Slug", &m.competition, m.field_idx == 0),
        field_line_tf("Message", &m.message, m.field_idx == 1),
        Line::raw(""),
        Line::from(Span::styled(" Enter Submit   Esc Cancel", Style::default().fg(MUTED))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_palette_modal(f: &mut Frame, rects: &mut UiRects, m: &PaletteModalState, area: Rect) {
    let inner = modal_box(f, rects, area, "Command Palette (Ctrl+K)");
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(inner);
    f.render_widget(Paragraph::new(field_line_tf("Search", &m.query, true)), chunks[0]);
    rects.modal_list_rect = chunks[1];
    let items = filtered_palette_items(&m.query.value);
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, (title, help, _))| {
            let st = if i == m.selected { Style::default().bg(SEL_BG).fg(Color::White).add_modifier(Modifier::BOLD) } else { Style::default().fg(TEXT) };
            Line::from(vec![Span::styled(format!("{}  ", title), st), Span::styled(*help, Style::default().fg(MUTED))])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), chunks[1]);
}

fn render_help_modal(f: &mut Frame, rects: &mut UiRects, active: &PaneKind, area: Rect) {
    let inner = modal_box(f, rects, area, "Keyboard Shortcuts");
    let pane_tips = match active {
        PaneKind::Shell => vec![
            "Shell Panel:",
            "  i / Enter        Enter INPUT mode (:run <cmd> / Alt+Enter streams loops live)",
            "  Ctrl+C           Copy selected text, or interrupt running command",
            "  y / Y            Copy latest output / full shell stream",
            "  ← / → / Tab      Switch Left <-> Right pane in NAV mode",
        ],
        PaneKind::LocalFiles(_) | PaneKind::RemoteFiles(_) => vec![
            "File Manager (Local & Remote):",
            "  ← / h / Bksp     Go up to parent directory (restores cursor)",
            "  → / l            Enter dir · Preview file (2x opens Editor)",
            "  Enter / Shift+X  Open Editor/Inspect · Run .py/.sh with Live Logs",
            "  Shift+←/→ / Tab  Switch Left <-> Right panel anytime",
            "  Space / A        Toggle select (+advance) / Select All",
            "  t / c / x / p    Transfer opposite / Copy / Cut / Paste",
            "  v / /            Auto-follow Preview Drawer / Filter or Find",
            "  n / N / R / d    New File / New Dir / Rename / Delete (2x)",
            "  s / S / . / u    Cycle Sort / Flip Asc-Desc / Dotfiles / DU%",
        ],
        PaneKind::JobsAndGpu => vec![
            "Jobs & GPU Panel:",
            "  ↑ / ↓ / ← / →    Select job / Switch Left <-> Right panel",
            "  n / x            Spawn job / Kill selected job",
            "  [ / ] / Shift+K  Select GPU PID / Kill GPU PID (2x)",
        ],
        PaneKind::LiveLogs => vec![
            "Live Logs:",
            "  Space / / / y    Pause auto-scroll / Regex Filter / Copy logs",
            "  ← / → / Tab      Switch Left <-> Right panel",
        ],
        PaneKind::HubList | PaneKind::HubDetail => vec![
            "Kaggle Hub:",
            "  ←/→ / /          Switch category / Search",
            "  Enter / s / d    Inspect / Stage for Boot / Download locally",
            "  l / Shift+D      Kernel logs / Delete kernel (2x)",
        ],
    };

    let mut lines = vec![
        Line::from(Span::styled("Global Shortcuts:", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from("  F1..F4           Presets: Cockpit | Files | Jobs | Hub"),
        Line::from("  Shift/Ctrl+Shift Select text in Editor, Shell, Logs & Inputs (^A/^C/^X)"),
        Line::from("  Mouse Drag / F8  Drag inside content to copy clean text · F8 native select"),
        Line::from("  Ctrl+W / F5      Workers, Cluster, Quota & Boot Profiles"),
        Line::from("  Tab / Alt+←/→    Switch Focus LEFT <-> RIGHT"),
        Line::from("  z / m / a / ^K   Zoom 100% / Swap Content / Actions / Palette"),
        Line::raw(""),
    ];
    for (idx, ln) in pane_tips.into_iter().enumerate() {
        let st = if idx == 0 { Style::default().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::default().fg(TEXT) };
        lines.push(Line::from(Span::styled(ln, st)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

