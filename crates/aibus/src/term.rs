//! What the terminal can be asked for (06 section 5): colour, the TUI itself, and its size.
//! `NO_COLOR`, `COLORTERM`, `TERM` and `EDITOR` are read here and in `config edit`. Colour here
//! is plain SGR (dim, bold), so `COLORTERM` has nothing to add; it only matters to the TUI theme.

use std::borrow::Cow;

use aibus_core::AibusError;

/// Whether human output on a stream may carry ANSI: it must be a terminal, `NO_COLOR` must be
/// unset or empty (no-color.org), and `TERM` must not be `dumb`.
pub fn use_color(is_tty: bool, no_color: Option<&str>, term: Option<&str>) -> bool {
    is_tty && no_color.is_none_or(str::is_empty) && term != Some("dumb")
}

/// `TERM=dumb` can run `aibus run` but not the dashboard: refuse it with a usage error (exit 2).
pub fn check_tui(term: Option<&str>) -> Result<(), AibusError> {
    if term == Some("dumb") {
        return Err(AibusError::Usage(
            "the dashboard needs a terminal that can draw, and TERM=dumb cannot; use `aibus run`"
                .into(),
        ));
    }
    Ok(())
}

/// Removes ANSI escape sequences (CSI and OSC) and other C0 controls except tab and newline,
/// from text that is about to be printed for a person whose output is not a colour terminal.
pub fn strip_ansi(text: &str) -> Cow<'_, str> {
    if !text.bytes().any(|b| (b < 0x20 && b != b'\n' && b != b'\t') || b == 0x7f) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // CSI: parameters, then a final byte in 0x40..=0x7e.
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST (ESC \).
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any other two-byte escape.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Columns and rows of the controlling terminal, or 80 by 24 when there is none.
pub fn size() -> (u16, u16) {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        let ask = |fd: std::os::fd::BorrowedFd<'_>| rustix::termios::tcgetwinsize(fd).ok();
        let found = ask(std::io::stderr().as_fd())
            .or_else(|| ask(std::io::stdout().as_fd()))
            .or_else(|| ask(std::io::stdin().as_fd()));
        if let Some(w) = found.filter(|w| w.ws_col > 0 && w.ws_row > 0) {
            return (w.ws_col, w.ws_row);
        }
    }
    (80, 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_needs_a_tty_and_no_opt_out() {
        assert!(use_color(true, None, Some("xterm-256color")));
        assert!(use_color(true, Some(""), None));
        assert!(!use_color(false, None, Some("xterm")));
        assert!(!use_color(true, Some("1"), Some("xterm")));
        assert!(!use_color(true, None, Some("dumb")));
    }

    #[test]
    fn dumb_terminals_cannot_open_the_dashboard() {
        assert_eq!(check_tui(Some("dumb")).unwrap_err().exit_code(), 2);
        assert!(check_tui(Some("xterm")).is_ok());
        assert!(check_tui(None).is_ok());
    }

    #[test]
    fn escapes_and_controls_are_stripped_and_plain_text_is_not_copied() {
        assert!(matches!(strip_ansi("plain\ttext\n"), Cow::Borrowed(_)));
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m and \u{1b}[1;32mgreen\u{1b}[m"), "red and green");
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{7}b\u{1b}]8;;http://x\u{1b}\\c"), "abc");
        assert_eq!(strip_ansi("bell\u{7}\r\nline"), "bell\nline");
        assert_eq!(strip_ansi("é\u{1b}Mü"), "éü");
    }

    #[test]
    fn size_falls_back_when_there_is_no_terminal() {
        let (c, r) = size();
        assert!(c > 0 && r > 0);
    }
}
