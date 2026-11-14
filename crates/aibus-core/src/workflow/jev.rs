//! The Jev decision record (#283, spec section 5.2) and the first deterministic routing rules.
//! Jev reads structured metadata, never a token or a prompt, and every decision says which rule
//! made it. The full policy engine and scheduler are #288; what is here is enough for the compiler
//! to pick a runtime and an execution mode for each step, and the same record #288 will log.

use serde::{Deserialize, Serialize};

use super::{Mode, Runtime};

/// What a runtime is known to offer. `status` is `available`, `experimental` (plans compile,
/// nothing runs) or `unavailable`. `capabilities` are the spec's capability vector.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct RuntimeInfo {
    pub id: &'static str,
    pub status: &'static str,
    pub capabilities: &'static [&'static str],
}

/// What Jev is asked for one step.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub task_id: String,
    /// Runtime capabilities the step needs (`one_shot`, `native_child_lineage`, ...).
    pub requires: Vec<String>,
    /// Runtime ids in the order the author prefers them.
    pub candidates: Vec<String>,
    pub policy_revision: String,
    pub budget_class: String,
    /// `read_only`, `writes` or `side_effects`.
    pub risk_class: String,
    /// The mode the step asked for.
    #[serde(default)]
    pub requested_mode: Mode,
    /// The runtime of the step that launches this one, for a step with a parent.
    #[serde(default)]
    pub parent_runtime: Option<String>,
}

/// What Jev decided, and why. `selected_runtime` is `None` when no candidate fits, which is
/// handed to a person, never guessed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub selected_runtime: Option<String>,
    pub execution_mode: Option<Mode>,
    pub reason_code: String,
    pub policy_evidence: Vec<String>,
    pub decision_source: String,
}

/// The table of runtimes this aibus knows.
pub fn runtime_info(rt: Runtime) -> RuntimeInfo {
    let _ = rt;
    unimplemented!("#283")
}

/// A warning for a step on a runtime that is not plainly available.
pub fn status_note(rt: Runtime) -> Option<String> {
    let _ = rt;
    None
}

/// `native_or_managed` becomes `native` when the runtime can show native child lineage and the
/// parent runs on the same runtime (a native subagent lives in its parent's process); otherwise
/// `aibus_managed`. The other two modes are what they say.
pub fn resolve_mode(requested: Mode, runtime: Runtime, parent: Option<Runtime>) -> Mode {
    let _ = (runtime, parent);
    requested
}

/// The policy's revision: the hash of its rules and runtime table, so a plan records which
/// policy made its decisions.
pub fn policy_revision() -> String {
    unimplemented!("#283")
}

/// Decides one request (spec 5.2 precedence: safety, availability, capabilities, then the
/// author's order).
pub fn decide(req: &Request) -> Decision {
    let _ = req;
    unimplemented!("#283")
}
