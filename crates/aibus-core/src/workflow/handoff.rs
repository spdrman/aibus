//! The consented handoff (#360, A2.2d, spec 10.2): what moves from a step on one runtime to a
//! step on another, shown before it moves, and the record of the user's answer.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One thing that would cross: a step's output or an artifact, by name, size and hash. The
/// content itself is never in a preview or in the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    pub from_step: String,
    pub from_runtime: String,
    pub to_step: String,
    pub to_runtime: String,
    pub items: Vec<Item>,
}

pub enum Decision<'a> {
    Granted { by: &'a str },
    Declined { by: &'a str },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    /// `granted`, `declined` or `not_needed` (both steps run on one runtime).
    pub state: String,
    pub by: String,
    pub at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: String,
    pub id: String,
    pub transfer: Transfer,
    pub preview_sha256: String,
    pub consent: Consent,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandoffError {
    #[error("the handoff id is not a uuid v7: {0}")]
    BadId(String),
    #[error("the user did not consent to this transfer")]
    NotConsented,
    #[error("{0} is not what the user was shown")]
    Changed(String),
}

/// A transfer needs the user's yes when it changes runtime: the next step is another program,
/// another sign-in and another bill.
pub fn needs_consent(t: &Transfer) -> bool {
    t.from_runtime != t.to_runtime
}

/// Where a runtime's data goes and who bills it, in words for the person who is asked.
fn destination(runtime: &str) -> &'static str {
    match runtime {
        "cli.claude" => "Anthropic, through the Claude Code CLI under your own sign-in and plan",
        "cli.codex" => "OpenAI, through the Codex CLI under your own sign-in and plan",
        "cli.gemini" => "Google, through the Gemini CLI under your own sign-in and plan",
        "openrouter" => "OpenRouter and the model provider it routes to, billed to your OpenRouter key as separate credits, not to any subscription",
        "attached" => "the agent session attached to this one, under its own permissions",
        _ => "aibus's own run loop, with the account the step's agent uses",
    }
}

/// What the user is shown before anything moves: from where to where, each item by name, size and
/// hash (never its content), and who receives it. Its canonical hash goes into the record.
pub fn preview(t: &Transfer) -> Value {
    let total: u64 = t.items.iter().map(|i| i.bytes).sum();
    let to = destination(&t.to_runtime);
    json!({
        "kind": "aibus.workflow.handoff.preview",
        "from": { "step": t.from_step, "runtime": t.from_runtime },
        "to": { "step": t.to_step, "runtime": t.to_runtime, "receives_as": to },
        "items": t.items,
        "total_bytes": total,
        "text": format!(
            "{} item(s), {total} bytes, move from step {} ({}) to step {} ({}). They go to {to}.",
            t.items.len(), t.from_step, t.from_runtime, t.to_step, t.to_runtime
        ),
    })
}

fn is_uuid_v7(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
        && b[14] == b'7'
}

/// The record of the user's answer to the preview of `t`. `id` is a uuid v7 (D-319). A transfer
/// that stays on one runtime is recorded as `not_needed` whatever the decision was.
pub fn record(t: Transfer, d: Decision, id: &str, at_unix: u64) -> Result<Artifact, HandoffError> {
    if !is_uuid_v7(id) {
        return Err(HandoffError::BadId(id.to_owned()));
    }
    let (state, by) = match (needs_consent(&t), d) {
        (false, Decision::Granted { by } | Decision::Declined { by }) => ("not_needed", by),
        (true, Decision::Granted { by }) => ("granted", by),
        (true, Decision::Declined { by }) => ("declined", by),
    };
    let shown = preview(&t);
    Ok(Artifact {
        kind: "aibus.workflow.handoff".into(),
        id: id.to_owned(),
        preview_sha256: super::compile::content_hash(&super::compile::canonical_json(&shown)),
        consent: Consent {
            state: state.into(),
            by: by.to_owned(),
            at_unix,
        },
        transfer: t,
    })
}

/// Whether `items` may be delivered under `a`: the user said yes (or none was needed), and every
/// item is exactly one that was shown, no more and no fewer. The name of the first mismatch is
/// in the error.
pub fn check_delivery(a: &Artifact, items: &[Item]) -> Result<(), HandoffError> {
    if a.consent.state == "declined" {
        return Err(HandoffError::NotConsented);
    }
    let shown = &a.transfer.items;
    if let Some(i) = items.iter().find(|i| !shown.contains(i)) {
        return Err(HandoffError::Changed(i.name.clone()));
    }
    match shown.iter().find(|i| !items.contains(i)) {
        Some(i) => Err(HandoffError::Changed(i.name.clone())),
        None => Ok(()),
    }
}
