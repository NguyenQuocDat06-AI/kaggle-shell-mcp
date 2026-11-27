//! Calm monochrome-first file glyphs, UTF-8 safe truncation, mtime, DU bars, and 3-tone preview highlighting.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const BG: Color = Color::Rgb(16, 18, 21);
pub const SURFACE: Color = Color::Rgb(23, 26, 32);
pub const BORDER: Color = Color::Rgb(50, 56, 68);
pub const BORDER_ACTIVE: Color = Color::Rgb(100, 181, 246);
pub const TEXT: Color = Color::Rgb(224, 228, 235);
pub const MUTED: Color = Color::Rgb(122, 130, 145);
pub const ACCENT: Color = Color::Rgb(100, 181, 246);
pub const OK: Color = Color::Rgb(74, 222, 128);
pub const WARN: Color = Color::Rgb(234, 179, 8);
pub const ERR: Color = Color::Rgb(239, 68, 68);
pub const SEL_BG: Color = Color::Rgb(39, 48, 66);
pub const SEL_GHOST_BG: Color = Color::Rgb(28, 32, 40);
pub const MARKED_BG: Color = Color::Rgb(34, 31, 23);

pub fn file_badge(name: &str, is_dir: bool) -> (&'static str, Color) {
    if name == ".." {
        return ("↰", MUTED);
    }
    if is_dir {
        return ("▸", ACCENT);
    }
    let low = name.to_ascii_lowercase();
    let ext = low.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let glyph = match ext {
        "py" | "ipynb" | "rs" | "js" | "ts" | "cpp" | "c" | "h" | "go" => "◇",
        "csv" | "tsv" | "parquet" | "json" | "jsonl" | "yaml" | "yml" | "toml" | "xml" => "⊞",
        "pt" | "pth" | "safetensors" | "onnx" | "ckpt" | "bin" | "h5" | "gguf" => "◈",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" => "▣",
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "bmp" => "◩",
        "sh" | "bash" | "ps1" | "bat" | "cmd" | "exe" => "»",
        "md" | "txt" | "log" | "cfg" | "ini" | "env" => "≡",
        _ => "·",
    };
    (glyph, MUTED)
}

pub fn truncate_end(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".into();
    }
    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

pub fn truncate_middle(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".into();
    }
    if max_chars <= 4 {
        return truncate_end(s, max_chars);
    }
    let keep_left = (max_chars - 1) / 2;
    let keep_right = (max_chars - 1) - keep_left;
    let left: String = s.chars().take(keep_left).collect();
    let right: String = s.chars().skip(count.saturating_sub(keep_right)).collect();
    format!("{}…{}", left, right)
}

pub fn fmt_mtime(ts: f64, now_sec: f64) -> String {
    if ts <= 0.0 {
        return "--".into();
    }
    let diff = (now_sec - ts).max(0.0) as u64;
    if diff < 60 {
        "now".into()
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h", diff / 3600)
    } else if diff < 86400 * 30 {
        format!("{}d", diff / 86400)
    } else if diff < 86400 * 365 {
        format!("{}mo", diff / (86400 * 30))
    } else {
        format!("{}y", diff / (86400 * 365))
    }
}

pub fn du_micro_bar(pct: f64) -> Vec<Span<'static>> {
    let p = pct.clamp(0.0, 100.0);
    let filled = ((p / 100.0) * 4.0).round() as usize;
    let empty = 4usize.saturating_sub(filled);
    let col = if p < 50.0 {
        MUTED
    } else if p < 80.0 {
        WARN
    } else {
        ERR
    };
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(col)),
        Span::styled("░".repeat(empty), Style::default().fg(BORDER)),
        Span::styled(format!(" {:>2.0}%", p), Style::default().fg(col)),
    ]
}

pub fn highlight_preview_line(filename: &str, line_no: usize, raw: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{:>4} │ ", line_no),
        Style::default().fg(MUTED),
    )];
    let low = filename.to_ascii_lowercase();
    let ext = low.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let trimmed = raw.trim_start();

    match ext {
        "py" | "sh" | "bash" | "yaml" | "yml" | "toml" => {
            if trimmed.starts_with('#') {
                spans.push(Span::styled(raw.to_string(), Style::default().fg(MUTED)));
            } else if matches!(ext, "yaml" | "yml" | "toml") {
                let delim = if ext == "toml" { '=' } else { ':' };
                if let Some((k, rest)) = raw.split_once(delim) {
                    spans.push(Span::styled(k.to_string(), Style::default().fg(ACCENT)));
                    spans.push(Span::styled(delim.to_string(), Style::default().fg(MUTED)));
                    spans.push(Span::styled(rest.to_string(), Style::default().fg(TEXT)));
                } else {
                    spans.push(Span::styled(raw.to_string(), Style::default().fg(TEXT)));
                }
            } else {
                spans.push(Span::styled(raw.to_string(), Style::default().fg(TEXT)));
            }
        }
        "md" => {
            if trimmed.starts_with('#') {
                spans.push(Span::styled(
                    raw.to_string(),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ));
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                let lead_len = raw.len() - trimmed.len();
                spans.push(Span::raw(raw[..lead_len].to_string()));
                spans.push(Span::styled(
                    trimmed[..2].to_string(),
                    Style::default().fg(ACCENT),
                ));
                spans.push(Span::styled(
                    trimmed[2..].to_string(),
                    Style::default().fg(TEXT),
                ));
            } else {
                spans.push(Span::styled(raw.to_string(), Style::default().fg(TEXT)));
            }
        }
        "csv" => {
            let st = if line_no == 1 {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            let parts: Vec<&str> = raw.split(',').collect();
            for (idx, p) in parts.into_iter().enumerate() {
                if idx > 0 {
                    spans.push(Span::styled(" │ ", Style::default().fg(MUTED)));
                }
                spans.push(Span::styled(p.to_string(), st));
            }
        }
        "json" | "jsonl" => {
            if let Some((k, rest)) = raw.split_once(':') {
                spans.push(Span::styled(k.to_string(), Style::default().fg(ACCENT)));
                spans.push(Span::styled(":", Style::default().fg(MUTED)));
                spans.push(Span::styled(rest.to_string(), Style::default().fg(TEXT)));
            } else {
                spans.push(Span::styled(raw.to_string(), Style::default().fg(TEXT)));
            }
        }
        _ => spans.push(Span::styled(raw.to_string(), Style::default().fg(TEXT))),
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_badges_truncation_and_mtime() {
        assert_eq!(file_badge("..", true), ("↰", MUTED));
        assert_eq!(file_badge("checkpoints", true), ("▸", ACCENT));
        assert_eq!(file_badge("train.py", false), ("◇", MUTED));
        assert_eq!(file_badge("sub.csv", false), ("⊞", MUTED));
        assert_eq!(file_badge("model.safetensors", false), ("◈", MUTED));
        assert_eq!(file_badge("archive.tar.gz", false), ("▣", MUTED));
        assert_eq!(file_badge("plot.png", false), ("◩", MUTED));

        assert_eq!(truncate_end("dữ_liệu_tiếng_việt.csv", 10), "dữ_liệu_t…");
        assert_eq!(truncate_middle("thư_mục_rất_dài_tiếng_việt", 11), "thư_m…_việt");
        assert_eq!(truncate_end("abc", 1), "…");
        assert_eq!(truncate_end("abc", 0), "");

        let now = 1_000_000.0;
        assert_eq!(fmt_mtime(0.0, now), "--");
        assert_eq!(fmt_mtime(now - 20.0, now), "now");
        assert_eq!(fmt_mtime(now - 180.0, now), "3m");
        assert_eq!(fmt_mtime(now - 7200.0, now), "2h");
        assert_eq!(fmt_mtime(now - 86400.0 * 4.0, now), "4d");

        let md_line = highlight_preview_line("README.md", 1, "# Title");
        assert_eq!(md_line.spans[1].style.fg, Some(ACCENT));
        let csv_line = highlight_preview_line("sub.csv", 1, "id,target");
        assert_eq!(csv_line.spans[2].content, " │ ");
    }
}
