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

pub fn needs_consent(_t: &Transfer) -> bool {
    false
}

pub fn preview(_t: &Transfer) -> Value {
    json!({})
}

pub fn record(t: Transfer, _d: Decision, id: &str, at_unix: u64) -> Result<Artifact, HandoffError> {
    Ok(Artifact {
        kind: String::new(),
        id: id.to_owned(),
        transfer: t,
        preview_sha256: String::new(),
        consent: Consent {
            state: String::new(),
            by: String::new(),
            at_unix,
        },
    })
}

pub fn check_delivery(_a: &Artifact, _items: &[Item]) -> Result<(), HandoffError> {
    Ok(())
}
