//! Fixture-based conformance (`aibus extensions test <path-or-id> --fixtures`): offline, no CLI,
//! no network. Each fixture is a recorded stdout stream with an `.expected.json` next to it; the
//! package's engine reads it and the result has to match the kinds, the final status and the
//! ordering and evidence rules every extension owes the trace.

use std::time::{Duration, SystemTime};

use aibus_supervisor::Ending;
use aibus_trace::{
    AttemptRef, AttemptStatus, DataQuality, EventKind, Metric, Origin, TraceEvent, Visibility,
};
use uuid::Uuid;

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
    pub ending: String,
    pub status: AttemptStatus,
    pub kinds: Vec<EventKind>,
    pub events: Vec<TraceEvent>,
}

fn wire(v: impl serde::Serialize) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Feeds `fixture` (a path in the package) through the package's engine with fixed arrival times and
/// `ending` (`exit:<code>`, `cancelled`, `wall_time`, `output_limit` or `signal:<n>`).
pub fn observe(pkg: &Package, fixture: &str, ending: &str) -> Result<Observed, String> {
    let bytes = pkg
        .files
        .get(fixture)
        .ok_or_else(|| format!("{fixture} is not in the package"))?;
    let number = |rest: &str| {
        rest.parse::<i32>()
            .map_err(|_| format!("{ending:?} is not an ending"))
    };
    let end = match ending {
        "cancelled" => Ending::Cancelled,
        "wall_time" => Ending::WallTime,
        "output_limit" => Ending::OutputLimit,
        _ => match ending.split_once(':') {
            Some(("exit", n)) => Ending::Exited(number(n)?),
            Some(("signal", n)) => Ending::Signalled(number(n)?),
            _ => return Err(format!("{ending:?} is not an ending")),
        },
    };
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(0x286_0001),
        attempt_id: Uuid::from_u128(0x286_0002),
        parent_attempt_id: None,
        provider_extension: pkg.manifest.id.clone(),
    };
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut eng = crate::sandbox::engine_for(pkg, attempt);
    let mut events = eng
        .start(base, serde_json::json!({ "fixture": fixture }))
        .events;
    let mut at = base;
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        at += Duration::from_millis(10);
        events.extend(eng.stream_line(line, at).events);
    }
    at += Duration::from_millis(10);
    events.extend(eng.finish(end, Duration::from_secs(1), at).events);
    let status = events
        .last()
        .and_then(|e| serde_json::from_value(e.data["status"].clone()).ok())
        .ok_or("the engine's last event carries no status")?;
    let kinds = events.iter().map(|e| e.kind).collect();
    Ok(Observed {
        ending: ending.to_owned(),
        status,
        kinds,
        events,
    })
}

fn check(name: String, passed: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        passed,
        detail: detail.into(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    ending: String,
    status: String,
    kinds: Vec<String>,
}

/// The obligations every extension owes the trace, over the events of one fixture: ascending uuid
/// v7 ids, an evidence level on every event, a data quality on every metric, a missing number
/// that stays unknown, and one ending, last. A third party can run it over their own events.
pub fn invariants(stem: &str, ev: &[TraceEvent]) -> Vec<Check> {
    let ascending = ev.windows(2).all(|w| w[0].event_id < w[1].event_id)
        && ev.iter().all(|e| e.event_id.get_version_num() == 7);
    // Raw-line evidence is cited, and nothing inferred passes for observed.
    let labelled = ev.iter().all(|e| {
        (e.origin != Origin::Derived || e.visibility != Visibility::Observed)
            && (e.origin != Origin::CliStream
                || e.kind == EventKind::GapDetected && e.source.raw_sha256.is_none()
                || !e.evidence_refs.is_empty())
    });
    let metrics: Vec<Metric> = ev
        .iter()
        .filter_map(|e| e.data.get("metrics"))
        .filter_map(|m| m.as_array())
        .flatten()
        .filter_map(|m| serde_json::from_value(m.clone()).ok())
        .collect();
    let every_metric_has_quality = ev
        .iter()
        .filter_map(|e| e.data.get("metrics").and_then(|m| m.as_array()))
        .flatten()
        .all(|m| serde_json::from_value::<Metric>(m.clone()).is_ok());
    let unknown_stays_unknown = metrics
        .iter()
        .all(|m| (m.value.is_none()) == (m.quality == DataQuality::Unknown));
    let one_terminal = ev
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::AttemptCompleted | EventKind::AttemptFailed
            )
        })
        .count()
        == 1
        && ev.last().is_some_and(|e| {
            matches!(
                e.kind,
                EventKind::AttemptCompleted | EventKind::AttemptFailed
            )
        });
    vec![
        check(
            format!("{stem}: event ids ascend (uuid v7)"),
            ascending,
            "ids in stream order",
        ),
        check(
            format!("{stem}: every event has an evidence level"),
            labelled,
            "cli events cite their raw line; derived events are never observed",
        ),
        check(
            format!("{stem}: every metric has a data quality"),
            every_metric_has_quality,
            "reported, derived, estimated or unknown",
        ),
        check(
            format!("{stem}: usage that is missing stays unknown"),
            unknown_stays_unknown,
            "no zero-filling",
        ),
        check(
            format!("{stem}: the attempt ends once, last"),
            one_terminal,
            "attempt_completed or attempt_failed",
        ),
    ]
}

pub fn run(pkg: &Package) -> Report {
    let mut checks = vec![check(
        format!("{}: the manifest is valid under schema 1", pkg.manifest.id),
        true,
        "parsed and validated when the package was read",
    )];
    for f in &pkg.manifest.fixtures.files {
        let stem = f
            .trim_start_matches("fixtures/")
            .trim_end_matches(".jsonl")
            .to_owned();
        let expected_path = f.replace(".jsonl", ".expected.json");
        let expected: Result<Expected, String> = pkg
            .files
            .get(&expected_path)
            .ok_or_else(|| format!("{expected_path} is missing"))
            .and_then(|b| serde_json::from_slice(b).map_err(|e| format!("{expected_path}: {e}")));
        let expected = match expected {
            Ok(x) => x,
            Err(why) => {
                checks.push(check(format!("{stem}: has an expected file"), false, why));
                continue;
            }
        };
        let observed = match observe(pkg, f, &expected.ending) {
            Ok(o) => o,
            Err(why) => {
                checks.push(check(
                    format!("{stem}: runs through the engine"),
                    false,
                    why,
                ));
                continue;
            }
        };
        let kinds: Vec<String> = observed.kinds.iter().map(wire).collect();
        let matches = wire(observed.status) == expected.status && kinds == expected.kinds;
        let detail = if matches {
            format!("{} events, {}", kinds.len(), expected.status)
        } else {
            format!(
                "expected {} {:?}, got {} {:?}",
                expected.status,
                expected.kinds,
                wire(observed.status),
                kinds
            )
        };
        checks.push(check(
            format!("{stem}: matches its expected events"),
            matches,
            detail,
        ));
        checks.extend(invariants(&stem, &observed.events));
    }
    Report { checks }
}
