use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;

use super::*;

impl App {
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let x = mouse.column;
        let y = mouse.row;

        if self.modal.is_some() {
            if matches!(self.modal, Some(Modal::FileEditor(_))) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                            m.scroll_row = m.scroll_row.saturating_sub(3);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                            let max_sc = m.lines.len().saturating_sub(1);
                            m.scroll_row = (m.scroll_row + 3).min(max_sc);
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.selection = None;
                        self.drag_anchor = None;
                        let path_rect = self.rects.editor_path_rect;
                        let body_rect = self.rects.editor_body_rect;
                        let code_rect = self.rects.editor_code_rect;
                        let modal_rect = self.rects.modal_rect;
                        if rect_contains(path_rect, x, y) {
                            self.editor_mouse_selecting = false;
                            if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                                m.focus_path = true;
                                m.sel_anchor = None;
                                let col = x.saturating_sub(path_rect.x + 23) as usize;
                                let clamped = col.min(m.path.value.chars().count());
                                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                                    if m.path.sel_anchor.is_none() {
                                        m.path.sel_anchor = Some(m.path.cursor);
                                    }
                                } else {
                                    m.path.sel_anchor = None;
                                }
                                m.path.cursor = clamped;
                            }
                        } else if rect_contains(body_rect, x, y) {
                            if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                                m.focus_path = false;
                                let rel_y = y.saturating_sub(body_rect.y) as usize;
                                let raw_row = m.scroll_row + rel_y;
                                let rel_x = x.saturating_sub(code_rect.x) as usize;
                                let (r, c) = m.clamp_pos(raw_row, rel_x);
                                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                                    if m.sel_anchor.is_none() {
                                        m.sel_anchor = Some((m.cur_row, m.cur_col));
                                    }
                                } else {
                                    m.sel_anchor = Some((r, c));
                                }
                                m.cur_row = r;
                                m.cur_col = c;
                                self.editor_mouse_selecting = true;
                            }
                        } else if !rect_contains(modal_rect, x, y) {
                            let dirty_guard = match self.modal {
                                Some(Modal::FileEditor(ref mut m)) if m.dirty && !m.confirm_discard => {
                                    m.confirm_discard = true;
                                    true
                                }
                                _ => false,
                            };
                            if dirty_guard {
                                self.notify(
                                    "Unsaved changes! Press Esc or click outside again to discard, or Ctrl+S to save.",
                                    true,
                                );
                            } else {
                                self.modal = None;
                            }
                            self.editor_mouse_selecting = false;
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) if self.editor_mouse_selecting => {
                        let code_rect = self.rects.editor_code_rect;
                        let body_rect = self.rects.editor_body_rect;
                        if body_rect.height > 0 {
                            if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                                let cy = y.clamp(body_rect.y, body_rect.bottom().saturating_sub(1));
                                let rel_y = cy.saturating_sub(body_rect.y) as usize;
                                let raw_row = m.scroll_row + rel_y;
                                let cx = if code_rect.width > 0 {
                                    x.clamp(code_rect.x, code_rect.right().saturating_sub(1))
                                } else {
                                    code_rect.x
                                };
                                let rel_x = cx.saturating_sub(code_rect.x) as usize;
                                let (r, c) = m.clamp_pos(raw_row, rel_x);
                                m.cur_row = r;
                                m.cur_col = c;
                            }
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) if self.editor_mouse_selecting => {
                        self.editor_mouse_selecting = false;
                        let copied = if let Some(Modal::FileEditor(ref mut m)) = self.modal {
                            if let Some(txt) = m.selected_text().filter(|s| !s.is_empty()) {
                                Some(txt)
                            } else {
                                m.sel_anchor = None;
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(txt) = copied {
                            self.notify_copied_text(&txt);
                        }
                    }
                    _ => {}
                }
                return;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(ref mut modal) = self.modal {
                        match modal {
                            Modal::ActionMenu(ref mut i) | Modal::SwapPane(ref mut i) => {
                                *i = i.saturating_sub(1);
                            }
                            Modal::CommandPalette(ref mut m) => m.selected = m.selected.saturating_sub(1),
                            Modal::SmartInspect(ref mut m) => m.scroll = m.scroll.saturating_sub(2),
                            Modal::Workers(ref mut m) if m.tab == 0 => {
                                m.cluster_selected = m.cluster_selected.saturating_sub(1);
                            }
                            _ => {}
                        }
                    }
                }
                MouseEventKind::ScrollDown => {
                    let act_len = self.action_menu_items().len();
                    if let Some(ref mut modal) = self.modal {
                        match modal {
                            Modal::ActionMenu(ref mut i) => {
                                if *i + 1 < act_len { *i += 1; }
                            }
                            Modal::SwapPane(ref mut i) => {
                                if *i + 1 < SWAP_PANE_OPTIONS.len() { *i += 1; }
                            }
                            Modal::CommandPalette(ref mut m) => {
                                let len = filtered_palette_items(&m.query.value).len();
                                if m.selected + 1 < len { m.selected += 1; }
                            }
                            Modal::SmartInspect(ref mut m) => m.scroll = m.scroll.saturating_add(2),
                            Modal::Workers(ref mut m)
                                if m.tab == 0 && m.cluster_selected + 1 < m.cluster_rows.len() =>
                            {
                                m.cluster_selected += 1;
                            }
                            _ => {}
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some((ax, ay, bounds, kind)) = self.drag_anchor {
                        if bounds.width > 0 && bounds.height > 0 {
                            let cx = x.clamp(bounds.x, bounds.right().saturating_sub(1));
                            let cy = y.clamp(bounds.y, bounds.bottom().saturating_sub(1));
                            if (cx, cy) != (ax, ay) {
                                self.selection = Some(TextSelection {
                                    start: (ax, ay),
                                    end: (cx, cy),
                                    bounds,
                                    dragging: true,
                                    kind,
                                    line_mode: false,
                                });
                            }
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.drag_anchor = None;
                    if let Some(mut sel) = self.selection {
                        if sel.dragging && sel.start != sel.end {
                            sel.dragging = false;
                            self.selection = Some(sel);
                            self.last_click_anchor = None;
                            let txt = self.extract_selected_text(sel);
                            self.notify_copied_text(&txt);
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.selection = None;
                    if rect_contains(self.rects.modal_list_rect, x, y) {
                        let rel = (y - self.rects.modal_list_rect.y) as usize;
                        match self.modal.clone() {
                            Some(Modal::ActionMenu(_)) => {
                                let items = self.action_menu_items();
                                if let Some((id, _)) = items.get(rel) {
                                    self.modal = None;
                                    self.execute_context_action(id);
                                }
                                return;
                            }
                            Some(Modal::SwapPane(_)) => {
                                if let Some((opt, _)) = SWAP_PANE_OPTIONS.get(rel) {
                                    self.modal = None;
                                    self.assign_pane(self.active_side, opt);
                                }
                                return;
                            }
                            Some(Modal::CommandPalette(m)) => {
                                let items = filtered_palette_items(&m.query.value);
                                if let Some((_, _, cmd)) = items.get(rel) {
                                    let c = cmd.to_string();
                                    self.modal = None;
                                    self.execute_palette_cmd(&c);
                                }
                                return;
                            }
                            Some(Modal::Workers(mut m)) if m.tab == 0 => {
                                if rel > 0 && rel - 1 < m.cluster_rows.len() {
                                    m.cluster_selected = rel - 1;
                                    self.modal = Some(Modal::Workers(m));
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                    let btn = self
                        .rects
                        .buttons
                        .iter()
                        .find(|(r, _)| rect_contains(*r, x, y))
                        .map(|(_, id)| id.clone());
                    if let Some(id) = btn {
                        self.handle_button_click(&id);
                        return;
                    }
                    if !rect_contains(self.rects.modal_rect, x, y) {
                        self.modal = None;
                        self.last_click_anchor = None;
                    } else {
                        let target = self.selection_target_for(x, y);
                        self.last_click_anchor = target;
                        self.drag_anchor = target;
                    }
                }
                _ => {}
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if rect_contains(self.rects.drawer_rect, x, y) {
                    if let Some(ref mut pd) = self.preview_drawer {
                        pd.scroll = pd.scroll.saturating_sub(3);
                    }
                    return;
                }
                let hover_side = if rect_contains(self.rects.right_rect, x, y) {
                    Side::Right
                } else {
                    Side::Left
                };
                self.scroll_side(hover_side, -2);
            }
            MouseEventKind::ScrollDown => {
                if rect_contains(self.rects.drawer_rect, x, y) {
                    if let Some(ref mut pd) = self.preview_drawer {
                        pd.scroll = pd.scroll.saturating_add(3);
                    }
                    return;
                }
                let hover_side = if rect_contains(self.rects.right_rect, x, y) {
                    Side::Right
                } else {
                    Side::Left
                };
                self.scroll_side(hover_side, 2);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((ax, ay, bounds, kind)) = self.drag_anchor {
                    if bounds.width > 0 && bounds.height > 0 {
                        let cx = x.clamp(bounds.x, bounds.right().saturating_sub(1));
                        let cy = y.clamp(bounds.y, bounds.bottom().saturating_sub(1));
                        if (cx, cy) != (ax, ay) {
                            self.selection = Some(TextSelection {
                                start: (ax, ay),
                                end: (cx, cy),
                                bounds,
                                dragging: true,
                                kind,
                                line_mode: false,
                            });
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_anchor = None;
                if let Some(mut sel) = self.selection {
                    if sel.dragging && sel.start != sel.end {
                        sel.dragging = false;
                        self.selection = Some(sel);
                        self.last_click_anchor = None;
                        let txt = self.extract_selected_text(sel);
                        self.notify_copied_text(&txt);
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                for (p, r) in &self.rects.nav_presets {
                    if rect_contains(*r, x, y) {
                        self.apply_preset(*p);
                        return;
                    }
                }
                if rect_contains(self.rects.nav_workers, x, y) {
                    self.open_workers_modal(0);
                    return;
                }
                if rect_contains(self.rects.nav_help, x, y) {
                    self.modal = Some(Modal::Help);
                    return;
                }
                let btn = self
                    .rects
                    .buttons
                    .iter()
                    .find(|(r, _)| rect_contains(*r, x, y))
                    .map(|(_, id)| id.clone());
                if let Some(id) = btn {
                    self.handle_button_click(&id);
                    return;
                }
                if rect_contains(self.rects.prompt_rect, x, y) {
                    self.input_focus = InputFocus::ShellPrompt;
                    self.last_click_anchor = None;
                    if matches!(self.left_pane, PaneKind::Shell) {
                        self.active_side = Side::Left;
                    } else if matches!(self.right_pane, PaneKind::Shell) {
                        self.active_side = Side::Right;
                    }
                    return;
                }
                if rect_contains(self.rects.drawer_rect, x, y) {
                    let target = self.selection_target_for(x, y);
                    self.last_click_anchor = target;
                    self.drag_anchor = target;
                } else if rect_contains(self.rects.left_rect, x, y) {
                    self.active_side = Side::Left;
                    self.input_focus = InputFocus::None;
                    let target = self.selection_target_for(x, y);
                    self.last_click_anchor = target;
                    self.drag_anchor = target;
                    self.click_list_row(Side::Left, y, false);
                } else if rect_contains(self.rects.right_rect, x, y) {
                    self.active_side = Side::Right;
                    self.input_focus = InputFocus::None;
                    let target = self.selection_target_for(x, y);
                    self.last_click_anchor = target;
                    self.drag_anchor = target;
                    self.click_list_row(Side::Right, y, false);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if rect_contains(self.rects.left_rect, x, y) {
                    self.active_side = Side::Left;
                    self.input_focus = InputFocus::None;
                    self.click_list_row(Side::Left, y, true);
                } else if rect_contains(self.rects.right_rect, x, y) {
                    self.active_side = Side::Right;
                    self.input_focus = InputFocus::None;
                    self.click_list_row(Side::Right, y, true);
                }
            }
            _ => {}
        }
    }

    fn scroll_side(&mut self, side: Side, delta: isize) {
        match self.pane_mut(side) {
            PaneKind::Shell => {
                if delta < 0 {
                    self.activity_scroll = self.activity_scroll.saturating_sub((-delta) as usize);
                } else {
                    self.activity_scroll = self.activity_scroll.saturating_add(delta as usize);
                }
            }
            PaneKind::LiveLogs => {
                if delta < 0 {
                    self.log_scroll = self.log_scroll.saturating_sub((-delta) as usize);
                } else {
                    self.log_scroll = self.log_scroll.saturating_add(delta as usize);
                }
            }
            PaneKind::HubDetail => {
                if delta < 0 {
                    self.hub_detail_scroll = self.hub_detail_scroll.saturating_sub((-delta) as usize);
                } else {
                    self.hub_detail_scroll = self.hub_detail_scroll.saturating_add(delta as usize);
                }
            }
            PaneKind::RemoteFiles(ref mut rp) => {
                if delta < 0 {
                    rp.cursor = rp.cursor.saturating_sub((-delta) as usize);
                } else if rp.cursor + (delta as usize) < rp.entries.len() {
                    rp.cursor += delta as usize;
                }
            }
            PaneKind::LocalFiles(ref mut lp) => {
                if delta < 0 {
                    lp.cursor = lp.cursor.saturating_sub((-delta) as usize);
                } else if lp.cursor + (delta as usize) < lp.entries.len() {
                    lp.cursor += delta as usize;
                }
            }
            _ => {}
        }
    }

    fn click_list_row(&mut self, side: Side, y: u16, toggle_select: bool) {
        let lr = match side {
            Side::Left => self.rects.left_list_rect,
            Side::Right => self.rects.right_list_rect,
        };
        if lr.height == 0 || y < lr.y || y >= lr.y + lr.height {
            return;
        }
        let rel = (y - lr.y) as usize;
        let now = Instant::now();
        let mut clicked_idx: Option<usize> = None;
        let hub_len = self.hub_items.len();

        match self.pane_mut(side) {
            PaneKind::RemoteFiles(ref mut rp) => {
                let idx = rel + rp.scroll;
                if idx < rp.entries.len() {
                    rp.cursor = idx;
                    clicked_idx = Some(idx);
                    if toggle_select {
                        let ent = rp.entries[idx].clone();
                        if ent.name != ".." && !rp.selected.remove(&ent.path) {
                            rp.selected.insert(ent.path);
                        }
                    }
                }
            }
            PaneKind::LocalFiles(ref mut lp) => {
                let idx = rel + lp.scroll;
                if idx < lp.entries.len() {
                    lp.cursor = idx;
                    clicked_idx = Some(idx);
                    if toggle_select {
                        let ent = lp.entries[idx].clone();
                        if ent.name != ".." && !lp.selected.remove(&ent.path) {
                            lp.selected.insert(ent.path);
                        }
                    }
                }
            }
            PaneKind::JobsAndGpu => {
                if rel < self.jobs.len() {
                    self.selected_job_idx = rel;
                    self.selected_job_id = self.jobs[rel].job_id.clone();
                    self.log_offset = 0;
                    self.raw_log_lines.clear();
                }
            }
            PaneKind::HubList if rel < hub_len => {
                self.hub_cursor = rel;
            }
            _ => {}
        }

        if toggle_select {
            return;
        }
        if let Some(idx) = clicked_idx {
            let is_dbl = self
                .last_click
                .map(|(s, prev_i, ts)| s == side && prev_i == idx && now.duration_since(ts) <= Duration::from_millis(450))
                .unwrap_or(false);
            if is_dbl {
                self.last_click = None;
                self.execute_context_action("open");
            } else {
                self.last_click = Some((side, idx, now));
                self.schedule_auto_preview(side);
            }
        }
    }

    fn handle_button_click(&mut self, id: &str) {
        if let Some(path) = id.strip_prefix("crumb-left:") {
            self.active_side = Side::Left;
            self.click_breadcrumb(Side::Left, path.to_string());
            return;
        }
        if let Some(path) = id.strip_prefix("crumb-right:") {
            self.active_side = Side::Right;
            self.click_breadcrumb(Side::Right, path.to_string());
            return;
        }
        if let Some(rest) = id.strip_prefix("tb-") {
            if let Some((side_str, act)) = rest.split_once('-') {
                let side = if side_str == "left" { Side::Left } else { Side::Right };
                self.active_side = side;
                self.handle_toolbar_action(side, act);
                return;
            }
        }
        if let Some(rest) = id.strip_prefix("sort-") {
            if let Some((side_str, col)) = rest.split_once('-') {
                let side = if side_str == "left" { Side::Left } else { Side::Right };
                self.active_side = side;
                let mode = match col {
                    "size" => SortMode::Size,
                    "time" => SortMode::Time,
                    _ => SortMode::Name,
                };
                match self.pane_mut(side) {
                    PaneKind::LocalFiles(ref mut lp) => {
                        if lp.sort == mode {
                            lp.sort_desc = !lp.sort_desc;
                        } else {
                            lp.sort = mode;
                            lp.sort_desc = false;
                        }
                        lp.reload();
                    }
                    PaneKind::RemoteFiles(ref mut rp) => {
                        if rp.sort == mode {
                            rp.sort_desc = !rp.sort_desc;
                        } else {
                            rp.sort = mode;
                            rp.sort_desc = false;
                        }
                        rp.rebuild_entries();
                    }
                    _ => {}
                }
                return;
            }
        }
        if let Some(rest) = id.strip_prefix("local-jump-") {
            if let Some((side_str, target_k)) = rest.split_once('-') {
                let side = if side_str == "left" { Side::Left } else { Side::Right };
                self.active_side = side;
                let dest = match target_k {
                    "home" => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
                    "dl" => PathBuf::from("./kaggle_downloads"),
                    _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                };
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.navigate_into(dest);
                }
                return;
            }
        }

        match id {
            "nav-boot" => self.open_workers_modal(1),
            "nav-mouse" => self.toggle_mouse_capture(),
            "nav-palette" => {
                self.modal = Some(Modal::CommandPalette(PaletteModalState {
                    query: TextField::new(""),
                    selected: 0,
                }));
            }
            "workers-tab-0" => {
                if let Some(Modal::Workers(ref mut w)) = self.modal {
                    w.tab = 0;
                }
            }
            "workers-tab-1" => {
                if let Some(Modal::Workers(ref mut w)) = self.modal {
                    w.tab = 1;
                }
            }
            "drawer-open" => self.execute_context_action("open"),
            "drawer-transfer" => self.transfer_to_opposite(),
            "drawer-copy" => {
                if let Some(txt) = self.preview_drawer.as_ref().map(|pd| pd.lines.join("\n")) {
                    self.set_clipboard_text(&txt);
                    self.notify("Copied preview text", false);
                }
            }
            "drawer-close" => {
                self.preview_drawer = None;
                self.preview_pending = None;
            }
            "swap-left" => {
                self.active_side = Side::Left;
                self.modal = Some(Modal::SwapPane(0));
            }
            "swap-right" => {
                self.active_side = Side::Right;
                self.modal = Some(Modal::SwapPane(0));
            }
            "act-left" => {
                self.active_side = Side::Left;
                self.modal = Some(Modal::ActionMenu(0));
            }
            "act-right" => {
                self.active_side = Side::Right;
                self.modal = Some(Modal::ActionMenu(0));
            }
            "root-left-working" => { self.active_side = Side::Left; self.spawn_fs_list(Side::Left, "/kaggle/working".into()); }
            "root-left-input" => { self.active_side = Side::Left; self.spawn_fs_list(Side::Left, "/kaggle/input".into()); }
            "root-left-tmp" => { self.active_side = Side::Left; self.spawn_fs_list(Side::Left, "/tmp".into()); }
            "root-right-working" => { self.active_side = Side::Right; self.spawn_fs_list(Side::Right, "/kaggle/working".into()); }
            "root-right-input" => { self.active_side = Side::Right; self.spawn_fs_list(Side::Right, "/kaggle/input".into()); }
            "root-right-tmp" => { self.active_side = Side::Right; self.spawn_fs_list(Side::Right, "/tmp".into()); }
            "quick-boot" => self.open_workers_modal(1),
            "quick-workers" => self.open_workers_modal(0),
            "quick-files" => self.apply_preset(Preset::DualFiles),
            "quick-hub" => self.apply_preset(Preset::Hub),
            "quick-sync" => self.open_sync_modal(),
            "foot-up" => self.handle_toolbar_action(self.active_side, "up"),
            "foot-open" => self.execute_context_action("open"),
            "foot-sel" => match self.pane_mut(self.active_side) {
                PaneKind::LocalFiles(ref mut lp) => {
                    if let Some(ent) = lp.entries.get(lp.cursor).cloned() {
                        if ent.name != ".." && !lp.selected.remove(&ent.path) {
                            lp.selected.insert(ent.path);
                        }
                    }
                }
                PaneKind::RemoteFiles(ref mut rp) => {
                    if let Some(ent) = rp.entries.get(rp.cursor).cloned() {
                        if ent.name != ".." && !rp.selected.remove(&ent.path) {
                            rp.selected.insert(ent.path);
                        }
                    }
                }
                _ => {}
            },
            "foot-transfer" => self.transfer_to_opposite(),
            "foot-clip-copy" => self.yank_or_cut_files(self.active_side, ClipOp::Copy),
            "foot-clip-paste" => self.paste_files(self.active_side),
            "foot-preview" => self.toggle_or_load_preview_drawer(false),
            "foot-new" => self.execute_context_action("new"),
            "foot-mkdir" => self.open_mkdir_modal(self.active_side),
            "foot-rename" => self.open_path_action(false),
            "foot-del" => self.trigger_delete_selected(),
            "foot-du" => {
                let d = self.active_remote_dir_or_default();
                self.spawn_du(self.active_side, d);
            }
            "foot-dot" => self.execute_context_action("hidden"),
            "foot-find" => self.handle_toolbar_action(self.active_side, "find"),
            "foot-pane" => self.active_side = self.active_side.opposite(),
            "foot-zoom" => self.zoomed = !self.zoomed,
            "foot-swap" => self.modal = Some(Modal::SwapPane(0)),
            "foot-actions" => self.modal = Some(Modal::ActionMenu(0)),
            "foot-help" => self.modal = Some(Modal::Help),
            "foot-tel" => self.show_telemetry = !self.show_telemetry,
            "foot-shell-input" => self.input_focus = InputFocus::ShellPrompt,
            "foot-shell-copy" => self.copy_shell_latest(),
            "foot-shell-stop" => self.interrupt_running_exec(),
            "foot-run-file" => self.spawn_run_remote_file(self.active_side),
            "logs-kill-job" => {
                if !self.selected_job_id.is_empty() {
                    self.spawn_kill_job(self.selected_job_id.clone());
                }
            }
            "foot-spawn-job" => self.execute_context_action("spawn_job"),
            "foot-kill-job" => self.execute_context_action("kill_job"),
            "foot-kill-gpu" => self.trigger_kill_gpu_proc(),
            "foot-log-pause" => self.execute_context_action("pause_logs"),
            "foot-log-copy" => self.execute_context_action("copy_logs"),
            "foot-hub-stage" => self.hub_stage_current(),
            "foot-hub-dl" => self.hub_download_current(),
            "foot-hub-logs" => self.hub_fetch_logs_current(),
            s if s.starts_with("hub-cat-") => {
                if let Ok(idx) = s["hub-cat-".len()..].parse::<usize>() {
                    if idx < HUB_CATEGORIES.len() {
                        self.hub_cat_idx = idx;
                        self.spawn_hub_search();
                    }
                }
            }
            _ => {}
        }
    }

    fn click_breadcrumb(&mut self, side: Side, target: String) {
        match self.pane(side) {
            PaneKind::LocalFiles(_) => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.navigate_into(PathBuf::from(target));
                }
            }
            PaneKind::RemoteFiles(_) => {
                self.navigate_remote_into(side, target, None);
            }
            _ => {}
        }
    }

    fn handle_toolbar_action(&mut self, side: Side, act: &str) {
        match act {
            "up" => match self.pane(side) {
                PaneKind::LocalFiles(_) => {
                    if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                        lp.navigate_up();
                    }
                }
                PaneKind::RemoteFiles(_) => self.navigate_remote_up(side),
                _ => {}
            },
            "open" => self.execute_context_action("open"),
            "transfer" => self.transfer_to_opposite(),
            "new" => self.open_new_file_modal(),
            "mkdir" => self.open_mkdir_modal(side),
            "rename" => self.open_path_action(false),
            "del" => self.trigger_delete_selected(),
            "du" => {
                let d = self.active_remote_dir_or_default();
                self.spawn_du(side, d);
            }
            "find" => match self.pane(side) {
                PaneKind::LocalFiles(_) => self.input_focus = InputFocus::LocalFilter(side),
                PaneKind::RemoteFiles(_) => self.input_focus = InputFocus::RemoteFind(side),
                _ => {}
            },
            "dot" => self.execute_context_action("hidden"),
            "sort" => self.execute_context_action("sort"),
            "refresh" => match self.pane_mut(side) {
                PaneKind::LocalFiles(ref mut lp) => lp.reload(),
                PaneKind::RemoteFiles(rp) => {
                    let d = rp.dir.clone();
                    self.spawn_fs_list(side, d);
                }
                _ => {}
            },
            "swap" => self.modal = Some(Modal::SwapPane(0)),
            "act" => self.modal = Some(Modal::ActionMenu(0)),
            _ => {}
        }
    }
}

