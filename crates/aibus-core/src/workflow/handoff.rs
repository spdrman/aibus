//! The consented handoff (#360, A2.2d, spec 10.2): what moves from a step on one runtime to a
//! step on another, shown before it moves, and the record of the user's answer.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::Runtime;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
}

/// One side of a transfer: the step, the runtime, and who receives and bills the data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Party {
    pub step: String,
    pub runtime: Runtime,
    pub provider: String,
    pub account: String,
}

/// Where and when a transfer happens: one run of one plan, from one party to another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub run_id: String,
    pub plan_hash: String,
    pub from: Party,
    pub to: Party,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    pub context: Context,
    pub items: Vec<Item>,
}

pub enum Decision<'a> {
    Granted { by: &'a str },
    Declined { by: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Granted,
    Declined,
    NotNeeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    pub state: State,
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
    #[error("a consent needs the name of who gave it")]
    NoOne,
    #[error("the hash given is not the hash of the preview of this transfer")]
    PreviewShown,
    #[error("the user did not consent to this transfer")]
    NotConsented,
    #[error("the recorded preview hash does not match the recorded transfer")]
    PreviewHash,
    #[error("this consent was given for another run, plan, source or destination")]
    WrongContext,
    #[error("this consent was already used")]
    Spent,
    #[error("{0} is not what the user was shown")]
    Changed(String),
}

/// The provider a runtime means, for the runtimes that have exactly one: a vendor CLI is its
/// vendor's, and OpenRouter is OpenRouter's, whatever a caller writes. `aibus` and an attached
/// session run under whatever account they are given, so they have none.
pub fn provider_of(rt: Runtime) -> Option<&'static str> {
    match rt {
        Runtime::ClaudeCli => Some("anthropic"),
        Runtime::CodexCli => Some("openai"),
        Runtime::GeminiCli => Some("google"),
        Runtime::OpenRouter => Some("openrouter"),
        Runtime::Aibus | Runtime::Attached => None,
    }
}

/// A transfer needs the user's yes unless the same party receives and bills it: the provider and
/// the account both have to match, and both have to be known. The runtime is not the boundary (two
/// `aibus` steps on an Anthropic and an OpenRouter account are a cross-provider move; the Claude
/// CLI to an `aibus` step on the same Anthropic account is not). Over-asking is safe, under-asking
/// is the bug: an empty or placeholder provider or account on either side (`unresolved`, `unknown`,
/// "resolved when the step runs") always asks, and a vendor CLI's or OpenRouter's provider comes
/// from the runtime, not from the caller.
pub fn needs_consent(c: &Context) -> bool {
    let known = |s: &str| {
        let s = s.trim();
        !(s.is_empty()
            || s.contains("unresolved")
            || s.contains("unknown")
            || s.contains("resolved when"))
    };
    let provider = |p: &Party| provider_of(p.runtime).unwrap_or(&p.provider).to_owned();
    let (from, to) = (&c.from, &c.to);
    ![
        provider(from),
        provider(to),
        from.account.clone(),
        to.account.clone(),
    ]
    .iter()
    .all(|v| known(v))
        || provider(from) != provider(to)
        || from.account != to.account
}

fn raw_sha256(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// What `bytes` is as an item: its name, kind, size and the hash of exactly these bytes.
pub fn item(name: &str, kind: &str, bytes: &[u8]) -> Item {
    Item {
        name: name.into(),
        kind: kind.into(),
        sha256: raw_sha256(bytes),
        bytes: bytes.len() as u64,
    }
}

/// What the user is shown before anything moves: the run and plan, from whom to whom, each item by
/// name, size and hash (never its content), and who receives and bills it. The hash of this value
/// is in the record, so the answer is tied to this exact text.
pub fn preview(t: &Transfer) -> Value {
    let total: u64 = t.items.iter().map(|i| i.bytes).sum();
    let to = &t.context.to;
    let receives = match to.runtime {
        Runtime::ClaudeCli => "Anthropic, through the Claude Code CLI under your own sign-in and plan",
        Runtime::CodexCli => "OpenAI, through the Codex CLI under your own sign-in and plan",
        Runtime::GeminiCli => "Google, through the Gemini CLI under your own sign-in and plan",
        Runtime::OpenRouter => "OpenRouter and the model provider it routes to, billed to your OpenRouter key as separate credits, not to any subscription",
        Runtime::Attached => "the agent session attached to this one, under its own permissions",
        Runtime::Aibus => "aibus's own run loop, under the provider and account named here",
    };
    json!({
        "kind": "aibus.workflow.handoff.preview",
        "context": t.context,
        "items": t.items,
        "total_bytes": total,
        "text": format!(
            "{} item(s), {total} bytes, move from step {} ({}, {}) to step {} ({}, account {}). They go to {receives}.",
            t.items.len(), t.context.from.step, t.context.from.provider, t.context.from.account,
            t.context.to.step, to.provider, to.account
        ),
    })
}

/// The hash a record holds of a preview: its canonical JSON.
pub fn preview_sha256(preview: &Value) -> String {
    super::compile::content_hash(&super::compile::canonical_json(preview))
}

/// The record of the user's answer to the preview of `t`. `shown_sha256` is the hash of the
/// preview they actually saw, and has to be this transfer's. `id` is a uuid v7 (D-319), in its
/// canonical lower-case form. A transfer that stays with one party is recorded `not_needed`.
pub fn record(
    t: Transfer,
    d: Decision,
    shown_sha256: &str,
    id: &str,
    at_unix: u64,
) -> Result<Artifact, HandoffError> {
    let v7 = uuid::Uuid::parse_str(id).ok().filter(|u| {
        u.get_version_num() == 7
            && u.get_variant() == uuid::Variant::RFC4122
            && u.hyphenated().to_string() == id
    });
    if v7.is_none() {
        return Err(HandoffError::BadId(id.to_owned()));
    }
    let (Decision::Granted { by } | Decision::Declined { by }) = d;
    if by.trim().is_empty() {
        return Err(HandoffError::NoOne);
    }
    let preview_sha256 = preview_sha256(&preview(&t));
    if shown_sha256 != preview_sha256 {
        return Err(HandoffError::PreviewShown);
    }
    let state = match (needs_consent(&t.context), d) {
        (false, _) => State::NotNeeded,
        (true, Decision::Granted { .. }) => State::Granted,
        (true, Decision::Declined { .. }) => State::Declined,
    };
    Ok(Artifact {
        kind: "aibus.workflow.handoff".into(),
        id: id.to_owned(),
        preview_sha256,
        consent: Consent {
            state,
            by: by.to_owned(),
            at_unix,
        },
        transfer: t,
    })
}

/// Hands over `contents` only if the record allows it right now: the answer was yes (or no yes
/// was needed, which is re-derived from the recorded parties, never read from a label), the
/// recorded preview is the hash of the recorded transfer, `now` is the same run, plan, source and
/// destination, the consent is unused, and the bytes hash to exactly the items shown, as a
/// multiset. The bytes returned are the ones hashed, so there is no second read to change. A
/// refusal does not use the consent up; a delivery does (`spent` holds the ids).
pub fn deliver(
    a: &Artifact,
    now: &Context,
    contents: Vec<(String, Vec<u8>)>,
    spent: &mut BTreeSet<String>,
) -> Result<Vec<(String, Vec<u8>)>, HandoffError> {
    match a.consent.state {
        State::Granted => {}
        State::NotNeeded if !needs_consent(&a.transfer.context) => {}
        State::Declined | State::NotNeeded => return Err(HandoffError::NotConsented),
    }
    if a.preview_sha256 != preview_sha256(&preview(&a.transfer)) {
        return Err(HandoffError::PreviewHash);
    }
    if a.transfer.context != *now {
        return Err(HandoffError::WrongContext);
    }
    // Keyed on the preview hash, which covers run, plan, both parties and items, so a copy of the
    // record under a new id is the same consent.
    if spent.contains(&a.preview_sha256) {
        return Err(HandoffError::Spent);
    }
    let mut shown = a.transfer.items.clone();
    for (name, bytes) in &contents {
        let kind = shown
            .iter()
            .find(|i| &i.name == name)
            .map_or("step_output", |i| i.kind.as_str());
        let got = item(name, kind, bytes);
        match shown.iter().position(|i| *i == got) {
            Some(at) => {
                shown.remove(at);
            }
            None => return Err(HandoffError::Changed(name.clone())),
        }
    }
    if let Some(left) = shown.first() {
        return Err(HandoffError::Changed(left.name.clone()));
    }
    spent.insert(a.preview_sha256.clone());
    Ok(contents)
}
