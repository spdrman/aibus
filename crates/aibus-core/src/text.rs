//! Text about to reach a terminal.

use std::borrow::Cow;

/// Removes ANSI escape sequences (CSI and OSC) and other C0 controls except tab and newline,
/// from text that is about to be printed for a person whose output is not a colour terminal.
pub fn strip_ansi(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c1_controls_and_the_osc_title_go() {
        assert_eq!(strip_ansi("a\u{9b}31mb"), "a31mb");
        assert_eq!(strip_ansi("ok\u{1b}]0;pwned\u{7}\u{1b}[2J!"), "ok!");
        assert!(matches!(strip_ansi("plain\ttext\n"), Cow::Borrowed(_)));
    }
}
