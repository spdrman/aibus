//! The control channel (06 section 2): with `--json`, stdin after the prompt is one JSON object
//! per line, `{"type":"Permission","id":"...","decision":"allow"}` or
//! `{"type":"SendMessage","run":"...","text":"..."}`. That is how an outer orchestrator, or a
//! test, drives a headless session.

use std::io::BufRead;

use aibus_core::{Decision, PermissionId, RunId};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

/// Longest message accepted on the channel.
const MAX_TEXT: usize = 1 << 20;

#[derive(PartialEq, Debug)]
pub enum Control {
    Permission { id: PermissionId, decision: Decision },
    SendMessage { run: RunId, text: String },
}

/// What the reader hands over.
#[derive(PartialEq, Debug)]
pub enum Line {
    Ok(Control),
    /// A line that is not a control message, with the reason.
    Bad(String),
    /// Stdin is closed: nobody can answer a prompt any more.
    Eof,
}

/// One line of the channel. `decision` is `allow`, `allow_session` or `deny`.
pub fn parse(line: &str) -> Result<Control, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let field = |name: &str| {
        v.get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing string field {name:?}"))
    };
    match field("type")? {
        "Permission" => {
            let id = field("id")?.parse().map_err(|e| format!("id: {e}"))?;
            let decision = match field("decision")? {
                "allow" => Decision::Allow,
                "allow_session" => Decision::AllowSession,
                "deny" => Decision::Deny,
                other => return Err(format!("decision {other:?}: want allow, allow_session or deny")),
            };
            Ok(Control::Permission { id, decision })
        }
        "SendMessage" => {
            let run = field("run")?.parse().map_err(|e| format!("run: {e}"))?;
            let text = field("text")?;
            if text.len() > MAX_TEXT {
                return Err(format!("text is over {MAX_TEXT} bytes"));
            }
            Ok(Control::SendMessage {
                run,
                text: text.to_owned(),
            })
        }
        other => Err(format!("unknown type {other:?}: want Permission or SendMessage")),
    }
}

/// Reads `input` on a thread of its own (the runtime has no async stdin) and sends each line
/// parsed, then `Eof`. Blank lines are skipped.
pub fn spawn(input: impl BufRead + Send + 'static, tx: UnboundedSender<Line>) {
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let item = match parse(&line) {
                Ok(c) => Line::Ok(c),
                Err(why) => Line::Bad(why),
            };
            if tx.send(item).is_err() {
                return;
            }
        }
        let _ = tx.send(Line::Eof);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_and_send_message_parse() {
        let id = PermissionId::new();
        let run = RunId::new();
        assert_eq!(
            parse(&format!(r#"{{"type":"Permission","id":"{id}","decision":"allow"}}"#)),
            Ok(Control::Permission { id, decision: Decision::Allow })
        );
        assert_eq!(
            parse(&format!(r#"{{"type":"Permission","id":"{id}","decision":"deny","note":1}}"#)),
            Ok(Control::Permission { id, decision: Decision::Deny })
        );
        assert_eq!(
            parse(&format!(r#"{{"type":"SendMessage","run":"{run}","text":"keep going"}}"#)),
            Ok(Control::SendMessage { run, text: "keep going".into() })
        );
    }

    #[test]
    fn anything_else_is_a_reason_not_a_panic() {
        for bad in [
            "nope",
            "[]",
            r#"{"type":"Quit"}"#,
            r#"{"type":"Permission","id":"x","decision":"allow"}"#,
            r#"{"type":"Permission","id":"01JC0000000000000000000000","decision":"maybe"}"#,
            r#"{"type":"SendMessage","run":"01JC0000000000000000000000"}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    /// A slash line is not a control message: the channel has two types, and `SendMessage` hands
    /// the text to a run as the words of a user turn. Nothing on it can name a command.
    #[test]
    fn a_slash_line_on_the_headless_control_channel_is_rejected() {
        let run = RunId::new();
        let lines = ["/login anthropic", "/cancel @lead", "/model haiku", "/workflow save x --force"];
        for line in lines {
            assert!(parse(line).is_err(), "{line}");
            for kind in ["Command", "SubmitPrompt", "SetModel", "Cancel", "Login", "Workflow"] {
                let v = serde_json::json!({"type": kind, "text": line, "line": line, "run": run.to_string()});
                let why = parse(&v.to_string()).expect_err(kind);
                assert!(why.contains("unknown type"), "{kind}: {why}");
            }
            // As the text of a message it is only ever text, and parsed back it is unchanged.
            let v = serde_json::json!({"type": "SendMessage", "run": run.to_string(), "text": line});
            assert_eq!(parse(&v.to_string()), Ok(Control::SendMessage { run, text: line.into() }));
        }
    }

    #[test]
    fn the_reader_skips_blank_lines_and_ends_with_eof() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn(std::io::Cursor::new("\n  \nnot json\n"), tx);
        let got: Vec<Line> = std::iter::from_fn(|| rx.blocking_recv()).collect();
        assert!(matches!(got.as_slice(), [Line::Bad(_), Line::Eof]), "{got:?}");
    }
}
