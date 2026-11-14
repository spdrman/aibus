//! The definition graph of a workflow (#283, spec section 4.2): route conditions, and the static
//! checks `Workflow::parse` makes before anything can run. Steps that run in a chain need none of
//! this; routes, parents, native steps and loops do.

use std::fmt;

use super::{Workflow, WorkflowError};

/// The longest `when` a route may have.
pub const MAX_WHEN_BYTES: usize = 256;

/// What a route condition can say about a step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pred {
    Completed,
    Failed,
    /// The step's JSON artifact parsed; only a step with `output = "json"` has one.
    ArtifactValid,
}

/// A route's `when`: `<step>.<completed|failed|artifact_valid>` joined with `!`, `&&` and `||`
/// (`&&` binds tighter; there are no parentheses).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cond {
    Is { step: String, what: Pred },
    Not(Box<Cond>),
    All(Vec<Cond>),
    Any(Vec<Cond>),
}

impl Cond {
    /// The default condition of a route with no `when`: its `from` step completed.
    pub fn completed(step: &str) -> Self {
        Cond::Is {
            step: step.to_owned(),
            what: Pred::Completed,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        unimplemented!("#283: {text}")
    }

    /// Every `<step>.<pred>` in the condition, in order.
    pub fn atoms(&self) -> Vec<(&str, Pred)> {
        match self {
            Cond::Is { step, what } => vec![(step.as_str(), *what)],
            Cond::Not(c) => c.atoms(),
            Cond::All(cs) | Cond::Any(cs) => cs.iter().flat_map(Cond::atoms).collect(),
        }
    }
}

/// The canonical spelling, which is what a plan records.
impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cond::Is { step, what } => {
                let what = match what {
                    Pred::Completed => "completed",
                    Pred::Failed => "failed",
                    Pred::ArtifactValid => "artifact_valid",
                };
                write!(f, "{step}.{what}")
            }
            Cond::Not(c) => write!(f, "!{c}"),
            Cond::All(cs) | Cond::Any(cs) => {
                let join = if matches!(self, Cond::All(_)) {
                    " && "
                } else {
                    " || "
                };
                let parts: Vec<String> = cs.iter().map(ToString::to_string).collect();
                f.write_str(&parts.join(join))
            }
        }
    }
}

/// Everything about the file as a whole that `parse` can't see one step at a time.
pub(super) fn check(wf: &Workflow) -> Result<(), WorkflowError> {
    let _ = wf;
    Ok(())
}

/// The most agent runs the workflow can start: every scheduled step once, every retry, and every
/// loop's body as often as its `max_iterations` allows (nested loops multiply). An upper bound.
pub fn worst_case_runs(wf: &Workflow) -> u64 {
    let _ = wf;
    0
}
