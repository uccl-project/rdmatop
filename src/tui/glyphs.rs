//! ASCII fallback for terminals that can't render UTF-8, where box-drawing and
//! block glyphs come out as garbage. We probe the terminal at startup (see
//! [`detect`]) and build the UI in Unicode or ASCII accordingly.

use ratatui::symbols;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation};
use std::borrow::Cow;
use std::io::Write;
use std::sync::OnceLock;

static UNICODE: OnceLock<bool> = OnceLock::new();

/// Whether we can safely draw UTF-8 glyphs. Cached after [`detect`].
pub fn unicode() -> bool {
    *UNICODE.get_or_init(locale_utf8)
}

/// Decide UTF-8 support from the live terminal, once, at startup. Must run in
/// raw mode with the alternate screen active. Inside tmux we trust its client
/// flag; otherwise we probe the terminal, then fall back to the locale.
pub fn detect() {
    // The probe is fooled inside tmux (see tmux_utf8), so when the tmux
    // query itself fails we must fall back to the locale, never the probe.
    let ok = if std::env::var_os("TMUX").is_some() {
        tmux_utf8().unwrap_or_else(locale_utf8)
    } else {
        probe().unwrap_or_else(locale_utf8)
    };
    let _ = UNICODE.set(ok);
}

/// Whether tmux will actually render UTF-8. The cursor probe is fooled under
/// tmux: its grid always decodes UTF-8, so the cursor advances as if UTF-8 even
/// when the client (non-UTF-8 locale, no `-u`) will strip the bytes to garbage.
/// `#{client_utf8}` is the only signal that reflects what reaches the terminal.
/// `None` when not under tmux, or no client is attached to answer.
fn tmux_utf8() -> Option<bool> {
    use std::process::{Command, Stdio};
    std::env::var_os("TMUX")?;
    let mut child = Command::new("tmux")
        .args(["display-message", "-p", "#{client_utf8}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Raw mode already disabled Ctrl-C, so a wedged tmux server must not
    // block startup forever: kill the query if it outlives the deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while child.try_wait().ok()?.is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = child.wait_with_output().ok()?;
    match out.stdout.first().copied()? {
        b'1' => Some(true),
        b'0' => Some(false),
        _ => None,
    }
}

/// Print a 1-column multibyte glyph and see how far the cursor moved: 1 column
/// means the terminal decoded UTF-8, more means it counted raw bytes.
fn probe() -> Option<bool> {
    use crossterm::{cursor, queue, terminal};
    let mut out = std::io::stdout();
    queue!(out, cursor::MoveTo(0, 0)).ok()?;
    write!(out, "\u{2588}").ok()?; // █: 3 UTF-8 bytes, 1 display column
    out.flush().ok()?;
    let (col, _) = cursor::position().ok()?;
    queue!(
        out,
        cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::All)
    )
    .ok()?;
    out.flush().ok()?;
    Some(col == 1)
}

/// Locale fallback, like htop: a UTF-8 `LC_*`/`LANG` gets blocks, else ASCII.
fn locale_utf8() -> bool {
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
        for c in [
            '│', '─', '↑', '↓', '←', '→', '●', '▏', '▐', '▲', '▼', '◀', '▶', '▬', '±', '…', '█',
            '▇', '▆', '▅', '▄', '▃', '▂', '▁', '░',
        ] {
            let a = ascii(c).unwrap_or_else(|| panic!("no ascii for {c:?}"));
            assert!(a.is_ascii(), "{c:?} -> {a:?} is not ascii");
        }
    }
}
