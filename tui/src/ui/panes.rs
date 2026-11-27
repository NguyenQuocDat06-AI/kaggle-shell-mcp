use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, Wrap,
};
use ratatui::Frame;

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneTag {
    Shell,
    Local,
    Remote,
    Jobs,
    Logs,
    HubList,
    HubDetail,
}

pub(super) fn render_pane(f: &mut Frame, app: &mut App, side: Side, area: Rect) {
    let is_active = app.active_side == side;
    let border_col = if is_active { BORDER_ACTIVE } else { BORDER };
    let dot = if is_active { "●" } else { "○" };

    let opp_is_remote = matches!(app.pane(side.opposite()), PaneKind::RemoteFiles(_));
    let opp_is_local = matches!(app.pane(side.opposite()), PaneKind::LocalFiles(_));

    let (pane_tag, top_left, top_right, ro_badge, bot_left, bot_right) = match app.pane(side) {
        PaneKind::Shell => (
            PaneTag::Shell,
            format!(" {} Shell ", dot),
            if app.zoomed && is_active { " [ZOOM] ".into() } else { String::new() },
            false,
            Line::default(),
            Line::default(),
        ),
        PaneKind::LocalFiles(lp) => {
            let folder = lp
                .dir
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("Local");
            let sel_bytes: u64 = lp
                .entries
                .iter()
                .filter(|e| lp.selected.contains(&e.path))
                .map(|e| e.size_bytes)
                .sum();
            let bl = if !lp.selected.is_empty() {
                Line::from(Span::styled(
                    format!(" {} selected · {} ", lp.selected.len(), fmt_bytes(sel_bytes)),
                    Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    format!(" {} items · {} ", lp.total_files + lp.total_dirs, fmt_bytes(lp.total_bytes)),
                    Style::default().fg(MUTED),
                ))
            };
            let br = Line::from(Span::styled(
                if lp.entries.is_empty() {
                    " 0/0 ".into()
                } else {
                    format!(" {}/{} ", lp.cursor + 1, lp.entries.len())
                },
                Style::default().fg(if is_active { TEXT } else { MUTED }),
            ));
            (
                PaneTag::Local,
                format!(" {} Local ({}) ", dot, truncate_end(folder, 20)),
                if app.zoomed && is_active { " [ZOOM] ".into() } else { String::new() },
                false,
                bl,
                br,
            )
        }
        PaneKind::RemoteFiles(rp) => {
            let ro = rp.dir.starts_with("/kaggle/input");
            let sel_bytes: u64 = rp
                .entries
                .iter()
                .filter(|e| rp.selected.contains(&e.path))
                .map(|e| e.size_bytes)
                .sum();
            let bl = if !rp.selected.is_empty() {
                Line::from(Span::styled(
                    format!(" {} selected · {} ", rp.selected.len(), fmt_bytes(sel_bytes)),
                    Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    format!(" {} items · {} ", rp.total_files + rp.total_dirs, fmt_bytes(rp.total_bytes)),
                    Style::default().fg(MUTED),
                ))
            };
            let br = Line::from(Span::styled(
                if rp.entries.is_empty() {
                    " 0/0 ".into()
                } else {
                    format!(" {}/{} ", rp.cursor + 1, rp.entries.len())
                },
                Style::default().fg(if is_active { TEXT } else { MUTED }),
            ));
            (
                PaneTag::Remote,
                format!(" {} Remote (slot={}) ", dot, app.current_slot),
                if ro {
                    if app.zoomed && is_active { " [RO] [ZOOM] ".into() } else { " [RO] ".into() }
                } else if app.zoomed && is_active {
                    " [ZOOM] ".into()
                } else {
                    String::new()
                },
                ro,
                bl,
                br,
            )
        }
        PaneKind::JobsAndGpu => (
            PaneTag::Jobs,
            format!(" {} Jobs & GPU ", dot),
            if app.zoomed && is_active {
                format!(" {} jobs [ZOOM] ", app.jobs.len())
            } else {
                format!(" {} jobs ", app.jobs.len())
            },
            false,
            Line::default(),
            Line::default(),
        ),
        PaneKind::LiveLogs => (
            PaneTag::Logs,
            format!(" {} Logs ", dot),
            if app.zoomed && is_active {
                format!(" {} lines [ZOOM] ", app.raw_log_lines.len())
            } else {
                format!(" {} lines ", app.raw_log_lines.len())
            },
            false,
            Line::default(),
            Line::default(),
        ),
        PaneKind::HubList => (
            PaneTag::HubList,
            format!(" {} Hub ", dot),
            if app.zoomed && is_active { " [ZOOM] ".into() } else { String::new() },
            false,
            Line::default(),
            Line::default(),
        ),
        PaneKind::HubDetail => (
            PaneTag::HubDetail,
            format!(" {} Inspector ", dot),
            if app.zoomed && is_active { " [ZOOM] ".into() } else { String::new() },
            false,
            Line::default(),
            Line::default(),
        ),
    };

    let mut blk = Block::default()
        .title_top(
            Line::from(Span::styled(
                top_left,
                Style::default()
                    .fg(if is_active { TEXT } else { MUTED })
                    .add_modifier(if is_active { Modifier::BOLD } else { Modifier::empty() }),
            ))
            .left_aligned(),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_col))
        .style(Style::default().bg(BG));

    if !top_right.is_empty() {
        blk = blk.title_top(
            Line::from(Span::styled(
                top_right,
                Style::default().fg(if ro_badge { WARN } else { MUTED }),
            ))
            .right_aligned(),
        );
    }

    if matches!(pane_tag, PaneTag::Local | PaneTag::Remote) {
        blk = blk
            .title_bottom(bot_left.left_aligned())
            .title_bottom(bot_right.right_aligned());
    }

    let inner = blk.inner(area);
    f.render_widget(blk, area);
    if inner.height < 3 || inner.width < 14 {
        return;
    }

    match pane_tag {
        PaneTag::Local => render_local_body(f, app, side, is_active, opp_is_remote, inner),
        PaneTag::Remote => render_remote_body(f, app, side, is_active, opp_is_remote, opp_is_local, inner),
        PaneTag::Shell => render_shell_body(f, app, is_active, inner),
        PaneTag::Jobs => render_jobs_body(f, app, side, is_active, inner),
        PaneTag::Logs => render_logs_body(f, app, inner),
        PaneTag::HubList => render_hub_list_body(f, app, side, is_active, inner),
        PaneTag::HubDetail => render_hub_detail_body(f, app, inner),
    }
}

fn render_local_body(
    f: &mut Frame,
    app: &mut App,
    side: Side,
    is_active: bool,
    _opp_is_remote: bool,
    area: Rect,
) {
    let side_tag = if side == Side::Left { "left" } else { "right" };
    let filtering = matches!(app.input_focus, InputFocus::LocalFilter(s) if s == side);

    let (dir, filter_tf, sort, sort_desc) = match app.pane(side) {
        PaneKind::LocalFiles(lp) => (
            lp.dir.clone(),
            lp.filter_input.clone(),
            lp.sort,
            lp.sort_desc,
        ),
        _ => return,
    };
    let show_filter_bar = filtering || !filter_tf.value.is_empty();

    let mut constraints = vec![Constraint::Length(1)];
    if show_filter_bar {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // column header
    constraints.push(Constraint::Min(1));    // list + scrollbar
    let chunks = Layout::default().direction(Direction::Vertical).constraints(constraints).split(area);

    // Row 1: Quick jumps + clickable breadcrumb + quiet swap/more on right
    let mut bx = chunks[0].x;
    let by = chunks[0].y;
    let bmax = chunks[0].x + chunks[0].width;
    let reserve_right = 14u16; // " m:swap  a:more"
    let crumb_max = bmax.saturating_sub(reserve_right);
    let mut b_spans = vec![Span::raw(" ")];
    bx = bx.saturating_add(1);
    text_btn(&mut app.rects, &mut b_spans, &mut bx, by, crumb_max, &format!("local-jump-{}-home", side_tag), "~", Style::default().fg(MUTED));
    text_btn(&mut app.rects, &mut b_spans, &mut bx, by, crumb_max, &format!("local-jump-{}-cwd", side_tag), ".", Style::default().fg(MUTED));
    text_btn(&mut app.rects, &mut b_spans, &mut bx, by, crumb_max, &format!("local-jump-{}-dl", side_tag), "dl", Style::default().fg(MUTED));

    if bx + 4 < crumb_max {
        b_spans.push(Span::styled("│ ", Style::default().fg(BORDER)));
        bx = bx.saturating_add(2);
    }

    let mut segs: Vec<(String, String)> = Vec::new();
    let mut acc = std::path::PathBuf::new();
    for comp in dir.components() {
        acc.push(comp);
        let lbl = comp.as_os_str().to_string_lossy().trim_end_matches(['\\', '/']).to_string();
        if !lbl.is_empty() {
            segs.push((lbl, acc.to_string_lossy().to_string()));
        }
    }
    let avail = crumb_max.saturating_sub(bx) as usize;
    let start_idx = if segs.len() > 3 && avail < 40 {
        b_spans.push(Span::styled("…›", Style::default().fg(MUTED)));
        bx = bx.saturating_add(2);
        segs.len() - 3
    } else {
        0
    };
    for (idx, (lbl, full)) in segs.into_iter().enumerate().skip(start_idx) {
        if bx + 4 > crumb_max {
            break;
        }
        if idx > start_idx {
            b_spans.push(Span::styled("›", Style::default().fg(MUTED)));
            bx = bx.saturating_add(1);
        }
        let disp = truncate_middle(&lbl, (crumb_max.saturating_sub(bx) as usize).min(18));
        let w = disp.chars().count() as u16;
        if bx + w <= crumb_max {
            app.rects.buttons.push((Rect::new(bx, by, w, 1), format!("crumb-{}:{}", side_tag, full)));
            let is_last = full == dir.to_string_lossy();
            b_spans.push(Span::styled(
                disp,
                Style::default()
                    .fg(if is_last { TEXT } else { MUTED })
                    .add_modifier(if is_last { Modifier::BOLD } else { Modifier::empty() }),
            ));
            bx = bx.saturating_add(w);
        }
    }
    if bx < bmax.saturating_sub(reserve_right) {
        let gap = (bmax.saturating_sub(reserve_right)).saturating_sub(bx);
        b_spans.push(Span::raw(" ".repeat(gap as usize)));
        bx = bx.saturating_add(gap);
        text_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, &format!("swap-{}", side_tag), "m:swap", Style::default().fg(MUTED));
        text_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, &format!("act-{}", side_tag), "a:more", Style::default().fg(MUTED));
    }
    f.render_widget(Paragraph::new(Line::from(b_spans)).style(Style::default().bg(BG)), chunks[0]);

    // Row 2 (conditional): Filter bar
    let mut next_idx = 1;
    if show_filter_bar {
        let f_area = chunks[next_idx];
        next_idx += 1;
        let mut f_spans = vec![Span::styled(
            " / ",
            Style::default().fg(if filtering { ACCENT } else { MUTED }).add_modifier(Modifier::BOLD),
        )];
        f_spans.extend(text_field_spans(&filter_tf, Style::default().fg(TEXT)));
        f_spans.push(Span::styled("  (Enter apply · Esc clear)", Style::default().fg(MUTED)));
        f.render_widget(Paragraph::new(Line::from(f_spans)).style(Style::default().bg(BG)), f_area);
        if filtering {
            let cx = f_area.x.saturating_add(3 + filter_tf.cursor as u16).min(f_area.x + f_area.width.saturating_sub(1));
            f.set_cursor_position((cx, f_area.y));
        }
    }

    // Row 3: Column Headers
    let col_area = chunks[next_idx];
    let list_outer = chunks[next_idx + 1];
    let body_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(list_outer);
    let list_area = body_split[0];
    let sb_area = body_split[1];

    let size_w: usize = 9;
    let time_w: usize = 7;
    let name_w = (list_area.width as usize).saturating_sub(2 + size_w + time_w + 1).max(8);

    render_column_headers(f, &mut app.rects, side_tag, sort, sort_desc, col_area, name_w, size_w, "Time", time_w);

    set_list_rect(&mut app.rects, side, list_area);
    let list_h = list_area.height as usize;
    if let PaneKind::LocalFiles(ref mut lp) = app.pane_mut(side) {
        ensure_scroll(lp.cursor, &mut lp.scroll, list_h);
    }

    let now_sec = current_unix_sec();
    let (lines, total_len, scroll_pos) = match app.pane(side) {
        PaneKind::LocalFiles(lp) => {
            if lp.entries.is_empty() {
                (vec![Line::from(Span::styled("  (empty folder)", Style::default().fg(MUTED)))], 0, 0)
            } else {
                let ls: Vec<Line> = lp
                    .entries
                    .iter()
                    .enumerate()
                    .skip(lp.scroll)
                    .take(list_h)
                    .map(|(i, e)| {
                        let is_cursor = i == lp.cursor;
                        let is_marked = lp.selected.contains(&e.path);
                        let (badge, b_col) = file_badge(&e.name, e.is_dir);
                        let raw_name = if e.is_dir && e.name != ".." {
                            format!("{}/", e.name)
                        } else {
                            e.name.clone()
                        };
                        let disp = truncate_end(&raw_name, name_w);
                        let pad = name_w.saturating_sub(disp.chars().count());
                        let sz = if e.name == ".." {
                            "<UP>".into()
                        } else if e.is_dir {
                            "<DIR>".into()
                        } else {
                            fmt_bytes(e.size_bytes)
                        };
                        let tm = if e.name == ".." { "--".into() } else { fmt_mtime(e.mtime, now_sec) };

                        let row_bg = if is_active && is_cursor {
                            Some(SEL_BG)
                        } else if !is_active && is_cursor {
                            Some(SEL_GHOST_BG)
                        } else if is_marked {
                            Some(MARKED_BG)
                        } else {
                            None
                        };
                        let mark_span = if is_marked {
                            Span::styled("▌ ", Style::default().fg(WARN).add_modifier(Modifier::BOLD))
                        } else {
                            Span::raw("  ")
                        };
                        let name_fg = if is_active && is_cursor {
                            Color::White
                        } else if e.name.starts_with('.') && e.name != ".." {
                            MUTED
                        } else if e.is_dir && e.name != ".." {
                            ACCENT
                        } else {
                            TEXT
                        };
                        let mut name_st = Style::default().fg(name_fg);
                        if (is_active && is_cursor) || (e.is_dir && e.name != "..") {
                            name_st = name_st.add_modifier(Modifier::BOLD);
                        }
                        let meta_st = Style::default().fg(if is_active && is_cursor { TEXT } else { MUTED });
                        let badge_st = Style::default().fg(if is_active && is_cursor { Color::White } else { b_col });
                        let mut line = Line::from(vec![
                            mark_span,
                            Span::styled(format!("{} ", badge), badge_st),
                            Span::styled(format!("{}{}", disp, " ".repeat(pad)), name_st),
                            Span::styled(format!("{:>width$}", sz, width = size_w), meta_st),
                            Span::styled(format!(" {:>width$}", tm, width = time_w - 1), meta_st),
                        ]);
                        if let Some(bg) = row_bg {
                            line = line.style(Style::default().bg(bg));
                        }
                        line
                    })
                    .collect();
                (ls, lp.entries.len(), lp.scroll)
            }
        }
        _ => return,
    };

    f.render_widget(Paragraph::new(lines), list_area);
    let mut sb_state = ScrollbarState::new(total_len.max(1)).position(scroll_pos);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(Style::default().fg(BORDER)),
        sb_area,
        &mut sb_state,
    );
}

fn render_remote_body(
    f: &mut Frame,
    app: &mut App,
    side: Side,
    is_active: bool,
    _opp_is_remote: bool,
    _opp_is_local: bool,
    area: Rect,
) {
    let side_tag = if side == Side::Left { "left" } else { "right" };
    let finding = matches!(app.input_focus, InputFocus::RemoteFind(s) if s == side);

    let (
        dir,
        find_tf,
        find_active,
        du_banner,
        du_warn,
        has_du_cols,
        sort,
        sort_desc,
        loading,
    ) = match app.pane(side) {
        PaneKind::RemoteFiles(rp) => (
            rp.dir.clone(),
            rp.find_input.clone(),
            rp.find_active,
            rp.du_banner.clone(),
            rp.du_warn,
            !rp.du_pct_by_path.is_empty(),
            rp.sort,
            rp.sort_desc,
            rp.loading,
        ),
        _ => return,
    };
    let show_strip2 = finding || !find_tf.value.is_empty() || !du_banner.is_empty();

    let mut constraints = vec![Constraint::Length(1)];
    if show_strip2 {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // column header
    constraints.push(Constraint::Min(1));    // list + scrollbar
    let chunks = Layout::default().direction(Direction::Vertical).constraints(constraints).split(area);

    // Row 1: Roots + Breadcrumb + quiet swap/more
    let mut bx = chunks[0].x;
    let by = chunks[0].y;
    let bmax = chunks[0].x + chunks[0].width;
    let reserve_right = 14u16;
    let crumb_max = bmax.saturating_sub(reserve_right);
    let mut b_spans = vec![Span::raw(" ")];
    bx = bx.saturating_add(1);
    for (key, lbl, root) in [
        ("working", "working", "/kaggle/working"),
        ("input", "input", "/kaggle/input"),
        ("tmp", "tmp", "/tmp"),
    ] {
        let on = dir.starts_with(root);
        let st = if on {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED)
        };
        text_btn(&mut app.rects, &mut b_spans, &mut bx, by, crumb_max, &format!("root-{}-{}", side_tag, key), lbl, st);
    }

    if bx + 4 < crumb_max {
        b_spans.push(Span::styled("│ ", Style::default().fg(BORDER)));
        bx = bx.saturating_add(2);
    }

    let parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let start_i = if parts.len() > 3 && crumb_max.saturating_sub(bx) < 34 {
        b_spans.push(Span::styled("…›", Style::default().fg(MUTED)));
        bx = bx.saturating_add(2);
        parts.len() - 3
    } else {
        0
    };
    let mut acc = String::new();
    for (idx, p) in parts.iter().enumerate() {
        acc.push('/');
        acc.push_str(p);
        if idx < start_i {
            continue;
        }
        if bx + 4 > crumb_max {
            break;
        }
        if idx > start_i {
            b_spans.push(Span::styled("›", Style::default().fg(MUTED)));
            bx = bx.saturating_add(1);
        }
        let disp = truncate_middle(p, (crumb_max.saturating_sub(bx) as usize).min(16));
        let w = disp.chars().count() as u16;
        if bx + w <= crumb_max {
            app.rects.buttons.push((Rect::new(bx, by, w, 1), format!("crumb-{}:{}", side_tag, acc)));
            let is_last = idx + 1 == parts.len();
            b_spans.push(Span::styled(
                disp,
                Style::default()
                    .fg(if is_last { TEXT } else { MUTED })
                    .add_modifier(if is_last { Modifier::BOLD } else { Modifier::empty() }),
            ));
            bx = bx.saturating_add(w);
        }
    }
    if bx < bmax.saturating_sub(reserve_right) {
        let gap = (bmax.saturating_sub(reserve_right)).saturating_sub(bx);
        b_spans.push(Span::raw(" ".repeat(gap as usize)));
        bx = bx.saturating_add(gap);
        text_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, &format!("swap-{}", side_tag), "m:swap", Style::default().fg(MUTED));
        text_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, &format!("act-{}", side_tag), "a:more", Style::default().fg(MUTED));
    }
    f.render_widget(Paragraph::new(Line::from(b_spans)).style(Style::default().bg(BG)), chunks[0]);

    let mut next_idx = 1;
    if show_strip2 {
        let s2 = chunks[next_idx];
        next_idx += 1;
        if finding || !find_tf.value.is_empty() {
            let mut s_spans = vec![Span::styled(
                " / ",
                Style::default().fg(if finding { ACCENT } else { MUTED }).add_modifier(Modifier::BOLD),
            )];
            s_spans.extend(text_field_spans(&find_tf, Style::default().fg(TEXT)));
            s_spans.push(Span::styled("  (Enter search · Esc exit)", Style::default().fg(MUTED)));
            f.render_widget(Paragraph::new(Line::from(s_spans)).style(Style::default().bg(BG)), s2);
            if finding {
                let cx = s2.x.saturating_add(3 + find_tf.cursor as u16).min(s2.x + s2.width.saturating_sub(1));
                f.set_cursor_position((cx, s2.y));
            }
        } else {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" DU ", Style::default().fg(if du_warn { ERR } else { WARN }).add_modifier(Modifier::BOLD)),
                    Span::styled(du_banner, Style::default().fg(MUTED)),
                ]))
                .style(Style::default().bg(BG)),
                s2,
            );
        }
    }

    let col_area = chunks[next_idx];
    let list_outer = chunks[next_idx + 1];

    if find_active {
        f.render_widget(
            Paragraph::new(Span::styled(
                " Search Results (←/Esc back · → preview · Enter open)",
                Style::default().fg(MUTED),
            ))
            .style(Style::default().bg(SURFACE)),
            col_area,
        );
        set_list_rect(&mut app.rects, side, list_outer);
        let hdr = Row::new(["Path", "Size", "Match"]).style(Style::default().fg(TEXT).bg(SURFACE).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = match app.pane(side) {
            PaneKind::RemoteFiles(rp) => rp
                .find_matches
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let st = if is_active && i == rp.find_cursor {
                        Style::default().bg(SEL_BG).fg(Color::White)
                    } else {
                        Style::default()
                    };
                    Row::new([
                        Cell::from(m.path.clone()),
                        Cell::from(fmt_bytes(m.size_bytes)),
                        Cell::from(if m.line > 0 { format!("L{}: {}", m.line, m.snippet) } else { "file".into() }),
                    ])
                    .style(st)
                })
                .collect(),
            _ => Vec::new(),
        };
        f.render_widget(
            Table::new(rows, [Constraint::Percentage(52), Constraint::Length(10), Constraint::Percentage(38)]).header(hdr),
            list_outer,
        );
        return;
    }

    let body_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(list_outer);
    let list_area = body_split[0];
    let sb_area = body_split[1];

    let size_w: usize = 9;
    let right_w: usize = if has_du_cols { 10 } else { 7 };
    let name_w = (list_area.width as usize).saturating_sub(2 + size_w + right_w + 1).max(8);
    let right_hdr = if has_du_cols { "DU%" } else { "Time" };

    render_column_headers(f, &mut app.rects, side_tag, sort, sort_desc, col_area, name_w, size_w, right_hdr, right_w);

    set_list_rect(&mut app.rects, side, list_area);
    let list_h = list_area.height as usize;
    if let PaneKind::RemoteFiles(ref mut rp) = app.pane_mut(side) {
        ensure_scroll(rp.cursor, &mut rp.scroll, list_h);
    }

    if loading {
        let spin = SPINNER[(app.frame_tick as usize) % SPINNER.len()];
        f.render_widget(
            Paragraph::new(format!("  {} Loading {}...", spin, dir)).style(Style::default().fg(ACCENT)),
            list_area,
        );
        return;
    }

    let has_session = app.active_session().is_some();
    let now_sec = current_unix_sec();
    let (lines, total_len, scroll_pos) = match app.pane(side) {
        PaneKind::RemoteFiles(rp) => {
            if rp.entries.is_empty() {
                let msg = if has_session {
                    "  Empty directory (r refresh, n new file)"
                } else {
                    "  No active Kaggle worker (F5 boot, ^W workers)"
                };
                (vec![Line::from(Span::styled(msg, Style::default().fg(MUTED)))], 0, 0)
            } else {
                let ls: Vec<Line> = rp
                    .entries
                    .iter()
                    .enumerate()
                    .skip(rp.scroll)
                    .take(list_h)
                    .map(|(i, e)| {
                        let is_cursor = i == rp.cursor;
                        let is_marked = rp.selected.contains(&e.path);
                        let (badge, b_col) = file_badge(&e.name, e.is_dir);
                        let raw_name = if e.is_dir && e.name != ".." {
                            format!("{}/", e.name)
                        } else {
                            e.name.clone()
                        };
                        let disp = truncate_end(&raw_name, name_w);
                        let pad = name_w.saturating_sub(disp.chars().count());
                        let sz = if e.name == ".." {
                            "<UP>".into()
                        } else if e.is_dir {
                            "<DIR>".into()
                        } else {
                            fmt_bytes(e.size_bytes)
                        };
                        let row_bg = if is_active && is_cursor {
                            Some(SEL_BG)
                        } else if !is_active && is_cursor {
                            Some(SEL_GHOST_BG)
                        } else if is_marked {
                            Some(MARKED_BG)
                        } else {
                            None
                        };
                        let mark_span = if is_marked {
                            Span::styled("▌ ", Style::default().fg(WARN).add_modifier(Modifier::BOLD))
                        } else {
                            Span::raw("  ")
                        };
                        let name_fg = if is_active && is_cursor {
                            Color::White
                        } else if e.name.starts_with('.') && e.name != ".." {
                            MUTED
                        } else if e.is_dir && e.name != ".." {
                            ACCENT
                        } else {
                            TEXT
                        };
                        let mut name_st = Style::default().fg(name_fg);
                        if (is_active && is_cursor) || (e.is_dir && e.name != "..") {
                            name_st = name_st.add_modifier(Modifier::BOLD);
                        }
                        let meta_st = Style::default().fg(if is_active && is_cursor { TEXT } else { MUTED });
                        let badge_st = Style::default().fg(if is_active && is_cursor { Color::White } else { b_col });

                        let mut spans = vec![
                            mark_span,
                            Span::styled(format!("{} ", badge), badge_st),
                            Span::styled(format!("{}{}", disp, " ".repeat(pad)), name_st),
                            Span::styled(format!("{:>width$}", sz, width = size_w), meta_st),
                            Span::raw(" "),
                        ];
                        if has_du_cols {
                            if let Some((pct, _)) = rp.du_pct_by_path.get(&e.path) {
                                spans.extend(du_micro_bar(*pct));
                            } else {
                                spans.push(Span::styled(format!("{:>width$}", "--", width = right_w - 1), meta_st));
                            }
                        } else {
                            let tm = if e.name == ".." { "--".into() } else { fmt_mtime(e.mtime, now_sec) };
                            spans.push(Span::styled(format!("{:>width$}", tm, width = right_w - 1), meta_st));
                        }
                        let mut line = Line::from(spans);
                        if let Some(bg) = row_bg {
                            line = line.style(Style::default().bg(bg));
                        }
                        line
                    })
                    .collect();
                (ls, rp.entries.len(), rp.scroll)
            }
        }
        _ => return,
    };

    f.render_widget(Paragraph::new(lines), list_area);
    let mut sb_state = ScrollbarState::new(total_len.max(1)).position(scroll_pos);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(Style::default().fg(BORDER)),
        sb_area,
        &mut sb_state,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_column_headers(
    f: &mut Frame,
    rects: &mut UiRects,
    side_tag: &str,
    sort: SortMode,
    sort_desc: bool,
    area: Rect,
    name_w: usize,
    size_w: usize,
    right_hdr: &str,
    right_w: usize,
) {
    let arrow = if sort_desc { "▼" } else { "▲" };
    let n_lbl = if sort == SortMode::Name { format!("Name {}", arrow) } else { "Name".into() };
    let s_lbl = if sort == SortMode::Size { format!("Size {}", arrow) } else { "Size".into() };
    let t_lbl = if sort == SortMode::Time { format!("{} {}", right_hdr, arrow) } else { right_hdr.into() };

    let x0 = area.x + 2;
    let y = area.y;
    let nw = name_w as u16;
    let sw = size_w as u16;
    let tw = right_w as u16;
    if x0 + nw <= area.x + area.width {
        rects.buttons.push((Rect::new(x0, y, nw, 1), format!("sort-{}-name", side_tag)));
    }
    if x0 + nw + sw <= area.x + area.width {
        rects.buttons.push((Rect::new(x0 + nw, y, sw, 1), format!("sort-{}-size", side_tag)));
    }
    if x0 + nw + sw + tw <= area.x + area.width {
        rects.buttons.push((Rect::new(x0 + nw + sw, y, tw, 1), format!("sort-{}-time", side_tag)));
    }

    let n_pad = name_w.saturating_sub(n_lbl.chars().count());
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("  {}{}", n_lbl, " ".repeat(n_pad)),
            Style::default()
                .fg(if sort == SortMode::Name { ACCENT } else { MUTED })
                .add_modifier(if sort == SortMode::Name { Modifier::BOLD } else { Modifier::empty() }),
        ),
        Span::styled(
            format!("{:>width$}", s_lbl, width = size_w),
            Style::default()
                .fg(if sort == SortMode::Size { ACCENT } else { MUTED })
                .add_modifier(if sort == SortMode::Size { Modifier::BOLD } else { Modifier::empty() }),
        ),
        Span::styled(
            format!("{:>width$}", t_lbl, width = right_w),
            Style::default()
                .fg(if sort == SortMode::Time { ACCENT } else { MUTED })
                .add_modifier(if sort == SortMode::Time { Modifier::BOLD } else { Modifier::empty() }),
        ),
    ]);
    f.render_widget(Paragraph::new(line).style(Style::default().bg(SURFACE)), area);
}

fn render_shell_body(f: &mut Frame, app: &mut App, is_active: bool, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    let mut lines: Vec<Line> = Vec::new();
    for ev in app.activities.iter().rev().take(60).rev() {
        let tag_col = if ev.caller == "mcp" { ACCENT } else { MUTED };
        let ec = match ev.exit_code {
            Some(0) => Span::styled(" ✓0", Style::default().fg(OK)),
            Some(c) => Span::styled(format!(" ✗{}", c), Style::default().fg(ERR)),
            None => Span::raw(""),
        };
        lines.push(Line::from(vec![
            Span::styled("│ ", Style::default().fg(tag_col)),
            Span::styled(format!("[{}] ", ev.caller), Style::default().fg(tag_col).add_modifier(Modifier::BOLD)),
            Span::styled(ev.summary.clone(), Style::default().fg(TEXT)),
            ec,
        ]));
        let detail_lines: Vec<&str> = ev.detail.lines().filter(|l| !l.trim().is_empty()).collect();
        let total_dl = detail_lines.len();
        for dl in detail_lines.into_iter().take(60) {
            lines.push(Line::from(vec![
                Span::styled("│   ", Style::default().fg(BORDER)),
                Span::styled(dl.to_string(), Style::default().fg(MUTED)),
            ]));
        }
        if total_dl > 60 {
            lines.push(Line::from(vec![
                Span::styled("│   ", Style::default().fg(BORDER)),
                Span::styled(
                    format!("… (+{} more lines, press y in NAV to copy all)", total_dl - 60),
                    Style::default().fg(MUTED),
                ),
            ]));
        }
    }
    if let Some(ref run) = app.running_exec {
        let spin = SPINNER[(app.frame_tick as usize) % SPINNER.len()];
        let elapsed = run.started_at.elapsed().as_secs_f64();
        lines.push(Line::from(vec![
            Span::styled("│ ", Style::default().fg(ACCENT)),
            Span::styled(
                format!("{} [running {:.1}s] ", spin, elapsed),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("$ {}", run.command), Style::default().fg(TEXT)),
            Span::styled("  (Ctrl+C stop)", Style::default().fg(MUTED)),
        ]));
        if !run.live_output.is_empty() {
            let live_lines: Vec<&str> = run.live_output.lines().collect();
            let skip_n = live_lines.len().saturating_sub(40);
            for dl in live_lines.into_iter().skip(skip_n) {
                lines.push(Line::from(vec![
                    Span::styled("│   ", Style::default().fg(ACCENT)),
                    Span::styled(dl.to_string(), Style::default().fg(TEXT)),
                ]));
            }
        } else if elapsed >= 2.0 && !run.worker_has_live {
            lines.push(Line::from(vec![
                Span::styled("│   ", Style::default().fg(BORDER)),
                Span::styled(
                    "waiting for output... (use :run <cmd> or Alt+Enter for instant streaming on older workers; reboot F5 for live PTY)",
                    Style::default().fg(MUTED),
                ),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " Ready. Type bash or :commands below (:run <cmd> / Alt+Enter for streaming loops).",
            Style::default().fg(MUTED),
        )));
    }
    app.rects.shell_content_rect = chunks[0];
    let h = chunks[0].height as usize;
    let max_sc = lines.len().saturating_sub(h);
    let sc = if app.activity_scroll == 0 { max_sc } else { app.activity_scroll.min(max_sc) };
    f.render_widget(Paragraph::new(lines).scroll((sc as u16, 0)).wrap(Wrap { trim: false }), chunks[0]);

    f.render_widget(Paragraph::new("─".repeat(chunks[1].width as usize)).style(Style::default().fg(BORDER)), chunks[1]);

    app.rects.prompt_rect = chunks[2];
    let typing = is_active && app.input_focus == InputFocus::ShellPrompt && app.modal.is_none();
    let mode_span = if typing {
        Span::styled(" [INPUT] ", Style::default().fg(OK).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" [NAV] ", Style::default().fg(MUTED))
    };
    let prefix = format!("kgsh({}) {} > ", app.current_slot, app.known_cwd);
    let badge_w = if typing { 9 } else { 7 };
    let p_len = badge_w + prefix.chars().count() as u16;
    let mut p_spans = vec![
        mode_span,
        Span::styled(prefix, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
    ];
    p_spans.extend(text_field_spans(&app.prompt, Style::default().fg(TEXT)));
    f.render_widget(
        Paragraph::new(Line::from(p_spans)).style(Style::default().bg(SURFACE)),
        chunks[2],
    );
    if typing {
        let cx = chunks[2].x.saturating_add(p_len + app.prompt.cursor as u16).min(chunks[2].x + chunks[2].width.saturating_sub(1));
        f.set_cursor_position((cx, chunks[2].y));
    }
}

fn render_jobs_body(f: &mut Frame, app: &mut App, side: Side, is_active: bool, area: Rect) {
    let vsplit = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    set_list_rect(&mut app.rects, side, vsplit[0]);
    let hdr = Row::new(["ID", "Name", "GPU", "State", "Time"])
        .style(Style::default().fg(TEXT).bg(SURFACE).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .jobs
        .iter()
        .enumerate()
        .map(|(i, j)| {
            let short_id: String = j.job_id.chars().rev().take(11).collect::<String>().chars().rev().collect();
            let st_col = match j.status.as_str() { "running" => WARN, "completed" => OK, _ => ERR };
            let st = if is_active && i == app.selected_job_idx { Style::default().bg(SEL_BG).fg(Color::White) } else { Style::default() };
            Row::new([
                Cell::from(short_id),
                Cell::from(j.name.clone()),
                Cell::from(if j.gpu_ids.is_empty() { "all" } else { &j.gpu_ids }.to_string()),
                Cell::from(Span::styled(j.status.clone(), Style::default().fg(st_col))),
                Cell::from(format!("{:.0}s", j.runtime_sec)),
            ])
            .style(st)
        })
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Length(12), Constraint::Min(10), Constraint::Length(6), Constraint::Length(10), Constraint::Length(7)]).header(hdr),
        vsplit[0],
    );

    let gblk = Block::default()
        .title(" GPU Processes ([/] select · K kill) ")
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER));
    let ginner = gblk.inner(vsplit[1]);
    f.render_widget(gblk, vsplit[1]);
    if app.gpu_procs.is_empty() {
        f.render_widget(Paragraph::new(" No active CUDA processes.").style(Style::default().fg(MUTED)), ginner);
    } else {
        let ghdr = Row::new(["PID", "GPU", "Process", "VRAM"]).style(Style::default().fg(TEXT).bg(SURFACE));
        let grows: Vec<Row> = app
            .gpu_procs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let st = if is_active && i == app.selected_gpu_proc_idx { Style::default().bg(SEL_BG).fg(Color::White) } else { Style::default() };
                Row::new([
                    Cell::from(p.pid.to_string()),
                    Cell::from(p.gpu_index.clone()),
                    Cell::from(p.process_name.clone()),
                    Cell::from(format!("{} MB", p.used_mb)),
                ])
                .style(st)
            })
            .collect();
        f.render_widget(
            Table::new(grows, [Constraint::Length(9), Constraint::Length(5), Constraint::Min(10), Constraint::Length(10)]).header(ghdr),
            ginner,
        );
    }
}

fn render_logs_body(f: &mut Frame, app: &mut App, area: Rect) {
    if app.selected_job_id.is_empty() && app.raw_log_lines.is_empty() {
        let max_x = area.x + area.width;
        let items = [
            ("quick-boot", "F5     Boot Worker"),
            ("quick-workers", "^W     Workers & Quota"),
            ("quick-files", "F2     Dual Files"),
            ("quick-hub", "F4     Kaggle Hub"),
            ("quick-sync", ":sync  Sync Files"),
        ];
        let mut lines = vec![Line::raw("")];
        for (i, (id, lbl)) in items.iter().enumerate() {
            let y = area.y + 1 + i as u16;
            if y >= area.y + area.height {
                break;
            }
            let mut x = area.x + 2;
            let mut row = vec![Span::raw("  ")];
            text_btn(&mut app.rects, &mut row, &mut x, y, max_x, id, lbl, Style::default().fg(TEXT));
            lines.push(Line::from(row));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("  Press `m` to swap this panel to Remote Files.", Style::default().fg(MUTED))));
        f.render_widget(Paragraph::new(lines), area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // Row 0: Live Job Status Bar + clickable controls
    let mut jx = chunks[0].x;
    let jy = chunks[0].y;
    let jmax = chunks[0].x + chunks[0].width;
    let mut j_spans = Vec::new();
    if let Some(job) = app.jobs.iter().find(|j| j.job_id == app.selected_job_id).cloned() {
        let (badge, bcol) = match job.status.as_str() {
            "running" => (" ● RUNNING ", WARN),
            "completed" => (" ✓ DONE ", OK),
            _ => (" ✗ FAIL ", ERR),
        };
        let head = format!(
            "{}  {:.1}s  {:.1} KB  ",
            truncate_end(&job.name, 20),
            job.runtime_sec,
            job.log_size_kb
        );
        j_spans.push(Span::styled(badge, Style::default().fg(bcol).add_modifier(Modifier::BOLD)));
        j_spans.push(Span::styled(head.clone(), Style::default().fg(TEXT)));
        jx = jx.saturating_add((badge.chars().count() + head.chars().count()) as u16);
        if job.status == "running" {
            key_hint_btn(&mut app.rects, &mut j_spans, &mut jx, jy, jmax, "logs-kill-job", "x", "kill", ERR);
        }
    } else {
        let lbl = " ● Job Stream  ";
        j_spans.push(Span::styled(lbl, Style::default().fg(MUTED)));
        jx = jx.saturating_add(lbl.chars().count() as u16);
    }
    let pause_lbl = if app.logs_paused { "resume" } else { "pause" };
    key_hint_btn(&mut app.rects, &mut j_spans, &mut jx, jy, jmax, "foot-log-pause", "Spc", pause_lbl, TEXT);
    key_hint_btn(&mut app.rects, &mut j_spans, &mut jx, jy, jmax, "foot-log-copy", "y", "copy", TEXT);
    f.render_widget(Paragraph::new(Line::from(j_spans)).style(Style::default().bg(BG)), chunks[0]);

    let filtering = app.input_focus == InputFocus::LogFilter;
    let mut lf_spans = vec![Span::styled(
        " / ",
        Style::default().fg(if filtering { ACCENT } else { MUTED }).add_modifier(Modifier::BOLD),
    )];
    lf_spans.extend(text_field_spans(&app.log_filter, Style::default().fg(TEXT)));
    lf_spans.push(Span::styled(if app.logs_paused { "  [PAUSED]" } else { "" }, Style::default().fg(WARN)));
    f.render_widget(
        Paragraph::new(Line::from(lf_spans)).style(Style::default().bg(SURFACE)),
        chunks[1],
    );
    app.rects.logs_content_rect = chunks[2];
    let filtered = app.filtered_log_lines();
    let lh = chunks[2].height as usize;
    let max_sc = filtered.len().saturating_sub(lh);
    let sc = if !app.logs_paused || app.log_scroll > max_sc { max_sc } else { app.log_scroll };
    let vis: Vec<Line> = filtered.into_iter().skip(sc).take(lh).map(|s| Line::from(s.as_str())).collect();
    f.render_widget(Paragraph::new(vis), chunks[2]);
}

fn render_hub_list_body(f: &mut Frame, app: &mut App, side: Side, is_active: bool, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let mut x = chunks[0].x + 1;
    let y = chunks[0].y;
    let max_x = chunks[0].x + chunks[0].width;
    let mut cat_spans = vec![Span::raw(" ")];
    for (idx, (_, label)) in HUB_CATEGORIES.iter().enumerate() {
        let id = format!("hub-cat-{}", idx);
        let st = if idx == app.hub_cat_idx {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(MUTED)
        };
        text_btn(&mut app.rects, &mut cat_spans, &mut x, y, max_x, &id, label, st);
    }
    f.render_widget(Paragraph::new(Line::from(cat_spans)).style(Style::default().bg(BG)), chunks[0]);

    let searching = app.input_focus == InputFocus::HubSearch;
    let mut hs_spans = vec![Span::styled(
        " / ",
        Style::default().fg(if searching { ACCENT } else { MUTED }).add_modifier(Modifier::BOLD),
    )];
    hs_spans.extend(text_field_spans(&app.hub_search, Style::default().fg(TEXT)));
    f.render_widget(
        Paragraph::new(Line::from(hs_spans)).style(Style::default().bg(SURFACE)),
        chunks[1],
    );

    set_list_rect(&mut app.rects, side, chunks[2]);
    let hdr = Row::new(["Ref / Slug", "Title", "Info", "Metric"]).style(Style::default().fg(TEXT).bg(SURFACE).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .hub_items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let st = if is_active && i == app.hub_cursor { Style::default().bg(SEL_BG).fg(Color::White) } else { Style::default() };
            Row::new([
                Cell::from(it.item_ref.chars().take(24).collect::<String>()),
                Cell::from(it.title.chars().take(26).collect::<String>()),
                Cell::from(it.info.clone()),
                Cell::from(it.metric.clone()),
            ])
            .style(st)
        })
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Percentage(32), Constraint::Percentage(34), Constraint::Percentage(17), Constraint::Percentage(17)]).header(hdr),
        chunks[2],
    );
}

fn render_hub_detail_body(f: &mut Frame, app: &mut App, area: Rect) {
    app.rects.hub_detail_rect = area;
    if let Some(ref d) = app.hub_detail {
        let lines: Vec<Line> = d.text.lines().map(|l| Line::from(l.to_string())).collect();
        f.render_widget(
            Paragraph::new(lines).scroll((app.hub_detail_scroll as u16, 0)).wrap(Wrap { trim: false }),
            area,
        );
    } else {
        f.render_widget(
            Paragraph::new(" Select an item on the left and press Enter to inspect.")
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

pub(super) fn render_preview_drawer(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(pd) = app.preview_drawer.clone() else { return; };
    let blk = Block::default()
        .title_top(
            Line::from(Span::styled(
                format!(" Preview: {} ({}) ", truncate_middle(&pd.title, 44), pd.subtitle),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ))
            .left_aligned(),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(BORDER_ACTIVE))
        .style(Style::default().bg(BG));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    if inner.height < 2 || inner.width < 10 {
        return;
    }

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let mut bx = split[0].x + 1;
    let by = split[0].y;
    let bmax = split[0].x + split[0].width;
    let mut b_spans = vec![Span::raw(" ")];
    key_hint_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, "drawer-open", "Enter", "open", TEXT);
    key_hint_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, "drawer-transfer", "t", "transfer", TEXT);
    key_hint_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, "drawer-copy", "y", "copy", TEXT);
    key_hint_btn(&mut app.rects, &mut b_spans, &mut bx, by, bmax, "drawer-close", "v", "close", TEXT);
    f.render_widget(Paragraph::new(Line::from(b_spans)).style(Style::default().bg(SURFACE)), split[0]);

    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(split[1]);
    app.rects.preview_body_rect = body_cols[0];
    app.rects.preview_code_rect = Rect::new(
        body_cols[0].x.saturating_add(7),
        body_cols[0].y,
        body_cols[0].width.saturating_sub(7),
        body_cols[0].height,
    );
    let vh = body_cols[0].height as usize;
    let total = pd.lines.len();
    let max_sc = total.saturating_sub(vh);
    let sc = pd.scroll.min(max_sc);
    let vis: Vec<Line> = pd
        .lines
        .iter()
        .enumerate()
        .skip(sc)
        .take(vh)
        .map(|(idx, ln)| highlight_preview_line(&pd.title, idx + 1, ln))
        .collect();
    f.render_widget(Paragraph::new(vis), body_cols[0]);

    let mut sb = ScrollbarState::new(total.max(1)).position(sc);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(Style::default().fg(BORDER)),
        body_cols[1],
        &mut sb,
    );
}

fn set_list_rect(rects: &mut UiRects, side: Side, r: Rect) {
    match side {
        Side::Left => rects.left_list_rect = r,
        Side::Right => rects.right_list_rect = r,
    }
}

