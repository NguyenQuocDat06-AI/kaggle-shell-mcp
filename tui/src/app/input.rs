use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::json;
use std::path::PathBuf;

use crate::state::{delete_profile, save_profile, BootProfile};
use super::text::*;
use super::*;

impl App {
    // ── Input Dispatch (Global -> Modal -> INPUT -> NAV Dual-Panel -> Per-Pane) ─

    pub fn handle_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Modifier(_)) {
            return;
        }
        let is_repeat = key.kind == KeyEventKind::Repeat;
        if key.code == KeyCode::F(8) {
            if !is_repeat {
                self.toggle_mouse_capture();
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q' | 'Q')) {
            if !is_repeat {
                self.should_quit = true;
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
            && !is_repeat
            && !matches!(self.modal, Some(Modal::FileEditor(_)))
        {
            if let Some(sel) = self.selection.take() {
                let txt = self.extract_selected_text(sel);
                self.notify_copied_text(&txt);
                return;
            }
            if self.modal.is_none()
                && self.input_focus == InputFocus::None
                && self.running_exec.is_some()
            {
                self.interrupt_running_exec();
                return;
            }
        }
        if self.modal.is_none()
            && self.input_focus == InputFocus::None
            && self.selection.is_some()
            && !is_repeat
            && key.code == KeyCode::Char('y')
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            if let Some(sel) = self.selection.take() {
                let txt = self.extract_selected_text(sel);
                self.notify_copied_text(&txt);
                return;
            }
        }
        if matches!(self.modal, Some(Modal::SmartInspect(_)))
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
        {
            self.extend_readonly_selection(key.code, key.modifiers.contains(KeyModifiers::CONTROL));
            return;
        }
        if self.modal.is_none()
            && self.input_focus == InputFocus::ShellPrompt
            && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            if matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            ) {
                self.prompt.sel_anchor = None;
                self.extend_readonly_selection(key.code, key.modifiers.contains(KeyModifiers::CONTROL));
                return;
            }
            if self.selection.is_some()
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                )
            {
                self.extend_readonly_selection(key.code, key.modifiers.contains(KeyModifiers::CONTROL));
                return;
            }
        }
        if self.modal.is_none()
            && self.input_focus == InputFocus::None
            && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            if (self.selection.is_some()
                || matches!(
                    self.active_pane(),
                    PaneKind::Shell | PaneKind::LiveLogs | PaneKind::HubDetail
                ))
                && matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                )
            {
                self.extend_readonly_selection(key.code, key.modifiers.contains(KeyModifiers::CONTROL));
                return;
            }
            if self.preview_drawer.is_some()
                && matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                )
            {
                self.extend_readonly_selection(key.code, key.modifiers.contains(KeyModifiers::CONTROL));
                return;
            }
        }
        self.selection = None;
        if key.code == KeyCode::Esc {
            self.last_click_anchor = None;
        }
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return;
        }

        // Global preset & overlay keys (always work, even in INPUT mode)
        match (key.modifiers, key.code) {
            (_, KeyCode::F(1)) => { self.apply_preset(Preset::Cockpit); return; }
            (_, KeyCode::F(2)) => { self.apply_preset(Preset::DualFiles); return; }
            (_, KeyCode::F(3)) => { self.apply_preset(Preset::Jobs); return; }
            (_, KeyCode::F(4)) => { self.apply_preset(Preset::Hub); return; }
            (m, KeyCode::Char('w')) if m.contains(KeyModifiers::CONTROL) && !is_repeat => {
                self.open_workers_modal(0);
                return;
            }
            (_, KeyCode::F(5)) if !is_repeat => {
                self.open_workers_modal(1);
                return;
            }
            (m, KeyCode::Char('k') | KeyCode::Char('p')) if m.contains(KeyModifiers::CONTROL) && !is_repeat => {
                self.modal = Some(Modal::CommandPalette(PaletteModalState {
                    query: TextField::new(""),
                    selected: 0,
                }));
                return;
            }
            (_, KeyCode::Esc) => {
                if self.input_focus != InputFocus::None {
                    self.input_focus = InputFocus::None;
                    return;
                }
                match self.pane_mut(self.active_side) {
                    PaneKind::RemoteFiles(ref mut rp) => {
                        if rp.find_active {
                            rp.find_active = false;
                            return;
                        }
                        if !rp.selected.is_empty() {
                            rp.selected.clear();
                            return;
                        }
                    }
                    PaneKind::LocalFiles(ref mut lp) => {
                        if !lp.filter_input.value.is_empty() {
                            lp.filter_input.set("");
                            lp.reload();
                            return;
                        }
                        if !lp.selected.is_empty() {
                            lp.selected.clear();
                            return;
                        }
                    }
                    _ => {}
                }
                if self.preview_drawer.is_some() {
                    self.preview_drawer = None;
                    self.preview_pending = None;
                    return;
                }
                if self.file_clipboard.is_some() {
                    self.file_clipboard = None;
                    self.notify("Cleared file clipboard", false);
                }
                return;
            }
            _ => {}
        }

        // Active text-input focus
        match self.input_focus {
            InputFocus::ShellPrompt => {
                self.handle_shell_input_key(key);
                return;
            }
            InputFocus::LogFilter => {
                if key.code == KeyCode::Enter {
                    self.input_focus = InputFocus::None;
                    return;
                }
                match apply_text_field_key(&mut self.log_filter, key) {
                    TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                        self.notify_copied_text(&t);
                    }
                    _ => {}
                }
                return;
            }
            InputFocus::LocalFilter(side) => {
                if key.code == KeyCode::Enter {
                    self.input_focus = InputFocus::None;
                    return;
                }
                let outcome = match self.pane_mut(side) {
                    PaneKind::LocalFiles(ref mut lp) => apply_text_field_key(&mut lp.filter_input, key),
                    _ => TextFieldKeyOutcome::Unhandled,
                };
                match outcome {
                    TextFieldKeyOutcome::Changed => {
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.reload();
                        }
                    }
                    TextFieldKeyOutcome::Cut(t) => {
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.reload();
                        }
                        self.notify_copied_text(&t);
                    }
                    TextFieldKeyOutcome::Copied(t) => self.notify_copied_text(&t),
                    _ => {}
                }
                return;
            }
            InputFocus::RemoteFind(side) => {
                if key.code == KeyCode::Enter {
                    self.input_focus = InputFocus::None;
                    let (root, q) = match self.pane(side) {
                        PaneKind::RemoteFiles(rp) => (rp.dir.clone(), rp.find_input.value.trim().to_string()),
                        _ => return,
                    };
                    if q.is_empty() {
                        if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                            rp.find_active = false;
                        }
                    } else {
                        self.spawn_find(side, root, q);
                    }
                    return;
                }
                let outcome = match self.pane_mut(side) {
                    PaneKind::RemoteFiles(ref mut rp) => apply_text_field_key(&mut rp.find_input, key),
                    _ => TextFieldKeyOutcome::Unhandled,
                };
                match outcome {
                    TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                        self.notify_copied_text(&t);
                    }
                    _ => {}
                }
                return;
            }
            InputFocus::HubSearch => {
                if key.code == KeyCode::Enter {
                    self.input_focus = InputFocus::None;
                    self.spawn_hub_search();
                    return;
                }
                match apply_text_field_key(&mut self.hub_search, key) {
                    TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                        self.notify_copied_text(&t);
                    }
                    _ => {}
                }
                return;
            }
            InputFocus::None => {}
        }

        // Shift/Ctrl/Alt + Left/Right ALWAYS switch panels from any pane in NAV mode
        if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT) {
            match key.code {
                KeyCode::Left => {
                    self.active_side = Side::Left;
                    return;
                }
                KeyCode::Right => {
                    self.active_side = Side::Right;
                    return;
                }
                _ => {}
            }
        }

        // Universal Dual-Panel NAV Keys
        match key.code {
            KeyCode::Tab => {
                self.active_side = self.active_side.opposite();
                return;
            }
            KeyCode::Char('z') if !is_repeat => {
                self.zoomed = !self.zoomed;
                return;
            }
            KeyCode::Char('T') if !is_repeat => {
                self.show_telemetry = !self.show_telemetry;
                return;
            }
            KeyCode::Char('m') if !is_repeat => {
                self.modal = Some(Modal::SwapPane(0));
                return;
            }
            KeyCode::Char('a') if !is_repeat => {
                self.modal = Some(Modal::ActionMenu(0));
                return;
            }
            KeyCode::Char('?') if !is_repeat => {
                self.modal = Some(Modal::Help);
                return;
            }
            _ => {}
        }

        // Active Panel Keys (in NAV mode)
        let side = self.active_side;
        match self.pane(side).clone() {
            PaneKind::Shell => match key.code {
                KeyCode::Left => {
                    self.active_side = Side::Left;
                    self.last_click_anchor = None;
                }
                KeyCode::Right => {
                    self.active_side = Side::Right;
                    self.last_click_anchor = None;
                }
                KeyCode::Char('i') | KeyCode::Enter => {
                    self.input_focus = InputFocus::ShellPrompt;
                    self.last_click_anchor = None;
                }
                KeyCode::Char('y') if !is_repeat => self.copy_shell_latest(),
                KeyCode::Char('Y') if !is_repeat => self.copy_shell_all(),
                KeyCode::PageUp | KeyCode::Up | KeyCode::Char('k') => {
                    self.activity_scroll = self.activity_scroll.saturating_sub(4);
                }
                KeyCode::PageDown | KeyCode::Down | KeyCode::Char('j') => {
                    self.activity_scroll = self.activity_scroll.saturating_add(4);
                }
                _ => {}
            },
            PaneKind::LocalFiles(_) => self.handle_local_files_key(side, key, is_repeat),
            PaneKind::RemoteFiles(_) => self.handle_remote_files_key(side, key, is_repeat),
            PaneKind::JobsAndGpu => self.handle_jobs_pane_key(key, is_repeat),
            PaneKind::LiveLogs => self.handle_logs_pane_key(key),
            PaneKind::HubList | PaneKind::HubDetail => self.handle_hub_pane_key(side, key, is_repeat),
        }
    }

    fn handle_shell_input_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
            && self.prompt.selection_range().is_none()
            && self.running_exec.is_some()
        {
            self.interrupt_running_exec();
            return;
        }
        match (key.modifiers, key.code) {
            (m, KeyCode::Enter) if m.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => {
                let cmd = self.prompt.value.trim().to_string();
                self.prompt.set("");
                if !cmd.is_empty() {
                    if self.cmd_history.last().map(|s| s.as_str()) != Some(&cmd) {
                        self.cmd_history.push(cmd.clone());
                    }
                    self.cmd_hist_idx = self.cmd_history.len();
                    self.spawn_job(cmd, String::new(), String::new());
                }
            }
            (_, KeyCode::Enter) => self.submit_prompt(),
            (_, KeyCode::Tab) => {
                if self.prompt.value.starts_with(':') {
                    let matches: Vec<&&str> = SLASH_COMPLETIONS
                        .iter()
                        .filter(|c| c.starts_with(&self.prompt.value))
                        .collect();
                    if matches.len() == 1 {
                        self.prompt.set(*matches[0]);
                    }
                }
            }
            (_, KeyCode::Up) => {
                if !self.cmd_history.is_empty() && self.cmd_hist_idx > 0 {
                    if self.cmd_hist_idx == self.cmd_history.len() {
                        self.cmd_draft = self.prompt.value.clone();
                    }
                    self.cmd_hist_idx -= 1;
                    self.prompt.set(self.cmd_history[self.cmd_hist_idx].clone());
                }
            }
            (_, KeyCode::Down) => {
                if self.cmd_hist_idx < self.cmd_history.len() {
                    self.cmd_hist_idx += 1;
                    let v = if self.cmd_hist_idx == self.cmd_history.len() {
                        self.cmd_draft.clone()
                    } else {
                        self.cmd_history[self.cmd_hist_idx].clone()
                    };
                    self.prompt.set(v);
                }
            }
            (_, KeyCode::PageUp) => self.activity_scroll = self.activity_scroll.saturating_sub(6),
            (_, KeyCode::PageDown) => self.activity_scroll = self.activity_scroll.saturating_add(6),
            _ => match apply_text_field_key(&mut self.prompt, key) {
                TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                    self.notify_copied_text(&t);
                }
                _ => {}
            },
        }
    }

    fn handle_local_files_key(&mut self, side: Side, key: KeyEvent, is_repeat: bool) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.cursor = lp.cursor.saturating_sub(1);
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    if lp.cursor + 1 < lp.entries.len() {
                        lp.cursor += 1;
                    }
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::PageUp => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.cursor = lp.cursor.saturating_sub(10);
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::PageDown => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    if !lp.entries.is_empty() {
                        lp.cursor = (lp.cursor + 10).min(lp.entries.len() - 1);
                    }
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.cursor = 0;
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::End | KeyCode::Char('G') => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.cursor = lp.entries.len().saturating_sub(1);
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                let moved = match self.pane_mut(side) {
                    PaneKind::LocalFiles(ref mut lp) => lp.navigate_up(),
                    _ => false,
                };
                if moved {
                    self.schedule_auto_preview(side);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                enum Next {
                    Up,
                    Into(PathBuf),
                    File,
                    None,
                }
                let next = match self.pane(side) {
                    PaneKind::LocalFiles(lp) => match lp.entries.get(lp.cursor) {
                        Some(ent) if ent.name == ".." => Next::Up,
                        Some(ent) if ent.is_dir => Next::Into(ent.path.clone()),
                        Some(_) => Next::File,
                        None => Next::None,
                    },
                    _ => Next::None,
                };
                match next {
                    Next::Up => {
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.navigate_up();
                        }
                        self.schedule_auto_preview(side);
                    }
                    Next::Into(p) => {
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.navigate_into(p);
                        }
                        self.schedule_auto_preview(side);
                    }
                    Next::File => self.open_preview_or_promote(side),
                    Next::None => {}
                }
            }
            KeyCode::Enter => {
                enum Act {
                    Up,
                    Into(PathBuf),
                    Edit(PathBuf),
                    None,
                }
                let act = match self.pane(side) {
                    PaneKind::LocalFiles(lp) => match lp.entries.get(lp.cursor) {
                        Some(ent) if ent.name == ".." => Act::Up,
                        Some(ent) if ent.is_dir => Act::Into(ent.path.clone()),
                        Some(ent) => Act::Edit(ent.path.clone()),
                        None => Act::None,
                    },
                    _ => Act::None,
                };
                match act {
                    Act::Up => {
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.navigate_up();
                        }
                    }
                    Act::Into(p) => {
                        if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                            lp.navigate_into(p);
                        }
                    }
                    Act::Edit(p) => self.open_local_file_editor(p),
                    Act::None => {}
                }
            }
            KeyCode::Char(' ') => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    if let Some(ent) = lp.entries.get(lp.cursor).cloned() {
                        if ent.name != ".." && !lp.selected.remove(&ent.path) {
                            lp.selected.insert(ent.path);
                        }
                    }
                    if lp.cursor + 1 < lp.entries.len() {
                        lp.cursor += 1;
                    }
                }
            }
            KeyCode::Char('A') if !is_repeat => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    let selectable: Vec<PathBuf> = lp
                        .entries
                        .iter()
                        .filter(|e| e.name != "..")
                        .map(|e| e.path.clone())
                        .collect();
                    if !selectable.is_empty() && lp.selected.len() == selectable.len() {
                        lp.selected.clear();
                    } else {
                        lp.selected = selectable.into_iter().collect();
                    }
                }
            }
            KeyCode::Char('t') if !is_repeat => self.transfer_to_opposite(),
            KeyCode::Char('X') if !is_repeat => self.spawn_run_remote_file(side),
            KeyCode::Char('c') if !is_repeat => self.yank_or_cut_files(side, ClipOp::Copy),
            KeyCode::Char('x') if !is_repeat => self.yank_or_cut_files(side, ClipOp::Cut),
            KeyCode::Char('p') if !is_repeat => self.paste_files(side),
            KeyCode::Char('v') if !is_repeat => self.toggle_or_load_preview_drawer(false),
            KeyCode::Char('/') => self.input_focus = InputFocus::LocalFilter(side),
            KeyCode::Char('n') | KeyCode::Char('N') if !is_repeat => self.open_mkdir_modal(side),
            KeyCode::Char('R') if !is_repeat => self.open_path_action(false),
            KeyCode::Char('d') | KeyCode::Delete if !is_repeat => self.trigger_delete_selected(),
            KeyCode::Char('s') if !is_repeat => {
                let lbl = match self.pane_mut(side) {
                    PaneKind::LocalFiles(ref mut lp) => {
                        lp.sort = lp.sort.next();
                        lp.reload();
                        lp.sort.label()
                    }
                    _ => "Name",
                };
                self.notify(format!("Sort: {}", lbl), false);
            }
            KeyCode::Char('S') if !is_repeat => {
                let desc = match self.pane_mut(side) {
                    PaneKind::LocalFiles(ref mut lp) => {
                        lp.sort_desc = !lp.sort_desc;
                        lp.reload();
                        lp.sort_desc
                    }
                    _ => false,
                };
                self.notify(if desc { "Sort: Descending" } else { "Sort: Ascending" }, false);
            }
            KeyCode::Char('.') if !is_repeat => {
                let shown = match self.pane_mut(side) {
                    PaneKind::LocalFiles(ref mut lp) => {
                        lp.show_hidden = !lp.show_hidden;
                        lp.reload();
                        lp.show_hidden
                    }
                    _ => false,
                };
                self.notify(if shown { "Hidden files shown" } else { "Hidden files hidden" }, false);
            }
            KeyCode::Char('y') if !is_repeat => {
                let p = match self.pane(side) {
                    PaneKind::LocalFiles(lp) => lp
                        .entries
                        .get(lp.cursor)
                        .filter(|e| e.name != "..")
                        .map(|e| e.path.to_string_lossy().to_string())
                        .unwrap_or_else(|| lp.dir.to_string_lossy().to_string()),
                    _ => ".".into(),
                };
                copy_clipboard(&p);
                self.notify(format!("Copied {}", p), false);
            }
            KeyCode::Char('r') if !is_repeat => {
                if let PaneKind::LocalFiles(ref mut lp) = self.pane_mut(side) {
                    lp.reload();
                }
            }
            _ => {}
        }
    }

    fn handle_remote_files_key(&mut self, side: Side, key: KeyEvent, is_repeat: bool) {
        let find_active = match self.pane(side) {
            PaneKind::RemoteFiles(rp) => rp.find_active,
            _ => false,
        };
        if find_active {
            match key.code {
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                    if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                        rp.find_active = false;
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    let hit = match self.pane(side) {
                        PaneKind::RemoteFiles(rp) => rp.find_matches.get(rp.find_cursor).map(|m| m.path.clone()),
                        _ => None,
                    };
                    if let Some(p) = hit {
                        self.spawn_remote_preview(p);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                        rp.find_cursor = rp.find_cursor.saturating_sub(1);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                        if rp.find_cursor + 1 < rp.find_matches.len() {
                            rp.find_cursor += 1;
                        }
                    }
                }
                KeyCode::Enter => {
                    let hit = match self.pane(side) {
                        PaneKind::RemoteFiles(rp) => rp.find_matches.get(rp.find_cursor).map(|m| m.path.clone()),
                        _ => None,
                    };
                    if let Some(p) = hit {
                        self.spawn_open_remote_file(p);
                    }
                }
                KeyCode::Char('/') => self.input_focus = InputFocus::RemoteFind(side),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    rp.cursor = rp.cursor.saturating_sub(1);
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    if rp.cursor + 1 < rp.entries.len() {
                        rp.cursor += 1;
                    }
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::PageUp => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    rp.cursor = rp.cursor.saturating_sub(10);
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::PageDown => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    if !rp.entries.is_empty() {
                        rp.cursor = (rp.cursor + 10).min(rp.entries.len() - 1);
                    }
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    rp.cursor = 0;
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::End | KeyCode::Char('G') => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    rp.cursor = rp.entries.len().saturating_sub(1);
                }
                self.schedule_auto_preview(side);
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                self.navigate_remote_up(side);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let target = match self.pane(side) {
                    PaneKind::RemoteFiles(rp) => rp.entries.get(rp.cursor).cloned(),
                    _ => None,
                };
                if let Some(ent) = target {
                    if ent.name == ".." {
                        self.navigate_remote_up(side);
                    } else if ent.is_dir {
                        self.navigate_remote_into(side, ent.path, Some(ent.name));
                    } else {
                        self.open_preview_or_promote(side);
                    }
                }
            }
            KeyCode::Enter => {
                let target = match self.pane(side) {
                    PaneKind::RemoteFiles(rp) => rp.entries.get(rp.cursor).cloned(),
                    _ => None,
                };
                if let Some(ent) = target {
                    if ent.name == ".." {
                        self.navigate_remote_up(side);
                    } else if ent.is_dir {
                        self.navigate_remote_into(side, ent.path, Some(ent.name));
                    } else {
                        self.spawn_open_remote_file(ent.path);
                    }
                }
            }
            KeyCode::Char(' ') => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    if let Some(ent) = rp.entries.get(rp.cursor).cloned() {
                        if ent.name != ".." && !rp.selected.remove(&ent.path) {
                            rp.selected.insert(ent.path);
                        }
                    }
                    if rp.cursor + 1 < rp.entries.len() {
                        rp.cursor += 1;
                    }
                }
            }
            KeyCode::Char('A') if !is_repeat => {
                if let PaneKind::RemoteFiles(ref mut rp) = self.pane_mut(side) {
                    let selectable: Vec<String> = rp
                        .entries
                        .iter()
                        .filter(|e| e.name != "..")
                        .map(|e| e.path.clone())
                        .collect();
                    if !selectable.is_empty() && rp.selected.len() == selectable.len() {
                        rp.selected.clear();
                    } else {
                        rp.selected = selectable.into_iter().collect();
                    }
                }
            }
            KeyCode::Char('t') if !is_repeat => self.transfer_to_opposite(),
            KeyCode::Char('X') if !is_repeat => self.spawn_run_remote_file(side),
            KeyCode::Char('c') if !is_repeat => self.yank_or_cut_files(side, ClipOp::Copy),
            KeyCode::Char('x') if !is_repeat => self.yank_or_cut_files(side, ClipOp::Cut),
            KeyCode::Char('p') if !is_repeat => self.paste_files(side),
            KeyCode::Char('v') if !is_repeat => self.toggle_or_load_preview_drawer(false),
            KeyCode::Char('/') => self.input_focus = InputFocus::RemoteFind(side),
            KeyCode::Char('n') if !is_repeat => self.open_new_file_modal(),
            KeyCode::Char('N') if !is_repeat => self.open_mkdir_modal(side),
            KeyCode::Char('R') if !is_repeat => self.open_path_action(false),
            KeyCode::Char('u') if !is_repeat => {
                let d = self.active_remote_dir_or_default();
                self.spawn_du(side, d);
            }
            KeyCode::Char('s') if !is_repeat => {
                let lbl = match self.pane_mut(side) {
                    PaneKind::RemoteFiles(ref mut rp) => {
                        rp.sort = rp.sort.next();
                        rp.rebuild_entries();
                        rp.sort.label()
                    }
                    _ => "Name",
                };
                self.notify(format!("Sort: {}", lbl), false);
            }
            KeyCode::Char('S') if !is_repeat => {
                let desc = match self.pane_mut(side) {
                    PaneKind::RemoteFiles(ref mut rp) => {
                        rp.sort_desc = !rp.sort_desc;
                        rp.rebuild_entries();
                        rp.sort_desc
                    }
                    _ => false,
                };
                self.notify(if desc { "Sort: Descending" } else { "Sort: Ascending" }, false);
            }
            KeyCode::Char('.') if !is_repeat => {
                let shown = match self.pane_mut(side) {
                    PaneKind::RemoteFiles(ref mut rp) => {
                        rp.show_hidden = !rp.show_hidden;
                        rp.rebuild_entries();
                        rp.show_hidden
                    }
                    _ => false,
                };
                self.notify(if shown { "Hidden files shown" } else { "Hidden files hidden" }, false);
            }
            KeyCode::Char('r') if !is_repeat => {
                let dir = match self.pane(side) {
                    PaneKind::RemoteFiles(rp) => Some(rp.dir.clone()),
                    _ => None,
                };
                if let Some(d) = dir {
                    self.spawn_fs_list(side, d);
                }
            }
            KeyCode::Char('y') if !is_repeat => {
                let p = match self.pane(side) {
                    PaneKind::RemoteFiles(rp) => rp
                        .entries
                        .get(rp.cursor)
                        .filter(|e| e.name != "..")
                        .map(|e| e.path.clone())
                        .unwrap_or_else(|| rp.dir.clone()),
                    _ => "/kaggle/working".into(),
                };
                copy_clipboard(&p);
                self.notify(format!("Copied {}", p), false);
            }
            KeyCode::Char('d') | KeyCode::Delete if !is_repeat => self.trigger_delete_selected(),
            _ => {}
        }
    }

    // ── Modal Key Handling ────────────────────────────────────────────────────

    fn handle_modal_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Modifier(_)) {
            return;
        }
        let is_repeat = key.kind == KeyEventKind::Repeat;
        if key.code == KeyCode::Esc {
            self.modal = None;
            self.editor_mouse_selecting = false;
            return;
        }
        let Some(modal) = self.modal.take() else { return; };
        match modal {
            Modal::Help => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char('?') | KeyCode::Left) {
                    return;
                }
                self.modal = Some(Modal::Help);
            }
            Modal::SwapPane(mut idx) => {
                match key.code {
                    KeyCode::Left => return,
                    KeyCode::Up | KeyCode::Char('k') => idx = idx.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        if idx + 1 < SWAP_PANE_OPTIONS.len() {
                            idx += 1;
                        }
                    }
                    KeyCode::Enter | KeyCode::Right => {
                        let opt = SWAP_PANE_OPTIONS[idx].0;
                        self.assign_pane(self.active_side, opt);
                        return;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::SwapPane(idx));
            }
            Modal::ActionMenu(mut idx) => {
                let items = self.action_menu_items();
                match key.code {
                    KeyCode::Left => return,
                    KeyCode::Up | KeyCode::Char('k') => idx = idx.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        if idx + 1 < items.len() {
                            idx += 1;
                        }
                    }
                    KeyCode::Enter | KeyCode::Right => {
                        if let Some((id, _)) = items.get(idx) {
                            self.execute_context_action(id);
                            return;
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::ActionMenu(idx));
            }
            Modal::Workers(mut m) => {
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    m.tab = if m.tab == 0 { 1 } else { 0 };
                    self.modal = Some(Modal::Workers(m));
                    return;
                }
                if key.modifiers.contains(KeyModifiers::ALT) {
                    match key.code {
                        KeyCode::Left | KeyCode::Char('1') => {
                            m.tab = 0;
                            self.modal = Some(Modal::Workers(m));
                            return;
                        }
                        KeyCode::Right | KeyCode::Char('2') => {
                            m.tab = 1;
                            self.modal = Some(Modal::Workers(m));
                            return;
                        }
                        _ => {}
                    }
                }
                if m.tab == 0 {
                    match key.code {
                        KeyCode::Right | KeyCode::Char('2') => {
                            m.tab = 1;
                            self.modal = Some(Modal::Workers(m));
                            return;
                        }
                        KeyCode::Left | KeyCode::Char('1') => {}
                        KeyCode::Up | KeyCode::Char('k') => m.cluster_selected = m.cluster_selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            if m.cluster_selected + 1 < m.cluster_rows.len() {
                                m.cluster_selected += 1;
                            }
                        }
                        KeyCode::Char('r') if !is_repeat => {
                            m.cluster_loading = true;
                            self.spawn_cluster_probe();
                            self.spawn_quota();
                        }
                        KeyCode::Enter if !is_repeat => {
                            if let Some(row) = m.cluster_rows.get(m.cluster_selected).cloned() {
                                self.switch_slot(row.slot.clone());
                                self.notify(format!("Switched to slot '{}'", row.slot), false);
                                return;
                            }
                        }
                        KeyCode::Char('x') if !is_repeat => {
                            if let Some(row) = m.cluster_rows.get(m.cluster_selected).cloned() {
                                self.switch_slot(row.slot);
                                self.run_command(":down".into());
                                return;
                            }
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Down => m.field_idx = (m.field_idx + 1) % 8,
                        KeyCode::Up => m.field_idx = (m.field_idx + 7) % 8,
                        KeyCode::Left if matches!(m.field_idx, 0 | 2) => {
                            if m.field_idx == 0 && !m.profiles.is_empty() {
                                m.profile_idx = if m.profile_idx == 0 { m.profiles.len() } else { m.profile_idx - 1 };
                                apply_boot_profile(&mut m);
                            } else if m.field_idx == 2 {
                                m.acc_idx = (m.acc_idx + ACCELERATORS.len() - 1) % ACCELERATORS.len();
                            }
                        }
                        KeyCode::Right if matches!(m.field_idx, 0 | 2) => {
                            if m.field_idx == 0 && !m.profiles.is_empty() {
                                m.profile_idx = (m.profile_idx + 1) % (m.profiles.len() + 1);
                                apply_boot_profile(&mut m);
                            } else if m.field_idx == 2 {
                                m.acc_idx = (m.acc_idx + 1) % ACCELERATORS.len();
                            }
                        }
                        KeyCode::Char(' ') if m.field_idx == 2 => m.chain_prev = !m.chain_prev,
                        KeyCode::Char('s' | 'S') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let pname = m.profile_name.value.trim().to_string();
                            if !pname.is_empty() {
                                let prof = BootProfile {
                                    name: pname,
                                    accelerator: ACCELERATORS[m.acc_idx].0.into(),
                                    slots: m.slots.value.trim().into(),
                                    datasets: split_csv(&m.datasets.value),
                                    models: split_csv(&m.models.value),
                                    competition: m.competition.value.trim().into(),
                                    idle_timeout_min: m.idle_min.value.trim().parse().unwrap_or(0),
                                    chain_previous_output: m.chain_prev,
                                };
                                if let Ok(list) = save_profile(&self.state_dir, prof) {
                                    m.profiles = list;
                                    self.notify("Saved boot profile.", false);
                                }
                            } else {
                                self.notify("Enter name in 'Save As Name' first.", true);
                            }
                        }
                        KeyCode::Char('d' | 'D') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let target = if m.profile_idx > 0 && m.profile_idx <= m.profiles.len() {
                                m.profiles[m.profile_idx - 1].name.clone()
                            } else {
                                m.profile_name.value.trim().to_string()
                            };
                            if let Ok(list) = delete_profile(&self.state_dir, &target) {
                                m.profiles = list;
                                m.profile_idx = 0;
                                m.profile_name.set("");
                                self.notify(format!("Deleted profile '{}'", target), false);
                            }
                        }
                        KeyCode::Enter if !is_repeat => {
                            let acc = ACCELERATORS[m.acc_idx].0.to_string();
                            let slots = if m.slots.value.trim().is_empty() { "1".into() } else { m.slots.value.trim().to_string() };
                            self.spawn_boot(
                                acc,
                                slots,
                                m.datasets.value.trim().into(),
                                m.models.value.trim().into(),
                                m.competition.value.trim().into(),
                                m.idle_min.value.trim().parse().unwrap_or(0),
                                m.chain_prev,
                            );
                            return;
                        }
                        _ => {
                            let skip_non_digit = m.field_idx == 7
                                && matches!(key.code, KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_ascii_digit());
                            if !skip_non_digit {
                                let tf = match m.field_idx {
                                    1 => Some(&mut m.profile_name),
                                    3 => Some(&mut m.slots),
                                    4 => Some(&mut m.datasets),
                                    5 => Some(&mut m.models),
                                    6 => Some(&mut m.competition),
                                    7 => Some(&mut m.idle_min),
                                    _ => None,
                                };
                                if let Some(tf) = tf {
                                    match apply_text_field_key(tf, key) {
                                        TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                            self.notify_copied_text(&t);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                self.modal = Some(Modal::Workers(m));
            }
            Modal::SpawnJob(mut m) => {
                match key.code {
                    KeyCode::Tab | KeyCode::Down => m.field_idx = (m.field_idx + 1) % 3,
                    KeyCode::BackTab | KeyCode::Up => m.field_idx = (m.field_idx + 2) % 3,
                    KeyCode::Enter if !is_repeat => {
                        let cmd = m.command.value.trim().to_string();
                        if cmd.is_empty() {
                            self.notify("Job command cannot be empty.", true);
                            self.modal = Some(Modal::SpawnJob(m));
                            return;
                        }
                        self.spawn_job(cmd, m.gpu_ids.value.trim().into(), m.name.value.trim().into());
                        return;
                    }
                    _ => {
                        let tf = match m.field_idx {
                            0 => &mut m.command,
                            1 => &mut m.gpu_ids,
                            _ => &mut m.name,
                        };
                        match apply_text_field_key(tf, key) {
                            TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                self.notify_copied_text(&t);
                            }
                            _ => {}
                        }
                    }
                }
                self.modal = Some(Modal::SpawnJob(m));
            }
            Modal::Sync(mut m) => {
                match key.code {
                    KeyCode::Tab | KeyCode::Down | KeyCode::Up => m.field_idx = (m.field_idx + 1) % 3,
                    KeyCode::Left | KeyCode::Right if m.field_idx == 0 => m.is_push = !m.is_push,
                    KeyCode::Enter if !is_repeat => {
                        self.spawn_sync(m.is_push, m.local_path.value.clone(), m.remote_path.value.clone());
                        return;
                    }
                    _ if m.field_idx == 1 || m.field_idx == 2 => {
                        let tf = if m.field_idx == 1 { &mut m.local_path } else { &mut m.remote_path };
                        match apply_text_field_key(tf, key) {
                            TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                self.notify_copied_text(&t);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Sync(m));
            }
            Modal::FileEditor(mut m) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                match key.code {
                    KeyCode::Char('s' | 'S') if ctrl => {
                        if m.readonly {
                            self.notify("Read-only file", true);
                            self.modal = Some(Modal::FileEditor(m));
                            return;
                        }
                        let p = m.path.value.trim().to_string();
                        let content = m.lines.join("\n");
                        if m.is_local {
                            let target = PathBuf::from(&p);
                            if let Some(parent) = target.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&target, content.as_bytes()) {
                                Ok(_) => {
                                    self.reload_all_local_panes();
                                    self.notify(format!("Saved local {}", p), false);
                                    return;
                                }
                                Err(e) => self.notify(format!("Save failed: {}", e), true),
                            }
                            self.modal = Some(Modal::FileEditor(m));
                            return;
                        }
                        let Some(sess) = self.active_session() else { return; };
                        let tid = self.begin_task(format!("Save {}", p));
                        let http = self.http.clone();
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            match http.fs_write_text(&sess, &p, &content).await {
                                Ok(_) => { let _ = tx.send(AppMsg::FsOpDone { msg: format!("Saved {}", p), refresh: true }); }
                                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Save failed: {}", e), is_err: true }); }
                            }
                            let _ = tx.send(AppMsg::TaskFinished(tid));
                        });
                        return;
                    }
                    KeyCode::Char('c' | 'C') if ctrl => {
                        if m.focus_path {
                            let txt = m.path.selected_text().unwrap_or_else(|| m.path.value.clone());
                            self.notify_copied_text(&txt);
                        } else if let Some(sel) = m.selected_text() {
                            self.notify_copied_text(&sel);
                        } else {
                            let full = m.lines.join("\n");
                            copy_clipboard(&full);
                            self.notify(format!("Copied full file ({} chars)", full.chars().count()), false);
                        }
                    }
                    KeyCode::Char('x' | 'X') if ctrl => {
                        if m.focus_path {
                            match apply_text_field_key(&mut m.path, key) {
                                TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                    self.notify_copied_text(&t);
                                }
                                _ => {}
                            }
                        } else if !m.readonly {
                            if let Some(sel) = m.selected_text() {
                                self.notify_copied_text(&sel);
                                m.delete_selection();
                            }
                        }
                    }
                    KeyCode::Char('a' | 'A') if ctrl => {
                        if m.focus_path {
                            m.path.select_all();
                        } else {
                            m.sel_anchor = Some((0, 0));
                            let last = m.lines.len().saturating_sub(1);
                            m.cur_row = last;
                            m.cur_col = m.lines.get(last).map(|l| l.chars().count()).unwrap_or(0);
                        }
                    }
                    KeyCode::Char('p' | 'P') if ctrl && !m.is_local => {
                        self.spawn_sync(false, "./kaggle_downloads".into(), m.path.value.clone());
                        return;
                    }
                    KeyCode::Char('d' | 'D') if ctrl && m.is_new => {
                        let p = m.path.value.trim().to_string();
                        if m.is_local {
                            match std::fs::create_dir_all(&p) {
                                Ok(_) => {
                                    self.reload_all_local_panes();
                                    self.notify(format!("Created local dir {}", p), false);
                                    return;
                                }
                                Err(e) => self.notify(format!("mkdir failed: {}", e), true),
                            }
                            self.modal = Some(Modal::FileEditor(m));
                            return;
                        }
                        let Some(sess) = self.active_session() else { return; };
                        let http = self.http.clone();
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            match http.fs_mkdir(&sess, &p).await {
                                Ok(_) => { let _ = tx.send(AppMsg::FsOpDone { msg: format!("Created dir {}", p), refresh: true }); }
                                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("mkdir failed: {}", e), is_err: true }); }
                            }
                        });
                        return;
                    }
                    KeyCode::Char('v' | 'V') if ctrl => {
                        if !m.readonly {
                            if let Some(clip) = paste_clipboard() {
                                if m.focus_path {
                                    let clean: String = clip.replace("\r\n", "\n").lines().collect::<Vec<_>>().join(" ");
                                    m.path.insert_str(&clean);
                                } else {
                                    m.delete_selection();
                                    for (idx, chunk) in clip.replace("\r\n", "\n").split('\n').enumerate() {
                                        if idx > 0 { editor_newline(&mut m); }
                                        editor_insert_str(&mut m, chunk);
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Tab if m.is_new => m.focus_path = !m.focus_path,
                    _ if m.focus_path => {
                        if key.code == KeyCode::Enter || (key.code == KeyCode::Down && !shift) {
                            m.focus_path = false;
                        } else {
                            match apply_text_field_key(&mut m.path, key) {
                                TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                    self.notify_copied_text(&t);
                                }
                                _ => {}
                            }
                        }
                    }
                    KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                    | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                        let prev_sel = m.selection_range();
                        if shift {
                            if m.sel_anchor.is_none() {
                                m.sel_anchor = Some((m.cur_row, m.cur_col));
                            }
                        } else {
                            m.sel_anchor = None;
                        }
                        match key.code {
                            KeyCode::Up => {
                                if m.cur_row > 0 {
                                    m.cur_row -= 1;
                                    let line_len = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                    m.cur_col = m.cur_col.min(line_len);
                                } else if shift {
                                    m.cur_col = 0;
                                }
                            }
                            KeyCode::Down => {
                                if m.cur_row + 1 < m.lines.len() {
                                    m.cur_row += 1;
                                    let line_len = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                    m.cur_col = m.cur_col.min(line_len);
                                } else if shift {
                                    m.cur_col = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                }
                            }
                            KeyCode::PageUp => {
                                if m.cur_row == 0 && shift {
                                    m.cur_col = 0;
                                } else {
                                    m.cur_row = m.cur_row.saturating_sub(12);
                                    let line_len = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                    m.cur_col = m.cur_col.min(line_len);
                                }
                            }
                            KeyCode::PageDown => {
                                let last = m.lines.len().saturating_sub(1);
                                if m.cur_row >= last && shift {
                                    m.cur_col = m.lines.get(last).map(|l| l.chars().count()).unwrap_or(0);
                                } else {
                                    m.cur_row = (m.cur_row + 12).min(last);
                                    let line_len = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                    m.cur_col = m.cur_col.min(line_len);
                                }
                            }
                            KeyCode::Left => {
                                if !shift && !ctrl {
                                    if let Some(((sr, sc), _)) = prev_sel {
                                        m.cur_row = sr;
                                        m.cur_col = sc;
                                    } else if m.cur_col > 0 {
                                        m.cur_col -= 1;
                                    } else if m.cur_row > 0 {
                                        m.cur_row -= 1;
                                        m.cur_col = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                    }
                                } else if ctrl {
                                    if m.cur_col > 0 {
                                        let line = m.lines.get(m.cur_row).map(|s| s.as_str()).unwrap_or("");
                                        m.cur_col = prev_word_idx(line, m.cur_col);
                                    } else if m.cur_row > 0 {
                                        m.cur_row -= 1;
                                        m.cur_col = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                    }
                                } else if m.cur_col > 0 {
                                    m.cur_col -= 1;
                                } else if m.cur_row > 0 {
                                    m.cur_row -= 1;
                                    m.cur_col = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                }
                            }
                            KeyCode::Right => {
                                let line_len = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                                if !shift && !ctrl {
                                    if let Some((_, (er, ec))) = prev_sel {
                                        m.cur_row = er;
                                        m.cur_col = ec;
                                    } else if m.cur_col < line_len {
                                        m.cur_col += 1;
                                    } else if m.cur_row + 1 < m.lines.len() {
                                        m.cur_row += 1;
                                        m.cur_col = 0;
                                    }
                                } else if ctrl {
                                    if m.cur_col < line_len {
                                        let line = m.lines.get(m.cur_row).map(|s| s.as_str()).unwrap_or("");
                                        m.cur_col = next_word_idx(line, m.cur_col);
                                    } else if m.cur_row + 1 < m.lines.len() {
                                        m.cur_row += 1;
                                        m.cur_col = 0;
                                    }
                                } else if m.cur_col < line_len {
                                    m.cur_col += 1;
                                } else if m.cur_row + 1 < m.lines.len() {
                                    m.cur_row += 1;
                                    m.cur_col = 0;
                                }
                            }
                            KeyCode::Home => {
                                if ctrl {
                                    m.cur_row = 0;
                                }
                                m.cur_col = 0;
                            }
                            KeyCode::End => {
                                if ctrl {
                                    m.cur_row = m.lines.len().saturating_sub(1);
                                }
                                m.cur_col = m.lines.get(m.cur_row).map(|l| l.chars().count()).unwrap_or(0);
                            }
                            _ => {}
                        }
                    }
                    KeyCode::Enter if !m.readonly => editor_newline(&mut m),
                    KeyCode::Backspace if !m.readonly => editor_backspace(&mut m),
                    KeyCode::Delete if !m.readonly => editor_delete(&mut m),
                    KeyCode::Char(c) if !m.readonly && !ctrl => {
                        let mut s = [0u8; 4];
                        editor_insert_str(&mut m, c.encode_utf8(&mut s));
                    }
                    _ => {}
                }
                self.modal = Some(Modal::FileEditor(m));
            }
            Modal::PathAction(mut m) => {
                match key.code {
                    KeyCode::Tab if m.kind != PathActionKind::Mkdir && !m.readonly_src => {
                        m.kind = match m.kind {
                            PathActionKind::Rename => PathActionKind::Copy,
                            PathActionKind::Copy => PathActionKind::Rename,
                            PathActionKind::Mkdir => PathActionKind::Mkdir,
                        };
                    }
                    KeyCode::Enter if !is_repeat => {
                        let dst = m.dst_path.value.trim().to_string();
                        if dst.is_empty() {
                            self.notify("Destination path cannot be empty.", true);
                            self.modal = Some(Modal::PathAction(m));
                            return;
                        }
                        let src = m.src_path;
                        if m.is_local {
                            let sp = PathBuf::from(&src);
                            let dp = PathBuf::from(&dst);
                            let res: std::io::Result<()> = match m.kind {
                                PathActionKind::Mkdir => std::fs::create_dir_all(&dp),
                                PathActionKind::Copy => {
                                    if sp.is_dir() {
                                        copy_dir_recursive(&sp, &dp)
                                    } else {
                                        if let Some(parent) = dp.parent() {
                                            let _ = std::fs::create_dir_all(parent);
                                        }
                                        std::fs::copy(&sp, &dp).map(|_| ())
                                    }
                                }
                                PathActionKind::Rename => {
                                    if let Some(parent) = dp.parent() {
                                        let _ = std::fs::create_dir_all(parent);
                                    }
                                    std::fs::rename(&sp, &dp)
                                }
                            };
                            match res {
                                Ok(_) => {
                                    self.reload_all_local_panes();
                                    self.notify(format!("Done -> {}", dst), false);
                                }
                                Err(e) => self.notify(format!("Path op failed: {}", e), true),
                            }
                            return;
                        }
                        let Some(sess) = self.active_session() else { return; };
                        let kind = if m.readonly_src && m.kind == PathActionKind::Rename {
                            PathActionKind::Copy
                        } else {
                            m.kind
                        };
                        let tid = self.begin_task(format!("Path op {}", dst));
                        let http = self.http.clone();
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            let res = match kind {
                                PathActionKind::Mkdir => http.fs_mkdir(&sess, &dst).await,
                                PathActionKind::Copy => http.fs_copy(&sess, &src, &dst).await,
                                PathActionKind::Rename => http.fs_rename(&sess, &src, &dst).await,
                            };
                            match res {
                                Ok(_) => {
                                    let _ = tx.send(AppMsg::FsOpDone {
                                        msg: format!("Updated -> {}", dst),
                                        refresh: true,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(AppMsg::Toast {
                                        text: format!("Path op failed: {}", e),
                                        is_err: true,
                                    });
                                }
                            }
                            let _ = tx.send(AppMsg::TaskFinished(tid));
                        });
                        return;
                    }
                    _ => match apply_text_field_key(&mut m.dst_path, key) {
                        TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                            self.notify_copied_text(&t);
                        }
                        _ => {}
                    },
                }
                self.modal = Some(Modal::PathAction(m));
            }
            Modal::SmartInspect(mut m) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => m.scroll = m.scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => m.scroll = m.scroll.saturating_add(1),
                    KeyCode::PageUp => m.scroll = m.scroll.saturating_sub(10),
                    KeyCode::PageDown => m.scroll = m.scroll.saturating_add(10),
                    KeyCode::Char('p') if !is_repeat => {
                        self.spawn_sync(false, "./kaggle_downloads".into(), m.info.path);
                        return;
                    }
                    KeyCode::Char('e') if m.info.kind == "archive" && !is_repeat => {
                        let Some(sess) = self.active_session() else { return; };
                        let tid = self.begin_task(format!("Extract {}", m.info.path));
                        let p = m.info.path;
                        let http = self.http.clone();
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            match http.fs_extract(&sess, &p, "").await {
                                Ok(v) => {
                                    let cnt = v.get("count").and_then(|x| x.as_u64()).unwrap_or(0);
                                    let _ = tx.send(AppMsg::FsOpDone { msg: format!("Extracted {} entries", cnt), refresh: true });
                                }
                                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Extract failed: {}", e), is_err: true }); }
                            }
                            let _ = tx.send(AppMsg::TaskFinished(tid));
                        });
                        return;
                    }
                    KeyCode::Char('s') if m.info.path.to_ascii_lowercase().ends_with(".csv") && !is_repeat => {
                        let comp = self.default_competition();
                        self.modal = Some(Modal::SubmitCompetition(SubmitCompModalState {
                            path: m.info.path,
                            is_remote: true,
                            competition: TextField::new(comp),
                            message: TextField::new("Submitted via kgsh"),
                            field_idx: 0,
                        }));
                        return;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::SmartInspect(m));
            }
            Modal::PublishDataset(mut m) => {
                match key.code {
                    KeyCode::Tab | KeyCode::Down => m.field_idx = (m.field_idx + 1) % 6,
                    KeyCode::BackTab | KeyCode::Up => m.field_idx = (m.field_idx + 5) % 6,
                    KeyCode::Char(' ') if m.field_idx == 4 => m.public = !m.public,
                    KeyCode::Char(' ') if m.field_idx == 5 => m.stage_after = !m.stage_after,
                    KeyCode::Enter if !is_repeat => {
                        let Some(sess) = self.active_session() else { return; };
                        let src = m.source_dir.value.clone();
                        let tid = self.begin_task(format!("Publish {}", src));
                        let http = self.http.clone();
                        let tx = self.tx.clone();
                        let slug = m.slug.value.clone();
                        let title = m.title.value.clone();
                        let notes = m.notes.value.clone();
                        let public = m.public;
                        let stage_after = m.stage_after;
                        self.notify(format!("Publishing {}...", src), false);
                        tokio::spawn(async move {
                            match http.fs_publish_dataset(&sess, &src, &slug, &title, &notes, public).await {
                                Ok(v) => {
                                    let dref = v.get("dataset_ref").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                    let _ = tx.send(AppMsg::FsPublishDone { dataset_ref: dref, url, stage_after });
                                }
                                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Publish failed: {}", e), is_err: true }); }
                            }
                            let _ = tx.send(AppMsg::TaskFinished(tid));
                        });
                        return;
                    }
                    _ => {
                        let tf = match m.field_idx {
                            0 => Some(&mut m.source_dir),
                            1 => Some(&mut m.slug),
                            2 => Some(&mut m.title),
                            3 => Some(&mut m.notes),
                            _ => None,
                        };
                        if let Some(tf) = tf {
                            match apply_text_field_key(tf, key) {
                                TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                    self.notify_copied_text(&t);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                self.modal = Some(Modal::PublishDataset(m));
            }
            Modal::SubmitCompetition(mut m) => {
                match key.code {
                    KeyCode::Tab | KeyCode::Down | KeyCode::Up => m.field_idx = (m.field_idx + 1) % 2,
                    KeyCode::Enter if !is_repeat => {
                        let comp = m.competition.value.trim().to_string();
                        if comp.is_empty() {
                            self.notify("Competition slug required.", true);
                            self.modal = Some(Modal::SubmitCompetition(m));
                            return;
                        }
                        let msg = m.message.value.trim().to_string();
                        let path = m.path.clone();
                        let is_remote = m.is_remote;
                        let sess = self.active_session();
                        let tid = self.begin_task(format!("Submit {}", comp));
                        let http = self.http.clone();
                        let bridge = self.bridge.clone();
                        let tx = self.tx.clone();
                        self.notify(format!("Submitting CSV to {}...", comp), false);
                        tokio::spawn(async move {
                            let local_csv = if is_remote {
                                let Some(s) = sess else {
                                    let _ = tx.send(AppMsg::Toast { text: "No active session".into(), is_err: true });
                                    let _ = tx.send(AppMsg::TaskFinished(tid));
                                    return;
                                };
                                let dl_dir = PathBuf::from("./kaggle_downloads/_submissions");
                                if let Err(e) = http.fs_pull(&s, &path, &dl_dir).await {
                                    let _ = tx.send(AppMsg::Toast { text: format!("Pull CSV failed: {}", e), is_err: true });
                                    let _ = tx.send(AppMsg::TaskFinished(tid));
                                    return;
                                }
                                let fname = Path::new(&path).file_name().unwrap_or_default();
                                dl_dir.join(fname).to_string_lossy().to_string()
                            } else {
                                path
                            };
                            match bridge
                                .call("submit_csv", json!({ "competition": comp, "local_csv_path": local_csv, "message": msg }))
                                .await
                            {
                                Ok(v) => {
                                    let res = v.get("result").and_then(|x| x.as_str()).unwrap_or("OK").to_string();
                                    let cslug = v.get("competition").and_then(|x| x.as_str()).unwrap_or(&comp).to_string();
                                    let _ = tx.send(AppMsg::SubmitCsvDone { competition: cslug, result: res });
                                }
                                Err(e) => { let _ = tx.send(AppMsg::Toast { text: format!("Submit failed: {}", e), is_err: true }); }
                            }
                            let _ = tx.send(AppMsg::TaskFinished(tid));
                        });
                        return;
                    }
                    _ => {
                        let tf = if m.field_idx == 0 { &mut m.competition } else { &mut m.message };
                        match apply_text_field_key(tf, key) {
                            TextFieldKeyOutcome::Copied(t) | TextFieldKeyOutcome::Cut(t) => {
                                self.notify_copied_text(&t);
                            }
                            _ => {}
                        }
                    }
                }
                self.modal = Some(Modal::SubmitCompetition(m));
            }
            Modal::CommandPalette(mut m) => {
                let items = filtered_palette_items(&m.query.value);
                match key.code {
                    KeyCode::Up => m.selected = m.selected.saturating_sub(1),
                    KeyCode::Down => {
                        if m.selected + 1 < items.len() { m.selected += 1; }
                    }
                    KeyCode::Enter if !is_repeat => {
                        if let Some((_, _, cmd)) = items.get(m.selected) {
                            let c = cmd.to_string();
                            self.execute_palette_cmd(&c);
                            return;
                        }
                    }
                    _ => match apply_text_field_key(&mut m.query, key) {
                        TextFieldKeyOutcome::Changed => m.selected = 0,
                        TextFieldKeyOutcome::Cut(t) => {
                            m.selected = 0;
                            self.notify_copied_text(&t);
                        }
                        TextFieldKeyOutcome::Copied(t) => self.notify_copied_text(&t),
                        _ => {}
                    },
                }
                self.modal = Some(Modal::CommandPalette(m));
            }
        }
    }

}

fn apply_boot_profile(m: &mut WorkersModalState) {
    if m.profile_idx == 0 || m.profile_idx > m.profiles.len() {
        return;
    }
    let p = m.profiles[m.profile_idx - 1].clone();
    m.acc_idx = ACCELERATORS
        .iter()
        .position(|(code, _)| *code == p.accelerator)
        .unwrap_or(0);
    m.slots.set(p.slots);
    m.datasets.set(p.datasets.join(", "));
    m.models.set(p.models.join(", "));
    m.competition.set(p.competition);
    m.idle_min.set(p.idle_timeout_min.to_string());
    m.chain_prev = p.chain_previous_output;
    m.profile_name.set(p.name);
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

