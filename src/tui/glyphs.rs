//! ASCII fallback for terminals whose locale isn't UTF-8 (e.g. LC_ALL=C, or
//! tmux without `-u`), where box-drawing/block glyphs render as garbage.
//! We build the UI in Unicode as usual and transliterate at the emit points.

use ratatui::symbols;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation};
use std::borrow::Cow;
use std::sync::OnceLock;

/// Whether we can safely draw UTF-8 glyphs. Cached at first call.
///
/// Locale-driven, like htop: a UTF-8 `LC_*`/`LANG` gets blocks, anything else
/// (C/POSIX) gets ASCII. `RDMATOP_UNICODE=0|1` forces it either way. Note that
/// `tmux -u` alone does NOT change the locale, so to get blocks under tmux set
/// a UTF-8 locale (e.g. `LC_ALL=C.UTF-8`) — which also renders without `-u`.
pub fn unicode() -> bool {
    static U: OnceLock<bool> = OnceLock::new();
    *U.get_or_init(|| {
        if let Ok(v) = std::env::var("RDMATOP_UNICODE") {
            let v = v.to_ascii_lowercase();
            return !matches!(v.as_str(), "" | "0" | "false" | "no");
        }
        // POSIX precedence: LC_ALL > LC_CTYPE > LANG; first non-empty decides.
        for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Ok(v) = std::env::var(k) {
                if !v.is_empty() {
                    let v = v.to_ascii_uppercase();
                    return v.contains("UTF-8") || v.contains("UTF8");
                }
            }
        }
        false
    })
}

/// Map a Unicode glyph to its closest ASCII stand-in.
fn ascii(c: char) -> Option<&'static str> {
    Some(match c {
        '│' | '▏' | '▐' => "|",
        '─' => "-",
        '↑' | '▲' => "^",
        '↓' | '▼' => "v",
        '←' | '◀' => "<",
        '→' | '▶' => ">",
        '●' => "*",
        '▬' => "=",
        '±' => "+-",
        '…' => "...",
        '█' | '▇' => "#",
        '░' => "-",
        '▆' | '▅' => "*",
        '▄' | '▃' => "+",
        '▂' => ".",
        '▁' => "_",
        _ => return None,
    })
}

/// Transliterate a string to ASCII when the terminal can't do UTF-8.
pub fn tr(s: &str) -> Cow<'_, str> {
    if unicode() || s.is_ascii() {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match ascii(c) {
            Some(a) => out.push_str(a),
            None => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Throughput meter fill/empty chars: solid block over faint shade in UTF-8,
/// htop-style pipe over blank in ASCII.
pub fn meter() -> (char, char) {
    if unicode() {
        ('█', '░')
    } else {
        ('|', ' ')
    }
}

/// Selection cursor for tables (`highlight_symbol`).
pub fn cursor() -> &'static str {
    if unicode() {
        "▶ "
    } else {
        "> "
    }
}

/// Border set for `Block`s; ASCII box when the locale isn't UTF-8.
pub fn border() -> symbols::border::Set<'static> {
    if unicode() {
        symbols::border::PLAIN
    } else {
        symbols::border::Set {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            vertical_left: "|",
            vertical_right: "|",
            horizontal_top: "-",
            horizontal_bottom: "-",
        }
    }
}

pub fn v_scrollbar() -> Scrollbar<'static> {
    let (thumb, track, up, down) = if unicode() {
        ("▐", "│", "▲", "▼")
    } else {
        ("#", "|", "^", "v")
    };
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol(thumb)
        .track_symbol(Some(track))
        .begin_symbol(Some(up))
        .end_symbol(Some(down))
}

pub fn h_scrollbar() -> Scrollbar<'static> {
    let (thumb, track, left, right) = if unicode() {
        ("▬", "─", "◀", "▶")
    } else {
        ("=", "-", "<", ">")
    };
    Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
        .thumb_symbol(thumb)
        .track_symbol(Some(track))
        .begin_symbol(Some(left))
        .end_symbol(Some(right))
}

#[cfg(test)]
mod tests {
    use super::ascii;

    #[test]
    fn ascii_map_covers_ui_glyphs() {
        // Every glyph the UI emits must have an ASCII stand-in, and none map
        // back to a multibyte char.
        for c in ['│', '─', '↑', '↓', '←', '→', '●', '▏', '▐', '▲', '▼', '◀',
            '▶', '▬', '±', '…', '█', '▇', '▆', '▅', '▄', '▃', '▂', '▁', '░']
        {
            let a = ascii(c).unwrap_or_else(|| panic!("no ascii for {c:?}"));
            assert!(a.is_ascii(), "{c:?} -> {a:?} is not ascii");
        }
    }
}
