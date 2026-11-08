//! Specifier matching and path confinement (09 section 2).

use std::io;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;

use crate::tool::ToolError;

/// Shell-word normalisation for `bash(...)` specifiers: leading `NAME=value` assignments are
/// dropped (quotes and backslashes respected) and runs of whitespace collapse to one space.
pub(crate) fn normalise_command(cmd: &str) -> String {
    let mut rest = cmd.trim_start();
    while let Some(after) = strip_assignment(rest) {
        rest = after.trim_start();
    }
    rest.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Returns the text after one leading `NAME=value` word, or `None` if the text does not start
/// with one.
fn strip_assignment(s: &str) -> Option<&str> {
    let eq = s.find('=')?;
    let name = &s[..eq];
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s[eq + 1..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c.is_whitespace() {
            return Some(&s[eq + 1 + i..]);
        }
    }
    Some("")
}

/// Glob match for a specifier. `*` crosses `/`, so `read(/abs/**)` and `bash(git *)` behave
/// the way the examples in 09 read. A spec that is not a valid glob matches nothing.
pub(crate) fn spec_matches(spec: &str, subject: &str) -> bool {
    GlobBuilder::new(spec)
        .literal_separator(false)
        .build()
        .is_ok_and(|g| g.compile_matcher().is_match(subject))
}

/// Canonicalises `p` (relative to `cwd`), resolving symlinks, and says whether the result is
/// inside the canonical project root.
pub(crate) fn resolve(cwd: &Path, p: &str) -> io::Result<(PathBuf, bool)> {
    let root = cwd.canonicalize()?;
    let canon = root.join(p).canonicalize()?;
    let inside = canon.starts_with(&root);
    Ok((canon, inside))
}

/// The path a tool may touch: canonical and inside `cwd`, or `Denied`.
pub(crate) fn confine(cwd: &Path, p: &str) -> Result<PathBuf, ToolError> {
    let (canon, inside) = resolve(cwd, p)?;
    if inside {
        Ok(canon)
    } else {
        Err(ToolError::Denied {
            reason: "path outside project".into(),
        })
    }
}
