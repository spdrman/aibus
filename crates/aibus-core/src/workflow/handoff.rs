//! The consented handoff (#360, A2.2d, spec 10.2): what moves from a step on one runtime to a
//! step on another, shown before it moves, and the record of the user's answer.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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

pub fn needs_consent(_c: &Context) -> bool {
    false
}

pub fn item(name: &str, kind: &str, _bytes: &[u8]) -> Item {
    Item {
        name: name.into(),
        kind: kind.into(),
        sha256: String::new(),
        bytes: 0,
    }
}

pub fn preview(_t: &Transfer) -> Value {
    json!({})
}

pub fn preview_sha256(_preview: &Value) -> String {
    String::new()
}

pub fn record(
    t: Transfer,
    _d: Decision,
    _shown_sha256: &str,
    id: &str,
    at_unix: u64,
) -> Result<Artifact, HandoffError> {
    Ok(Artifact {
        kind: String::new(),
        id: id.to_owned(),
        transfer: t,
        preview_sha256: String::new(),
        consent: Consent {
            state: State::NotNeeded,
            by: String::new(),
            at_unix,
        },
    })
}

pub fn deliver(
    _a: &Artifact,
    _now: &Context,
    contents: Vec<(String, Vec<u8>)>,
    _spent: &mut BTreeSet<String>,
) -> Result<Vec<(String, Vec<u8>)>, HandoffError> {
    Ok(contents)
}
