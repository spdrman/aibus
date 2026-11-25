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
    /// What the run may change (tool names or paths); a policy check reads it, the runner does not.
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
    Deterministic { tool: String, version: String },
    Human { reviewer: String },
    /// A model judge: the model, the prompt (by hash) and the version are all recorded.
    Model { model: String, prompt_sha256: String, version: String },
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
}

#[derive(Debug, PartialEq)]
pub enum EvalError {
    Parse(String),
    Invalid(String),
    UnknownCase(String),
    /// The run's repository revision is not the case's fixture commit.
    RevisionMismatch { case_id: String, want: String, got: String },
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
                write!(f, "case {case_id} runs at fixture commit {want}, the run says {got}")
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
    todo!("#293 red")
}
}

/// How well a metric is known, weakest last.
fn rank(q: DataQuality) -> u8 {
    match q {
        DataQuality::Reported => 0,
        DataQuality::Derived => 1,
        DataQuality::Estimated => 2,
        DataQuality::Unknown => 3,
    }
}

fn derived(name: &str, value: f64, unit: &str, source: &str) -> Metric {
    Metric {
        name: name.into(),
        value: Some(value),
        unit: unit.into(),
        quality: DataQuality::Derived,
        source: source.into(),
    }
}

/// The millisecond a uuid v7 event id carries, which aibus stamped from its own clock.
fn id_ms(id: Uuid) -> u64 {
    id.as_bytes()[..6].iter().fold(0u64, |a, x| a << 8 | u64::from(*x))
}

/// Evaluates one recorded run against its case. Every number it makes is derived from the trace or
/// handed in by a vendor, with its quality, and an unknown stays unknown.
pub fn evaluate(suite: &EvalSuite, run: &RecordedRun) -> Result<EvaluationResult, EvalError> {
    todo!("#293 red")
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

/// Counts for the checks of a result, by outcome, used by the comparison.
pub(crate) fn failed_checks(r: &EvaluationResult, kind: Option<CheckKind>) -> usize {
    r.checks
        .iter()
        .filter(|c| c.outcome == CheckOutcome::Fail && kind.is_none_or(|k| c.kind == k))
        .count()
}

