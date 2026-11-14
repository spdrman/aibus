//! The workflow compiler (#283, spec sections 4.1 and 4.2): a parsed workflow and the agent
//! definitions it names become one normalized plan. The plan is the definition DAG with typed
//! edges, the fan-out groups, the envelope, the approval boundary, a content hash for every prompt
//! and agent, the invocation each step would get and the routing decisions, all with a
//! `plan_hash` over the lot. Nothing runs, and nothing in the plan depends on the host, the clock
//! or the order a map happens to iterate in, so the same input gives the same bytes anywhere.

use indexmap::IndexMap;
use serde_json::Value;

use super::{Problem, Workflow};
use crate::agent::AgentDef;
use crate::ids::AgentName;
use crate::Redactor;

/// The most steps a workflow the compiler accepts may have.
pub const MAX_STEPS: usize = 256;

/// `sha256:<hex>` of `text` with CRLF line ends read as LF, so a checkout that converts line ends
/// does not change a version.
pub fn content_hash(text: &str) -> String {
    let _ = text;
    unimplemented!("#283")
}

/// `v` as JSON with every object's keys sorted and no whitespace: what `plan_hash` hashes.
pub fn canonical_json(v: &Value) -> String {
    let _ = v;
    unimplemented!("#283")
}

/// The hash of a compiled plan: `content_hash` of its canonical JSON without the `plan_hash`
/// key. A saved plan whose stored hash differs from this was edited by hand.
pub fn plan_hash(plan: &Value) -> String {
    let _ = plan;
    unimplemented!("#283")
}

/// The capabilities a step `requires` that its agent's tools don't grant.
pub fn capability_problems(wf: &Workflow, agents: &IndexMap<AgentName, AgentDef>) -> Vec<Problem> {
    let _ = (wf, agents);
    Vec::new()
}

/// Compiles `wf` (read from `source`) against `agents`. `Err` is every problem found, and a
/// workflow with any is never planned.
pub fn compile(
    wf: &Workflow,
    source: &str,
    agents: &IndexMap<AgentName, AgentDef>,
    redactor: &Redactor,
) -> Result<Value, Vec<Problem>> {
    let _ = (wf, source, agents, redactor);
    unimplemented!("#283")
}
