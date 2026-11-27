use crossterm::event::KeyCode;
use ratatui::layout::Rect;

use super::text::is_word_char;
use super::*;

impl App {
    pub fn selection_target_for(&self, x: u16, y: u16) -> Option<(u16, u16, Rect, SelectionKind)> {
        fn inset1(r: Rect) -> Rect {
            if r.width > 2 && r.height > 2 {
                Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2)
            } else {
                r
            }
        }
        if let Some(ref modal) = self.modal {
            match modal {
                Modal::FileEditor(_) => return None,
                Modal::SmartInspect(_) => {
                    if rect_contains(self.rects.inspect_content_rect, x, y) {
                        return Some((x, y, self.rects.inspect_content_rect, SelectionKind::Plain));
                    }
                    return None;
                }
                _ => {
                    if rect_contains(self.rects.modal_rect, x, y) {
                        let b = inset1(self.rects.modal_rect);
                        if rect_contains(b, x, y) {
                            return Some((x, y, b, SelectionKind::Plain));
                        }
                    }
                    return None;
                }
            }
        }
        if rect_contains(self.rects.drawer_rect, x, y) {
            if rect_contains(self.rects.preview_body_rect, x, y) && self.rects.preview_code_rect.width > 0 {
                let cx = x.max(self.rects.preview_code_rect.x);
                return Some((cx, y, self.rects.preview_code_rect, SelectionKind::PreviewCode));
            }
            return None;
        }
        if rect_contains(self.rects.shell_content_rect, x, y) {
            return Some((x, y, self.rects.shell_content_rect, SelectionKind::ShellStream));
        }
        if rect_contains(self.rects.logs_content_rect, x, y) {
            return Some((x, y, self.rects.logs_content_rect, SelectionKind::Plain));
        }
        if rect_contains(self.rects.hub_detail_rect, x, y) {
            return Some((x, y, self.rects.hub_detail_rect, SelectionKind::Plain));
        }
        if rect_contains(self.rects.left_list_rect, x, y) {
            return Some((x, y, self.rects.left_list_rect, SelectionKind::Plain));
        }
        if rect_contains(self.rects.right_list_rect, x, y) {
            return Some((x, y, self.rects.right_list_rect, SelectionKind::Plain));
        }
        None
    }

    pub fn shell_gutter_width(buf: &ratatui::buffer::Buffer, bounds_x: u16, row: u16, max_x: u16) -> u16 {
        let Some(c0) = buf.cell((bounds_x, row)) else {
            return 0;
        };
        if c0.symbol() != "│" {
            return 0;
        }
        let mut spaces = 0u16;
        for off in 1..=3 {
            let col = bounds_x.saturating_add(off);
            if col > max_x {
                break;
            }
            if buf.cell((col, row)).map(|c| c.symbol() == " ").unwrap_or(false) {
                spaces += 1;
            } else {
                break;
            }
        }
        if spaces >= 3 {
            4
        } else if spaces >= 1 {
            2
        } else {
            1
        }
    }

    pub fn extract_selected_text(&self, sel: TextSelection) -> String {
        let Some(ref buf) = self.screen_buf else {
            return String::new();
        };
        let ((sx, sy), (ex, ey)) = if (sel.start.1, sel.start.0) <= (sel.end.1, sel.end.0) {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };
        let mut out = Vec::new();
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
                let gw = Self::shell_gutter_width(buf, sel.bounds.x, row, max_x);
                cs = cs.max(sel.bounds.x.saturating_add(gw));
            }
            if cs > ce {
                out.push(String::new());
                continue;
            }
            let mut line = String::new();
            for col in cs..=ce {
                if let Some(cell) = buf.cell((col, row)) {
                    let sym = cell.symbol();
                    if !sym.is_empty() {
                        line.push_str(sym);
                    }
                }
            }
            out.push(line.trim_end().to_string());
        }
        while out.last().map(|s| s.is_empty()).unwrap_or(false) {
            out.pop();
        }
        out.join("\n")
    }

    pub fn notify_copied_text(&mut self, txt: &str) {
        if txt.is_empty() {
            return;
        }
        let lines_cnt = txt.lines().count();
        let n = txt.chars().count();
        self.set_clipboard_text(txt);
        let msg = if lines_cnt > 1 {
            format!("Copied {} chars ({} lines)", n, lines_cnt)
        } else {
            format!("Copied {} chars", n)
        };
        self.notify(msg, false);
    }

    pub fn extend_readonly_selection(&mut self, code: KeyCode, ctrl: bool) {
        let Some(buf) = self.screen_buf.as_ref() else {
            return;
        };
        let (bounds, kind, scan_bottom_up, click_pos) = if let Some(s) = self.selection {
            if s.bounds.width > 0 && s.bounds.height > 0 {
                (s.bounds, s.kind, true, None)
            } else {
                return;
            }
        } else if matches!(self.modal, Some(Modal::SmartInspect(_)))
            && self.rects.inspect_content_rect.width > 0
            && self.rects.inspect_content_rect.height > 0
        {
            (self.rects.inspect_content_rect, SelectionKind::Plain, false, None)
        } else if let Some((ax, ay, b, k)) = self.last_click_anchor {
            if b.width > 0 && b.height > 0 {
                (b, k, false, Some((ax, ay)))
            } else {
                return;
            }
        } else {
            match self.active_pane() {
                PaneKind::Shell if self.rects.shell_content_rect.width > 0 && self.rects.shell_content_rect.height > 0 => {
                    (self.rects.shell_content_rect, SelectionKind::ShellStream, true, None)
                }
                PaneKind::LiveLogs if self.rects.logs_content_rect.width > 0 && self.rects.logs_content_rect.height > 0 => {
                    (self.rects.logs_content_rect, SelectionKind::Plain, true, None)
                }
                PaneKind::HubDetail if self.rects.hub_detail_rect.width > 0 && self.rects.hub_detail_rect.height > 0 => {
                    (self.rects.hub_detail_rect, SelectionKind::Plain, false, None)
                }
                _ if self.preview_drawer.is_some() && self.rects.preview_code_rect.width > 0 => {
                    (self.rects.preview_code_rect, SelectionKind::PreviewCode, false, None)
                }
                _ => return,
            }
        };
        if bounds.width == 0 || bounds.height == 0 {
            return;
        }
        let max_y = buf.area().bottom().saturating_sub(1);
        let max_x = buf.area().right().saturating_sub(1);
        let top_y = bounds.y.min(max_y);
        let bot_y = bounds.bottom().saturating_sub(1).min(max_y);
        let right_x = bounds.right().saturating_sub(1).min(max_x);
        if top_y > bot_y || bounds.x > right_x {
            return;
        }

        let content_start = |row: u16| -> u16 {
            if kind == SelectionKind::ShellStream {
                bounds.x.saturating_add(Self::shell_gutter_width(buf, bounds.x, row, max_x)).min(right_x)
            } else {
                bounds.x
            }
        };
        let row_last_non_ws = |row: u16| -> Option<u16> {
            let cs = content_start(row);
            for col in (cs..=right_x).rev() {
                if let Some(cell) = buf.cell((col, row)) {
                    if cell.symbol().chars().any(|c| !c.is_whitespace()) {
                        return Some(col);
                    }
                }
            }
            None
        };

        let mut sel = match self.selection {
            Some(s) if s.bounds == bounds && s.kind == kind => s,
            _ => {
                let init_row = if let Some((_, cy)) = click_pos {
                    cy.clamp(top_y, bot_y)
                } else if scan_bottom_up {
                    (top_y..=bot_y).rev().find(|&r| row_last_non_ws(r).is_some()).unwrap_or(bot_y)
                } else {
                    (top_y..=bot_y).find(|&r| row_last_non_ws(r).is_some()).unwrap_or(top_y)
                };
                let cs = content_start(init_row);
                let anchor_x = click_pos
                    .map(|(cx, _)| cx.clamp(cs, right_x))
                    .unwrap_or_else(|| row_last_non_ws(init_row).unwrap_or(cs));
                let is_line = matches!(
                    code,
                    KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                );
                let (start, end) = match code {
                    KeyCode::Left => ((anchor_x, init_row), (anchor_x.saturating_sub(1).max(cs), init_row)),
                    KeyCode::Right => {
                        let start_x = click_pos.map(|(cx, _)| cx.clamp(cs, right_x)).unwrap_or(cs);
                        ((start_x, init_row), (start_x.saturating_add(1).min(right_x), init_row))
                    }
                    KeyCode::Home => {
                        let start_x = click_pos.map(|(cx, _)| cx.clamp(cs, right_x)).unwrap_or(anchor_x);
                        ((start_x, init_row), (cs, init_row))
                    }
                    KeyCode::End => {
                        let start_x = click_pos.map(|(cx, _)| cx.clamp(cs, right_x)).unwrap_or(cs);
                        ((start_x, init_row), (right_x, init_row))
                    }
                    _ => ((cs, init_row), (right_x, init_row)),
                };
                self.selection = Some(TextSelection {
                    start,
                    end,
                    bounds,
                    dragging: false,
                    kind,
                    line_mode: is_line,
                });
                return;
            }
        };

        match code {
            KeyCode::Up | KeyCode::PageUp => {
                let step: u16 = if code == KeyCode::PageUp { 6 } else { 1 };
                sel.end.1 = sel.end.1.saturating_sub(step).max(top_y);
                if sel.line_mode {
                    if sel.end.1 < sel.start.1 {
                        sel.start.0 = right_x;
                        sel.end.0 = content_start(sel.end.1);
                    } else {
                        sel.start.0 = content_start(sel.start.1);
                        sel.end.0 = right_x;
                    }
                } else {
                    sel.end.0 = sel.end.0.clamp(content_start(sel.end.1), right_x);
                }
            }
            KeyCode::Down | KeyCode::PageDown => {
                let step: u16 = if code == KeyCode::PageDown { 6 } else { 1 };
                sel.end.1 = sel.end.1.saturating_add(step).min(bot_y);
                if sel.line_mode {
                    if sel.end.1 < sel.start.1 {
                        sel.start.0 = right_x;
                        sel.end.0 = content_start(sel.end.1);
                    } else {
                        sel.start.0 = content_start(sel.start.1);
                        sel.end.0 = right_x;
                    }
                } else {
                    sel.end.0 = sel.end.0.clamp(content_start(sel.end.1), right_x);
                }
            }
            KeyCode::Left => {
                sel.line_mode = false;
                let cs = content_start(sel.end.1);
                if ctrl {
                    let mut c = sel.end.0;
                    let is_wc = |col: u16| -> bool {
                        buf.cell((col, sel.end.1))
                            .and_then(|cell| cell.symbol().chars().next())
                            .map(is_word_char)
                            .unwrap_or(false)
                    };
                    while c > cs && !is_wc(c.saturating_sub(1)) {
                        c -= 1;
                    }
                    while c > cs && is_wc(c.saturating_sub(1)) {
                        c -= 1;
                    }
                    sel.end.0 = c;
                } else {
                    sel.end.0 = sel.end.0.saturating_sub(1).max(cs);
                }
            }
            KeyCode::Right => {
                sel.line_mode = false;
                let cs = content_start(sel.end.1);
                if ctrl {
                    let mut c = sel.end.0.max(cs);
                    let is_wc = |col: u16| -> bool {
                        buf.cell((col, sel.end.1))
                            .and_then(|cell| cell.symbol().chars().next())
                            .map(is_word_char)
                            .unwrap_or(false)
                    };
                    if is_wc(c) {
                        while c < right_x && is_wc(c) {
                            c += 1;
                        }
                    } else {
                        while c < right_x && !is_wc(c) {
                            c += 1;
                        }
                    }
                    while c < right_x
                        && buf
                            .cell((c, sel.end.1))
                            .and_then(|cell| cell.symbol().chars().next())
                            .map(|ch| ch.is_whitespace())
                            .unwrap_or(false)
                    {
                        c += 1;
                    }
                    sel.end.0 = c;
                } else if sel.end.0 < right_x {
                    sel.end.0 += 1;
                }
            }
            KeyCode::Home => {
                sel.line_mode = false;
                if ctrl {
                    sel.end = (content_start(top_y), top_y);
                } else {
                    sel.end.0 = content_start(sel.end.1);
                }
            }
            KeyCode::End => {
                sel.line_mode = false;
                if ctrl {
                    sel.end = (right_x, bot_y);
                } else {
                    sel.end.0 = right_x;
                }
            }
            _ => {}
        }
        sel.dragging = false;
        self.selection = Some(sel);
    }

    pub fn copy_shell_latest(&mut self) {
        let txt = if let Some(ref run) = self.running_exec {
            if run.live_output.trim().is_empty() {
                format!("$ {}", run.command)
            } else {
                run.live_output.clone()
            }
        } else if let Some(ev) = self.activities.back() {
            if ev.detail.trim().is_empty() {
                ev.summary.clone()
            } else {
                ev.detail.clone()
            }
        } else {
            self.notify("No shell output to copy.", true);
            return;
        };
        let n = txt.chars().count();
        self.set_clipboard_text(&txt);
        self.notify(format!("Copied {} chars", n), false);
    }

    pub fn copy_shell_all(&mut self) {
        let mut parts = Vec::new();
        for ev in &self.activities {
            parts.push(format!("[{}] {}", ev.caller, ev.summary));
            if !ev.detail.trim().is_empty() {
                parts.push(ev.detail.clone());
            }
        }
        if let Some(ref run) = self.running_exec {
            parts.push(format!("[running] $ {}", run.command));
            if !run.live_output.trim().is_empty() {
                parts.push(run.live_output.clone());
            }
        }
        let txt = parts.join("\n");
        if txt.is_empty() {
            self.notify("No shell output to copy.", true);
            return;
        }
        let n = txt.chars().count();
        self.set_clipboard_text(&txt);
        self.notify(format!("Copied {} shell chars", n), false);
    }

}
