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
