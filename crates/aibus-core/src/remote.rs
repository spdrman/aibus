//! What crosses from a session's control socket (#274) into the terminal loop. The socket end
//! (`aibus-control`) turns each line a client sends into an `Ask`, the loop turns an `Ask` into
//! the messages a key would have produced and answers on `reply`. Nothing here knows about the
//! socket or about MCP.

use serde_json::Value;
use tokio::sync::oneshot;

/// One request of a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Lead, model, cards and open permission prompts.
    Status,
    /// A prompt for the lead, as if typed on the prompt line. A slash line is refused.
    Send { text: String },
    /// A slash command line, through the same parser as typing.
    Command { line: String },
    /// Stops a run: an id, a unique prefix of one, an agent name or `lead`.
    Cancel { run: String },
    /// Answers an open permission prompt: `allow`, `allow_session` or `deny`. Refused unless the
    /// session was started with `--control-permission`.
    Permission { id: String, decision: String },
    /// A run's transcript (the lead's without `run`) and the session log rows.
    Transcript { run: Option<String> },
    /// An agent session asks to attach (#322). The keyboard accepts it with the code it is given.
    Attach {
        label: Option<String>,
        lineage: Vec<String>,
        pid: Option<u32>,
        cwd: Option<String>,
    },
    /// The attached session pulls the next step aibus queued for it.
    NextStep,
    /// The attached session's answer to a step: `done`, `declined` or `failed`.
    StepResult {
        token: String,
        status: String,
        summary: String,
    },
    /// The attached session says it began the step it pulled (#346).
    StepAck { token: String },
    /// The attached session reports one tool call (by name) it made for its step (#346).
    StepToolCall { token: String, name: String },
    /// A read-only panel on the screen: a message and a table, never a screen with live keys.
    Present {
        title: String,
        message: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

/// The answer to an `Ask`: the result, or the sentence saying why not.
pub type Reply = oneshot::Sender<Result<Value, String>>;

/// What the loop is told.
#[derive(Debug)]
pub enum Remote {
    Connected {
        client: String,
    },
    Disconnected {
        client: String,
    },
    Call {
        client: String,
        ask: Ask,
        reply: Reply,
    },
}

/// The name a client gave, safe to draw, log and put in a Markdown heading: ASCII letters,
/// digits, space, `.`, `_` and `-` only (no control, zero-width or bidi characters, no Markdown),
/// at most 32 characters, and `client` when nothing is left. The control socket and the screen
/// both apply it, so it lives where both can reach it.
pub fn client_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        .take(32)
        .collect();
    match name.trim() {
        "" => "client".into(),
        n => n.into(),
    }
}
