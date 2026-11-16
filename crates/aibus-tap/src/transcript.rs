//! The transcript tail, behind an explicit per-client opt-in (D-355).
//!
//! Off unless the user ran `aibus sessions allow-transcripts claude`. Then: read-only, the last
//! megabyte of one file under `<config>/projects/` at most, never copied (the store never sees
//! it, the text lives in memory for the screen and is redacted and cleaned first), never sent
//! anywhere. Claude Code says the entry format is internal and changes between versions, so the
//! parser names the versions it was written against; on any other it still shows what parses and
//! counts what it cannot as `trace_warning unknown_variant`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use aibus_core::Redactor;
use aibus_trace::view::clean;
use serde_json::Value;

/// Claude Code versions whose real transcripts the parser was checked against. Empty: the
/// fixtures are synthetic and no real transcript was ever parsed (D-355b). `claude` on PATH here
/// is 2.1.311 and the shell alias 2.1.304; neither was read. A version goes in only after a live
/// check says what was parsed.
pub const TESTED: &[&str] = &[];

const TAIL_BYTES: u64 = 1 << 20;
const TAIL_LINES: usize = 200;
const MESSAGES: usize = 20;
const FLAG: &str = "allow-transcripts-claude";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The user has not run `aibus sessions allow-transcripts claude`.
    Off,
    /// On, and the version is one the parser was tested on.
    Tested,
    /// On, but the version is not tested (or not known): only what parses is shown.
    UntestedVersion,
    /// On, but the file was refused or unreadable.
    Unreadable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tail {
    pub status: Status,
    /// `role: text`, redacted, cleaned and cut.
    pub messages: Vec<String>,
    pub tool_calls: u64,
    /// Entries or blocks of a type the parser does not know: `trace_warning unknown_variant`.
    pub unknown_variants: u64,
    pub bytes_read: u64,
}

fn flag(state: &Path) -> std::path::PathBuf {
    state.join("tap").join(FLAG)
}

pub fn allowed(state: &Path) -> bool {
    std::fs::symlink_metadata(flag(state)).is_ok_and(|m| m.is_file())
}

pub fn set_allowed(state: &Path, on: bool) -> Result<(), String> {
    let f = flag(state);
    if on {
        crate::hooks::write_private(
            &state.join("tap"),
            &f,
            b"transcript tail allowed for claude (read-only, never copied, never sent)\n",
            0o600,
        )
    } else {
        match std::fs::remove_file(&f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

fn empty(status: Status) -> Tail {
    Tail {
        status,
        messages: Vec::new(),
        tool_calls: 0,
        unknown_variants: 0,
        bytes_read: 0,
    }
}

/// Reads the end of the transcript at `path` when the opt-in is on. `version` is the CLI's, when
/// known; without it the entries' own `version` field is used, and failing that the version is
/// untested.
pub fn tail(
    state: &Path,
    config: &Path,
    path: &Path,
    version: Option<&str>,
    redactor: &Redactor,
) -> Tail {
    if !allowed(state) {
        return empty(Status::Off);
    }
    let refuse = |why: &str| empty(Status::Unreadable(why.to_owned()));
    // A link is refused before it is resolved; the file must sit under <config>/projects.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return refuse("the transcript path is a link");
    }
    let (Ok(real), Ok(projects)) = (
        std::fs::canonicalize(path),
        std::fs::canonicalize(config.join("projects")),
    ) else {
        return refuse("the transcript is not under the config directory's projects folder");
    };
    let opened = if real.starts_with(&projects) {
        crate::open_own(&real, u64::MAX)
    } else {
        None
    };
    let Some(mut file) = opened else {
        return refuse(
            "the transcript is not a file of yours under the config directory's projects folder",
        );
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(TAIL_BYTES);
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err()
        || (&mut file).take(TAIL_BYTES).read_to_end(&mut buf).is_err()
    {
        return refuse("could not read the transcript");
    }
    let bytes_read = buf.len() as u64;
    // A read that starts mid-file starts mid-line: drop that first piece.
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let lines = &lines[lines.len().saturating_sub(TAIL_LINES)..];

    let mut messages = Vec::new();
    let (mut tool_calls, mut unknown) = (0u64, 0u64);
    let mut seen_version: Option<String> = None;
    for l in lines {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            unknown += 1;
            continue;
        };
        if seen_version.is_none() {
            seen_version = v["version"].as_str().map(str::to_owned);
        }
        let role = match v["type"].as_str() {
            Some(r @ ("user" | "assistant")) => r,
            Some("system" | "summary") => continue,
            _ => {
                unknown += 1;
                continue;
            }
        };
        let say = |t: &str| format!("{role}: {}", clean(&redactor.redact(t), 200));
        match &v["message"]["content"] {
            Value::String(t) => messages.push(say(t)),
            Value::Array(blocks) => {
                for b in blocks {
                    match b["type"].as_str() {
                        Some("text") => messages.push(say(b["text"].as_str().unwrap_or(""))),
                        Some("tool_use") => tool_calls += 1,
                        Some("tool_result" | "thinking") => {}
                        _ => unknown += 1,
                    }
                }
            }
            _ => unknown += 1,
        }
    }
    let skip = messages.len().saturating_sub(MESSAGES);
    let tested = version
        .or(seen_version.as_deref())
        .is_some_and(|v| TESTED.contains(&v));
    Tail {
        status: if tested {
            Status::Tested
        } else {
            Status::UntestedVersion
        },
        messages: messages.split_off(skip),
        tool_calls,
        unknown_variants: unknown,
        bytes_read,
    }
}
