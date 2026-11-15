//! Fixture-based conformance (`aibus extensions test <path-or-id> --fixtures`): offline, no CLI,
//! no network. Each fixture is a recorded stdout stream with an `.expected.json` next to it; the
//! package's engine reads it and the result has to match the kinds, the final status and the
//! ordering and evidence rules every extension owes the trace.

use aibus_trace::{AttemptStatus, EventKind, TraceEvent};

use crate::package::Package;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.passed)
    }
}

/// What an engine made of one fixture.
#[derive(Clone, Debug)]
pub struct Observed {
    pub status: AttemptStatus,
    pub kinds: Vec<EventKind>,
    pub events: Vec<TraceEvent>,
}

/// Feeds `fixture` (a path in the package) through the package's engine with fixed arrival times and
/// `ending` (`exit:<code>`, `cancelled`, `wall_time`, `output_limit` or `signal:<n>`).
pub fn observe(pkg: &Package, fixture: &str, ending: &str) -> Result<Observed, String> {
    todo!("{:?} {fixture} {ending}", pkg.hash)
}

/// The text of an `.expected.json` for what was observed.
pub fn expected_json(o: &Observed) -> String {
    todo!("{:?}", o.status)
}

pub fn run(pkg: &Package) -> Report {
    todo!("{:?}", pkg.hash)
}
