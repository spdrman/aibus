//! `aibus run` without `--json` (06 section 2): the lead's text on stdout as it streams, the
//! final summary, and one-line notices for tool calls and problems on stderr. No ANSI reaches a
//! stream that is not a colour terminal, and none of the model's own escape sequences reach a
//! terminal at all.

use std::collections::HashMap;
use std::io::Write;

use aibus_core::{Decision, Msg, Redactor};
use aibus_provider::StreamChunk;
use serde_json::Value;

use crate::term;

pub struct Plain<'a> {
    redactor: &'a Redactor,
    /// Colour on stderr (dim notices).
    color: bool,
    /// Tool name by call id, for the notice when it fails.
    names: HashMap<String, String>,
    streamed: bool,
    /// The last byte written to stdout was a newline (or nothing was written).
    at_line_start: bool,
}

/// The input's most telling field, or its JSON, cut to one short line.
fn brief(input: &Value) -> String {
    let pick = ["path", "command", "pattern", "query", "url"]
        .iter()
        .find_map(|k| input.get(k).and_then(Value::as_str));
    let text = pick.map_or_else(|| input.to_string(), str::to_owned);
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 80 {
        format!("{}...", line.chars().take(77).collect::<String>())
    } else {
        line
    }
}

impl<'a> Plain<'a> {
    pub fn new(redactor: &'a Redactor, color: bool) -> Self {
        Self {
            redactor,
            color,
            names: HashMap::new(),
            streamed: false,
            at_line_start: true,
        }
    }

    fn notice(&self, text: &str) {
        let text = self.redactor.redact(text);
        let text = term::strip_ansi(&text);
        let (on, off) = if self.color { ("\u{1b}[2m", "\u{1b}[0m") } else { ("", "") };
        let _ = writeln!(std::io::stderr().lock(), "{on}aibus: {text}{off}");
    }

    /// Prints what `msg` has for a person. `out_color` says whether stdout may carry ANSI; the
    /// model's text is stripped of escapes either way unless it is a colour terminal.
    pub fn show(&mut self, msg: &Msg, out_color: bool) -> std::io::Result<()> {
        match msg {
            Msg::Stream { chunks, .. } => {
                let text: String = chunks
                    .iter()
                    .filter_map(|c| match c {
                        StreamChunk::TextDelta(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                self.stdout(&text, out_color)?;
            }
            Msg::ToolCallRequested { call, .. } => {
                self.names.insert(call.id.0.to_string(), call.name.to_string());
                self.notice(&format!("tool {} {}", call.name, brief(&call.input)));
            }
            Msg::ToolCallFinished {
                call_id,
                result,
                duration,
                ..
            } if result.is_error => {
                let name = self.names.get(&call_id.0.to_string()).map_or("call", String::as_str);
                self.notice(&format!("tool {name} failed ({} ms)", duration.as_millis()));
            }
            Msg::PermissionDecided {
                decision: Decision::Deny,
                reason,
                ..
            } => {
                let why = reason.as_deref().unwrap_or("denied");
                self.notice(&format!("permission denied: {why}"));
            }
            Msg::RunFailed { error, .. } => {
                self.notice(&format!("run failed: {}", error.message));
            }
            Msg::IoError { context, error } => self.notice(&format!("{context}: {error}")),
            Msg::RunFinished { result, .. } => {
                // A run that streamed nothing (a failure, a budget stop) still gets its summary.
                if !self.streamed && !result.summary.is_empty() {
                    self.stdout(&result.summary, out_color)?;
                }
                if !self.at_line_start {
                    self.stdout("\n", out_color)?;
                }
                let u = &result.usage;
                self.notice(&format!(
                    "{}, {} turns, {} in / {} out tokens, ${:.4}",
                    result.status.as_value(),
                    result.turns,
                    u.input_tokens,
                    u.output_tokens,
                    u.cost_usd
                ));
                if let Some(path) = &result.summary_file {
                    self.notice(&format!("the full result is in {}", path.display()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn stdout(&mut self, text: &str, color: bool) -> std::io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let text = self.redactor.redact(text);
        let text = if color { text } else { term::strip_ansi(&text) };
        if text.is_empty() {
            return Ok(());
        }
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes())?;
        out.flush()?;
        self.streamed = true;
        self.at_line_start = text.ends_with('\n');
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_notice_names_the_most_telling_field_on_one_short_line() {
        assert_eq!(brief(&json!({"path": "src/main.rs", "lines": [1, 2]})), "src/main.rs");
        assert_eq!(brief(&json!({"command": "cargo\n  test -p x"})), "cargo test -p x");
        assert_eq!(brief(&json!({"n": 1})), r#"{"n":1}"#);
        let long = brief(&json!({"command": "x".repeat(200)}));
        assert_eq!(long.chars().count(), 80);
        assert!(long.ends_with("..."));
    }
}
