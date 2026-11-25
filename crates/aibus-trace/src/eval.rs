//! Evaluation cases and results (#293, spec section 9.1).
//!
//! A suite is a versioned list of cases, each naming its fixture repository and commit, the side
//! effects it allows, the artifacts it expects, the deterministic commands that decide it, a rubric,
//! a privacy policy and a duration and cost envelope. `evaluate` turns one recorded run of a
//! case (its trace, the outcomes of the deterministic commands someone ran, and any scores) into a
//! versioned `EvaluationResult` that says who or what assessed each part.
//!
//! Nothing here runs a process or a model: the deterministic commands run elsewhere and their
//! outcomes come in as `CheckResult`s, so an experiment over recorded traces repeats exactly. The
//! primary gate is the deterministic checks. Human and model scores are secondary, never change the
//! verdict, and a model score is refused unless the suite switched model judging on.
//!
//! A case may name an `agent` it targets (the per-agent evals of #392). #293 does not build those:
//! the field is read, written and evaluated like any other case, so a per-agent suite does not need a
//! new schema version.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{project, AttemptStatus, DataQuality, EventKind, Metric, TraceEvent};

/// The version of `EvalSuite`. A reader refuses any other.
pub const SUITE_SCHEMA_VERSION: u32 = 1;
/// The version of `EvaluationResult`, written into every result.
pub const RESULT_VERSION: u32 = 1;
/// Spec 9.3: a benchmark suite has at least this many cases.
pub const MIN_BENCHMARK_CASES: usize = 10;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvalSuite {
    pub schema_version: u32,
    pub name: String,
    /// Bumped whenever a case changes, so a result names the exact cases it ran.
    pub suite_version: u32,
    /// Model-judge scores are refused unless this is true (spec 9.1: only when explicitly configured).
    #[serde(default)]
    pub model_judge: bool,
    pub cases: Vec<EvalCase>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub case_id: String,
    pub task_input: String,
    pub fixture: Fixture,
    /// The tools that may change things in this run. `evaluate` fails the case on a Bash, Edit or
    /// other tool call not named here. Paths are not checked, the trace has no tool input.
    pub allowed_side_effects: Vec<String>,
    /// Artifact names the trace must show as created.
    pub expected_artifacts: Vec<String>,
    /// Deterministic commands. The caller runs them and hands each outcome in under the same name.
    pub test_commands: Vec<String>,
    /// The items a human or a model judge may score.
    pub rubric: Vec<String>,
    pub privacy: PrivacyPolicy,
    pub envelope: Envelope,
    /// The agent this case targets, for the per-agent evals of #392. `None` is the whole workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub repository: String,
    pub commit: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub max_duration_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPolicy {
    /// Only counts, names and ids leave the run.
    MetadataOnly,
    /// Payloads may be kept with the trace, redacted.
    RetainRedactedPayloads,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Compile,
    UnitTests,
    StaticCheck,
    ExpectedArtifact,
    PolicyInvariant,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Pass,
    Fail,
    /// Nobody reported an outcome. Never counts as a pass.
    NotRun,
}

/// One deterministic check. For a test command, `name` is the command string of the case.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CheckResult {
    pub kind: CheckKind,
    pub name: String,
    pub outcome: CheckOutcome,
    /// Trace events that back the outcome, when the trace has any.
    #[serde(default)]
    pub evidence: Vec<Uuid>,
}

/// Who or what produced a score or a result (spec 9.1: assessor provenance).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Assessor {
    Deterministic {
        tool: String,
        version: String,
    },
    Human {
        reviewer: String,
    },
    /// A model judge: the model, the prompt (by hash) and the version are all recorded.
    Model {
        model: String,
        prompt_sha256: String,
        version: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Assessment {
    pub assessor: Assessor,
    pub rubric_item: String,
    pub score: f64,
    pub max_score: f64,
}

/// What a run was made of (spec 9.1 context), recorded so two variants' differences are visible.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RunContext {
    pub repository_revision: String,
    pub extension_version: String,
    pub cli_version: Option<String>,
    /// How many runs were executing at once. Above 1, wall times are not isolated.
    pub concurrent_runs: u32,
    pub cache_note: Option<String>,
}

/// One recorded run of a case under a variant.
#[derive(Clone, Debug)]
pub struct RecordedRun {
    pub case_id: String,
    pub variant: String,
    pub events: Vec<TraceEvent>,
    pub checks: Vec<CheckResult>,
    pub assessments: Vec<Assessment>,
    pub context: RunContext,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every check passed, and there was at least one.
    Pass,
    Fail,
    /// No failure, but a check has no outcome or the case has no deterministic check at all.
    Incomplete,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EvaluationResult {
    pub result_version: u32,
    pub suite: String,
    pub suite_version: u32,
    pub case_id: String,
    pub variant: String,
    pub workflow_run_ids: Vec<Uuid>,
    pub context: RunContext,
    /// The runner itself, as the assessor of the deterministic part.
    pub assessor: Assessor,
    pub verdict: Verdict,
    pub checks: Vec<CheckResult>,
    /// Secondary. These never change the verdict.
    pub assessments: Vec<Assessment>,
    pub metrics: Vec<Metric>,
    /// The case's privacy policy. The result holds only names, counts, ids and metrics, so it is
    /// safe under either policy; whatever stores or shows a payload must read this first.
    pub privacy: PrivacyPolicy,
}

#[derive(Debug, PartialEq)]
pub enum EvalError {
    Parse(String),
    Invalid(String),
    UnknownCase(String),
    /// The run's repository revision is not the case's fixture commit.
    RevisionMismatch {
        case_id: String,
        want: String,
        got: String,
    },
    ModelJudgeOff(String),
    BadAssessment(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "eval suite does not parse: {e}"),
            Self::Invalid(e) => write!(f, "eval suite is invalid: {e}"),
            Self::UnknownCase(c) => write!(f, "the suite has no case {c}"),
            Self::RevisionMismatch { case_id, want, got } => {
                write!(
                    f,
                    "case {case_id} runs at fixture commit {want}, the run says {got}"
                )
            }
            Self::ModelJudgeOff(c) => write!(
                f,
                "case {c} has a model score but the suite does not enable model judging"
            ),
            Self::BadAssessment(e) => write!(f, "bad assessment: {e}"),
        }
    }
}

impl std::error::Error for EvalError {}

impl EvalSuite {
    /// Reads a suite from JSON and validates it.
    pub fn parse(json: &str) -> Result<Self, EvalError> {
        let s: Self = serde_json::from_str(json).map_err(|e| EvalError::Parse(e.to_string()))?;
        s.validate()?;
        Ok(s)
    }

    pub fn validate(&self) -> Result<(), EvalError> {
        let bad = |m: String| Err(EvalError::Invalid(m));
        if self.schema_version != SUITE_SCHEMA_VERSION {
            return bad(format!(
                "schema_version {} (this build reads {SUITE_SCHEMA_VERSION})",
                self.schema_version
            ));
        }
        if self.name.trim().is_empty() || self.suite_version == 0 || self.cases.is_empty() {
            return bad("a suite needs a name, a suite_version of at least 1 and a case".into());
        }
        let mut seen = BTreeSet::new();
        for c in &self.cases {
            let id_ok = !c.case_id.is_empty()
                && !c.case_id.starts_with('.')
                && c.case_id
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
            if !id_ok {
                return bad(format!(
                    "case id {:?} is empty or starts with a dot or has characters outside [A-Za-z0-9._-]",
                    c.case_id
                ));
            }
            if !seen.insert(c.case_id.as_str()) {
                return bad(format!("case id {} appears twice", c.case_id));
            }
            if c.task_input.trim().is_empty()
                || c.fixture.repository.is_empty()
                || c.fixture.commit.is_empty()
            {
                return bad(format!(
                    "case {} needs a task input and a fixture repository and commit",
                    c.case_id
                ));
            }
            // Spec 9.1: deterministic checks are the primary gate, so a case must have one.
            if c.test_commands.is_empty() && c.expected_artifacts.is_empty() {
                return bad(format!(
                    "case {} has no deterministic check (test command or expected artifact)",
                    c.case_id
                ));
            }
            if c.envelope.max_duration_secs == 0
                || c.envelope
                    .max_cost_usd
                    .is_some_and(|v| !v.is_finite() || v < 0.0)
            {
                return bad(format!(
                    "case {} has an empty or invalid envelope",
                    c.case_id
                ));
            }
        }
        Ok(())
    }
}

/// Evaluates one recorded run against its case. Every number it makes is derived from the trace or
/// handed in by a vendor, with its quality, and an unknown stays unknown.
pub fn evaluate(suite: &EvalSuite, run: &RecordedRun) -> Result<EvaluationResult, EvalError> {
    suite.validate()?;
    let case = suite
        .cases
        .iter()
        .find(|c| c.case_id == run.case_id)
        .ok_or_else(|| EvalError::UnknownCase(run.case_id.clone()))?;
    if run.context.repository_revision != case.fixture.commit {
        return Err(EvalError::RevisionMismatch {
            case_id: case.case_id.clone(),
            want: case.fixture.commit.clone(),
            got: run.context.repository_revision.clone(),
        });
    }
    for a in &run.assessments {
        if !case.rubric.contains(&a.rubric_item) {
            return Err(EvalError::BadAssessment(format!(
                "{} is not a rubric item of case {}",
                a.rubric_item, case.case_id
            )));
        }
        if !(a.score.is_finite() && a.max_score.is_finite() && a.max_score > 0.0)
            || a.score < 0.0
            || a.score > a.max_score
        {
            return Err(EvalError::BadAssessment(format!(
                "score {} of {} is out of range",
                a.score, a.max_score
            )));
        }
        if matches!(&a.assessor, Assessor::Human { reviewer } if reviewer.trim().is_empty()) {
            return Err(EvalError::BadAssessment(
                "a human score records who the reviewer is".into(),
            ));
        }
        if let Assessor::Model {
            model,
            prompt_sha256,
            version,
        } = &a.assessor
        {
            if !suite.model_judge {
                return Err(EvalError::ModelJudgeOff(case.case_id.clone()));
            }
            if model.is_empty() || prompt_sha256.is_empty() || version.is_empty() {
                return Err(EvalError::BadAssessment(
                    "a model score records the model, the prompt hash and the version".into(),
                ));
            }
        }
    }

    let mut sorted: Vec<&TraceEvent> = run
        .events
        .iter()
        .filter(|e| e.corroborates.is_none())
        .collect();
    sorted.sort_by_key(|e| e.event_id);
    // The millisecond a uuid v7 event id carries, which aibus stamped from its own clock.
    let id_ms = |id: Uuid| {
        id.as_bytes()[..6]
            .iter()
            .fold(0u64, |a, x| a << 8 | u64::from(*x))
    };
    let derived = |name: &str, value: f64, unit: &str, source: &str| Metric {
        name: name.into(),
        value: Some(value),
        unit: unit.into(),
        quality: DataQuality::Derived,
        source: source.into(),
    };
    let count = |k: EventKind| sorted.iter().filter(|e| e.kind == k).count() as f64;
    let projection = project(&run.events);

    // Checks: the case's commands (each must be accounted for), the expected artifacts read off the
    // trace, then whatever else was recorded (compile, policy invariants).
    let mut checks: Vec<CheckResult> = Vec::new();
    for cmd in &case.test_commands {
        // Every record under the command's name counts: any Fail fails, then a NotRun (or no
        // record at all) is NotRun, and only an all-Pass set passes.
        let recorded: Vec<&CheckResult> = run.checks.iter().filter(|c| &c.name == cmd).collect();
        let any = |o: CheckOutcome| recorded.iter().any(|c| c.outcome == o);
        checks.push(CheckResult {
            kind: recorded.first().map_or(CheckKind::UnitTests, |c| c.kind),
            name: cmd.clone(),
            outcome: if any(CheckOutcome::Fail) {
                CheckOutcome::Fail
            } else if recorded.is_empty() || any(CheckOutcome::NotRun) {
                CheckOutcome::NotRun
            } else {
                CheckOutcome::Pass
            },
            evidence: recorded
                .iter()
                .flat_map(|c| c.evidence.iter().copied())
                .collect(),
        });
    }
    for art in &case.expected_artifacts {
        let hits: Vec<Uuid> = sorted
            .iter()
            .filter(|e| {
                e.kind == EventKind::ArtifactCreated
                    && e.data["name"].as_str() == Some(art.as_str())
            })
            .map(|e| e.event_id)
            .collect();
        checks.push(CheckResult {
            kind: CheckKind::ExpectedArtifact,
            name: art.clone(),
            outcome: if hits.is_empty() {
                CheckOutcome::Fail
            } else {
                CheckOutcome::Pass
            },
            evidence: hits,
        });
    }
    for c in &run.checks {
        if !case.test_commands.contains(&c.name) {
            checks.push(c.clone());
        }
    }

    // Side effects: the trace names every tool call, so a tool that can change things and is not
    // in the case's `allowed_side_effects` fails the case. A call the supervisor denied did not
    // happen. The trace carries no tool input, so a path in the list is not checked; docs/DEFERRED.md.
    const READ_ONLY_TOOLS: [&str; 11] = [
        "Read",
        "Grep",
        "Glob",
        "LS",
        "NotebookRead",
        "WebFetch",
        "WebSearch",
        "TodoWrite",
        "Task",
        "Agent",
        "ExitPlanMode",
    ];
    let denied: BTreeSet<&str> = sorted
        .iter()
        .filter(|e| e.kind == EventKind::ToolFailed && e.data["reason"] == "permission_denied")
        .filter_map(|e| e.native.tool_use_id.as_deref())
        .collect();
    let (mut broke, mut unnamed) = (Vec::new(), false);
    for e in sorted.iter().filter(|e| {
        e.kind == EventKind::ToolStarted
            && !e
                .native
                .tool_use_id
                .as_deref()
                .is_some_and(|id| denied.contains(id))
    }) {
        match e.data["tool"].as_str() {
            Some(t) if READ_ONLY_TOOLS.contains(&t) => {}
            Some(t) if case.allowed_side_effects.iter().any(|a| a == t) => {}
            Some(_) => broke.push(e.event_id),
            None => unnamed = true,
        }
    }
    checks.push(CheckResult {
        kind: CheckKind::PolicyInvariant,
        name: "allowed_side_effects".into(),
        outcome: if !broke.is_empty() {
            CheckOutcome::Fail
        } else if unnamed {
            CheckOutcome::NotRun
        } else {
            CheckOutcome::Pass
        },
        evidence: broke,
    });

    // Metrics.
    let mut metrics = Vec::new();
    let first = sorted.first().map(|e| id_ms(e.event_id));
    let last = sorted.last().map(|e| id_ms(e.event_id));
    let wall_ms = match (first, last) {
        (Some(a), Some(b)) if sorted.len() > 1 => Some((b - a) as f64),
        _ => None,
    };
    metrics.push(match wall_ms {
        Some(v) => derived(
            "wall_time_ms",
            v,
            "ms",
            "aibus clock, first to last event of the trace",
        ),
        None => Metric {
            name: "wall_time_ms".into(),
            value: None,
            unit: "ms".into(),
            quality: DataQuality::Unknown,
            source: "the trace has fewer than two events".into(),
        },
    });
    let src = "counted from the trace";
    metrics.push(derived(
        "invocations",
        count(EventKind::AttemptStarted),
        "attempts",
        src,
    ));
    metrics.push(derived(
        "delegated_tasks",
        count(EventKind::TaskDelegated),
        "tasks",
        src,
    ));
    metrics.push(derived(
        "tool_calls",
        count(EventKind::ToolStarted),
        "calls",
        src,
    ));
    metrics.push(derived(
        "tool_failures",
        count(EventKind::ToolFailed),
        "calls",
        src,
    ));
    metrics.push(derived(
        "permission_denials",
        sorted
            .iter()
            .filter(|e| e.kind == EventKind::ToolFailed && e.data["reason"] == "permission_denied")
            .count() as f64,
        "calls",
        src,
    ));
    metrics.push(derived(
        "approvals_requested",
        count(EventKind::ApprovalRequested),
        "approvals",
        src,
    ));
    metrics.push(derived(
        "failed_attempts",
        projection
            .attempts
            .iter()
            .filter(|a| {
                matches!(
                    a.status,
                    Some(AttemptStatus::Failed | AttemptStatus::Interrupted)
                )
            })
            .count() as f64,
        "attempts",
        src,
    ));
    // Parallelism: the most tools open at once, in event order.
    let (mut open, mut peak) = (BTreeSet::new(), 0usize);
    for e in &sorted {
        let Some(id) = e.native.tool_use_id.clone() else {
            continue;
        };
        match e.kind {
            EventKind::ToolStarted => {
                open.insert(id);
                peak = peak.max(open.len());
            }
            EventKind::ToolCompleted | EventKind::ToolFailed => {
                open.remove(&id);
            }
            _ => {}
        }
    }
    metrics.push(derived(
        "max_parallel_tools",
        peak as f64,
        "tools",
        "counted from the trace, in event order",
    ));

    // Usage: summed over the attempts, as weak as the weakest attempt. An attempt with no usage
    // report leaves the sum unknown for that name rather than a smaller number.
    let attempts = projection.attempts.len();
    // How well a metric is known, weakest last.
    let rank = |q: DataQuality| match q {
        DataQuality::Reported => 0,
        DataQuality::Derived => 1,
        DataQuality::Estimated => 2,
        DataQuality::Unknown => 3,
    };
    let mut usage: BTreeMap<(String, String), (f64, usize, DataQuality, String)> = BTreeMap::new();
    for a in &projection.attempts {
        for m in &a.usage {
            let Some(v) = m.value else { continue };
            let slot = usage.entry((m.name.clone(), m.unit.clone())).or_insert((
                0.0,
                0,
                m.quality,
                m.source.clone(),
            ));
            slot.0 += v;
            slot.1 += 1;
            if rank(m.quality) > rank(slot.2) {
                slot.2 = m.quality;
                slot.3.clone_from(&m.source);
            }
        }
    }
    for ((name, unit), (sum, n, quality, source)) in usage {
        if n == attempts {
            metrics.push(Metric {
                name,
                value: Some(sum),
                unit,
                quality,
                source,
            });
        } else {
            metrics.push(Metric {
                name,
                value: None,
                unit,
                quality: DataQuality::Unknown,
                source: format!("reported for {n} of {attempts} attempts"),
            });
        }
    }

    // The envelope is a check of its own.
    let cost = metrics
        .iter()
        .find(|m| m.name == "cost_usd")
        .and_then(|m| m.value);
    let over_time = wall_ms.map(|w| w > case.envelope.max_duration_secs as f64 * 1000.0);
    let over_cost = match (case.envelope.max_cost_usd, cost) {
        (Some(max), Some(c)) => Some(c > max),
        // A cost limit with no cost figure is unknown, the same as an unknown wall time.
        (Some(_), None) => None,
        (None, _) => Some(false),
    };
    checks.push(CheckResult {
        kind: CheckKind::PolicyInvariant,
        name: "within_envelope".into(),
        outcome: match (over_time, over_cost) {
            (Some(true), _) | (_, Some(true)) => CheckOutcome::Fail,
            (Some(false), Some(false)) => CheckOutcome::Pass,
            _ => CheckOutcome::NotRun,
        },
        evidence: Vec::new(),
    });

    let verdict = if checks.iter().any(|c| c.outcome == CheckOutcome::Fail) {
        Verdict::Fail
    } else if checks.iter().any(|c| c.outcome == CheckOutcome::NotRun)
        || checks.iter().all(|c| c.name == "within_envelope")
    {
        Verdict::Incomplete
    } else {
        Verdict::Pass
    };

    let ids: BTreeSet<Uuid> = run.events.iter().map(|e| e.workflow_run_id).collect();
    Ok(EvaluationResult {
        result_version: RESULT_VERSION,
        suite: suite.name.clone(),
        suite_version: suite.suite_version,
        case_id: case.case_id.clone(),
        variant: run.variant.clone(),
        workflow_run_ids: ids.into_iter().collect(),
        context: run.context.clone(),
        assessor: Assessor::Deterministic {
            tool: "aibus-trace eval".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        verdict,
        checks,
        assessments: run.assessments.clone(),
        metrics,
        privacy: case.privacy,
    })
}

impl EvaluationResult {
    pub fn metric(&self, name: &str) -> Option<&Metric> {
        self.metrics.iter().find(|m| m.name == name)
    }

    /// A metric's value, or `None` when it is missing or unknown.
    pub fn value(&self, name: &str) -> Option<f64> {
        self.metric(name).and_then(|m| m.value)
    }
}
