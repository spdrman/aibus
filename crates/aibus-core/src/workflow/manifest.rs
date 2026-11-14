//! The reproducibility manifest (#283, spec section 4.3) and the comparison of two plans.
//!
//! `run_manifest` pins everything a run starts from: the compiled plan's hash and versions, the
//! task input (by hash only), the aibus build, the host, the runtimes, the routing decisions and
//! the repository. A value nobody knows yet is written as `{"unknown": "<why>"}` and one that does
//! not apply as `{"not_applicable": "<why>"}`, never as an empty string or a guess.

use serde::Serialize;
use serde_json::Value;

/// What the caller knows about the run that is about to start. Each `Err` says why a value is
/// not known.
#[derive(Clone, Debug)]
pub struct RunFacts {
    /// The task input; only its hash and length go into the manifest.
    pub input: Option<String>,
    pub aibus_version: String,
    pub os: String,
    pub arch: String,
    pub base_commit: Result<String, String>,
    pub dirty: Result<bool, String>,
    /// The names of the environment variables (never a value).
    pub env_names: Vec<String>,
    /// The per-run budgets in force (`max_turns`, ...).
    pub budgets: Value,
}

/// One difference between two plans: where, and the value on each side (`null` for absent).
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Change {
    pub path: String,
    pub a: Value,
    pub b: Value,
}

/// The `run-manifest.json` for a run of `plan`.
pub fn run_manifest(plan: &Value, facts: &RunFacts) -> Value {
    let _ = (plan, facts);
    unimplemented!("#283")
}

/// Every difference between two plans. Steps are matched by key and edges by their ends and
/// kind, so a step moved in the file is not a change and a renamed one is.
pub fn diff(a: &Value, b: &Value) -> Vec<Change> {
    let _ = (a, b);
    unimplemented!("#283")
}
