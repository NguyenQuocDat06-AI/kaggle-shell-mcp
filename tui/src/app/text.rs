use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::paste_clipboard;

#[derive(Debug, Clone, Default)]
pub struct TextField {
    pub value: String,
    pub cursor: usize,
    pub sel_anchor: Option<usize>,
}

pub(super) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub(super) fn prev_word_idx(s: &str, col: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = col.min(chars.len());
    while i > 0 && !is_word_char(chars[i - 1]) {
        i -= 1;
    }
    while i > 0 && is_word_char(chars[i - 1]) {
        i -= 1;
    }
    i
}

pub(super) fn next_word_idx(s: &str, col: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = col.min(n);
    if i >= n {
        return n;
    }
    if is_word_char(chars[i]) {
        while i < n && is_word_char(chars[i]) {
            i += 1;
        }
    } else {
        while i < n && !is_word_char(chars[i]) {
            i += 1;
        }
    }
    while i < n && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

impl TextField {
    pub fn new(v: impl Into<String>) -> Self {
        let value = v.into();
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            sel_anchor: None,
        }
    }
    pub fn set(&mut self, v: impl Into<String>) {
        self.value = v.into();
        self.cursor = self.value.chars().count();
        self.sel_anchor = None;
    }
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let a = self.sel_anchor?;
        if a == self.cursor {
            None
        } else {
            Some((a.min(self.cursor), a.max(self.cursor)))
        }
    }
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection_range()?;
        let bs = char_to_byte(&self.value, s);
        let be = char_to_byte(&self.value, e);
        Some(self.value[bs..be].to_string())
    }
    pub fn select_all(&mut self) {
        self.sel_anchor = Some(0);
        self.cursor = self.value.chars().count();
    }
    pub fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection_range() else {
            self.sel_anchor = None;
            return false;
        };
        let bs = char_to_byte(&self.value, s);
        let be = char_to_byte(&self.value, e);
        self.value.replace_range(bs..be, "");
        self.cursor = s;
        self.sel_anchor = None;
        true
    }
    pub fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        let idx = char_to_byte(&self.value, self.cursor);
        self.value.insert(idx, ch);
        self.cursor += 1;
    }
    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        let idx = char_to_byte(&self.value, self.cursor);
        self.value.insert_str(idx, s);
        self.cursor += s.chars().count();
    }
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let end = char_to_byte(&self.value, self.cursor);
        let start = char_to_byte(&self.value, self.cursor - 1);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let len = self.value.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = char_to_byte(&self.value, self.cursor);
        let end = char_to_byte(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
    }
    pub fn move_left_ext(&mut self, select: bool) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
            self.cursor = self.cursor.saturating_sub(1);
        } else if let Some((s, _)) = self.selection_range() {
            self.cursor = s;
            self.sel_anchor = None;
        } else {
            self.sel_anchor = None;
            self.cursor = self.cursor.saturating_sub(1);
        }
    }
    pub fn move_right_ext(&mut self, select: bool) {
        let len = self.value.chars().count();
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
            if self.cursor < len {
                self.cursor += 1;
            }
        } else if let Some((_, e)) = self.selection_range() {
            self.cursor = e;
            self.sel_anchor = None;
        } else {
            self.sel_anchor = None;
            if self.cursor < len {
                self.cursor += 1;
            }
        }
    }
    pub fn move_word_left_ext(&mut self, select: bool) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = prev_word_idx(&self.value, self.cursor);
    }
    pub fn move_word_right_ext(&mut self, select: bool) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = next_word_idx(&self.value, self.cursor);
    }
    pub fn home_ext(&mut self, select: bool) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = 0;
    }
    pub fn end_ext(&mut self, select: bool) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = self.value.chars().count();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TextFieldKeyOutcome {
    Unhandled,
    Moved,
    Changed,
    Copied(String),
    Cut(String),
}

pub(super) fn apply_text_field_key(
    tf: &mut TextField,
    key: KeyEvent,
    fallback_yank: &str,
) -> TextFieldKeyOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('a' | 'A') if ctrl => {
            tf.select_all();
            TextFieldKeyOutcome::Moved
        }
        KeyCode::Char('c' | 'C') if ctrl => {
            if let Some(t) = tf.selected_text() {
                TextFieldKeyOutcome::Copied(t)
            } else if !tf.value.is_empty() {
                TextFieldKeyOutcome::Copied(tf.value.clone())
            } else {
                TextFieldKeyOutcome::Moved
            }
        }
        KeyCode::Char('x' | 'X') if ctrl => {
            if let Some(t) = tf.selected_text() {
                tf.delete_selection();
                TextFieldKeyOutcome::Cut(t)
            } else {
                TextFieldKeyOutcome::Moved
            }
        }
        KeyCode::Char('v' | 'V') if ctrl => {
            let clip_opt = paste_clipboard().filter(|s| !s.is_empty()).or_else(|| {
                if fallback_yank.is_empty() {
                    None
                } else {
                    Some(fallback_yank.to_string())
                }
            });
            if let Some(clip) = clip_opt {
                let clean: String = clip.replace("\r\n", "\n").lines().collect::<Vec<_>>().join(" ");
                tf.insert_str(&clean);
                TextFieldKeyOutcome::Changed
            } else {
                TextFieldKeyOutcome::Moved
            }
        }
        KeyCode::Left => {
            if ctrl {
                tf.move_word_left_ext(shift);
            } else {
                tf.move_left_ext(shift);
            }
            TextFieldKeyOutcome::Moved
        }
        KeyCode::Right => {
            if ctrl {
                tf.move_word_right_ext(shift);
            } else {
                tf.move_right_ext(shift);
            }
            TextFieldKeyOutcome::Moved
        }
        KeyCode::Home => {
            tf.home_ext(shift);
            TextFieldKeyOutcome::Moved
        }
        KeyCode::End => {
            tf.end_ext(shift);
            TextFieldKeyOutcome::Moved
        }
        KeyCode::Backspace => {
            tf.backspace();
            TextFieldKeyOutcome::Changed
        }
        KeyCode::Delete => {
            tf.delete();
            TextFieldKeyOutcome::Changed
        }
        KeyCode::Char(c) if !ctrl => {
            tf.insert_char(c);
            TextFieldKeyOutcome::Changed
        }
        _ => TextFieldKeyOutcome::Unhandled,
    }
}

pub(super) fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[derive(Debug, Clone, Default)]
pub struct EditorModalState {
    pub is_local: bool,
    pub is_new: bool,
    pub path: TextField,
    pub lines: Vec<String>,
    pub cur_row: usize,
    pub cur_col: usize,
    pub sel_anchor: Option<(usize, usize)>,
    pub scroll_row: usize,
    pub readonly: bool,
    pub focus_path: bool,
    pub dirty: bool,
    pub confirm_discard: bool,
    pub saving: bool,
    pub readonly_reason: String,
}

impl EditorModalState {
    pub fn clamp_pos(&self, row: usize, col: usize) -> (usize, usize) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        let r = row.min(self.lines.len() - 1);
        let c = col.min(self.lines[r].chars().count());
        (r, c)
    }

    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.clamp_pos(self.sel_anchor?.0, self.sel_anchor?.1);
        let b = self.clamp_pos(self.cur_row, self.cur_col);
        if a == b {
            None
        } else if a < b {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let ((sr, sc), (er, ec)) = self.selection_range()?;
        if sr == er {
            let line = self.lines.get(sr)?;
            let bs = char_to_byte(line, sc);
            let be = char_to_byte(line, ec);
            return Some(line[bs..be].to_string());
        }
        let mut out = Vec::with_capacity(er - sr + 1);
        let first = &self.lines[sr];
        out.push(first[char_to_byte(first, sc)..].to_string());
        for line in self.lines.iter().take(er).skip(sr + 1) {
            out.push(line.clone());
        }
        let last = &self.lines[er];
        out.push(last[..char_to_byte(last, ec)].to_string());
        Some(out.join("\n"))
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some(((sr, sc), (er, ec))) = self.selection_range() else {
            self.sel_anchor = None;
            return false;
        };
        if sr == er {
            let line = &mut self.lines[sr];
            let bs = char_to_byte(line, sc);
            let be = char_to_byte(line, ec);
            line.replace_range(bs..be, "");
        } else {
            let head = {
                let first = &self.lines[sr];
                first[..char_to_byte(first, sc)].to_string()
            };
            let tail = {
                let last = &self.lines[er];
                last[char_to_byte(last, ec)..].to_string()
            };
            self.lines[sr] = format!("{}{}", head, tail);
            self.lines.drain((sr + 1)..=er);
        }
        self.cur_row = sr;
        self.cur_col = sc;
        self.sel_anchor = None;
        true
    }

    pub fn ensure_scroll(&mut self, view_h: usize) {
        let h = view_h.max(1);
        if self.cur_row < self.scroll_row {
            self.scroll_row = self.cur_row;
        } else if self.cur_row >= self.scroll_row + h {
            self.scroll_row = self.cur_row + 1 - h;
        }
        let max_scroll = self.lines.len().saturating_sub(1);
        if self.scroll_row > max_scroll {
            self.scroll_row = max_scroll;
        }
    }
}

pub(super) fn editor_insert_str(m: &mut EditorModalState, txt: &str) {
    m.delete_selection();
    if m.cur_row >= m.lines.len() {
        m.lines.resize(m.cur_row + 1, String::new());
    }
    let line = &mut m.lines[m.cur_row];
    let byte_idx = char_to_byte(line, m.cur_col);
    line.insert_str(byte_idx, txt);
    m.cur_col += txt.chars().count();
}

pub(super) fn editor_newline(m: &mut EditorModalState) {
    m.delete_selection();
    if m.cur_row >= m.lines.len() {
        m.lines.push(String::new());
        m.cur_row = m.lines.len() - 1;
        m.cur_col = 0;
        return;
    }
    let idx = char_to_byte(&m.lines[m.cur_row], m.cur_col);
    let tail = m.lines[m.cur_row].split_off(idx);
    m.cur_row += 1;
    m.lines.insert(m.cur_row, tail);
    m.cur_col = 0;
}

pub(super) fn editor_backspace(m: &mut EditorModalState) {
    if m.delete_selection() {
        return;
    }
    if m.cur_col > 0 {
        let line = &mut m.lines[m.cur_row];
        let end = char_to_byte(line, m.cur_col);
        let start = char_to_byte(line, m.cur_col - 1);
        line.replace_range(start..end, "");
        m.cur_col -= 1;
    } else if m.cur_row > 0 {
        let tail = m.lines.remove(m.cur_row);
        m.cur_row -= 1;
        m.cur_col = m.lines[m.cur_row].chars().count();
        m.lines[m.cur_row].push_str(&tail);
    }
}

pub(super) fn editor_delete(m: &mut EditorModalState) {
    if m.delete_selection() {
        return;
    }
    if m.cur_row >= m.lines.len() {
        return;
    }
    let line_len = m.lines[m.cur_row].chars().count();
    if m.cur_col < line_len {
        let line = &mut m.lines[m.cur_row];
        let start = char_to_byte(line, m.cur_col);
        let end = char_to_byte(line, m.cur_col + 1);
        line.replace_range(start..end, "");
    } else if m.cur_row + 1 < m.lines.len() {
        let next = m.lines.remove(m.cur_row + 1);
        m.lines[m.cur_row].push_str(&next);
    }
}

