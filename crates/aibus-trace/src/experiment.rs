//! Repeatable experiments over recorded traces (#293, spec section 9.3).
//!
//! `plan` fixes an experiment before anything runs: the suite and its version, the variants with
//! their pins, how many repetitions, and for each case the order the variants run in (a rotation
//! seeded by the experiment, so no variant always goes first). The id of the manifest is a hash of
//! all of that, so the same plan always has the same id. `compare` then reads the evaluated runs
//! back against the manifest and refuses a result set that is not the same cases for every variant.
//!
//! What a comparison reports is what was observed: pass counts, wall time only when there are
//! enough samples, usage with its coverage, failure counts. It never claims one variant caused a
//! difference, and `judge` never calls a speed or cost gain an improvement when the
//! tests or the permissions got worse.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::eval::{
    failed_checks, CheckKind, EvalError, EvalSuite, EvaluationResult, Fixture, Verdict,
    MIN_BENCHMARK_CASES,
};

pub const MANIFEST_VERSION: u32 = 1;
/// A median needs this many samples before it is reported.
pub const MEDIAN_MIN_SAMPLES: usize = 3;
/// A p95 needs this many samples before it is reported.
pub const P95_MIN_SAMPLES: usize = 20;

/// One side of an A/B. `pins` is what the variant fixes (workflow file, provider extension,
/// model), as free key and value pairs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Variant {
    pub id: String,
    pub description: String,
    pub pins: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ManifestCase {
    pub case_id: String,
    pub fixture: Fixture,
    /// For each repetition, the order the variants run in.
    pub orders: Vec<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ComparisonManifest {
    pub manifest_version: u32,
    pub experiment_id: String,
    pub suite: String,
    pub suite_version: u32,
    pub repetitions: u32,
    pub variants: Vec<Variant>,
    pub cases: Vec<ManifestCase>,
}

#[derive(Debug, PartialEq)]
pub enum ExperimentError {
    Suite(EvalError),
    /// The plan itself is wrong (fewer than two variants, a repeated id, no repetitions).
    Plan(String),
    /// The results do not fit the manifest, or are not the same cases for every variant.
    Mismatch(String),
}

impl fmt::Display for ExperimentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Suite(e) => write!(f, "{e}"),
            Self::Plan(e) => write!(f, "experiment plan: {e}"),
            Self::Mismatch(e) => write!(f, "results do not fit the experiment: {e}"),
        }
    }
}

impl std::error::Error for ExperimentError {}

/// Plans an experiment: the same cases for every variant, each case's variant order rotated by a
/// hash of the experiment id and the case id, and shifted by one more per repetition.
pub fn plan(
    suite: &EvalSuite,
    variants: Vec<Variant>,
    repetitions: u32,
) -> Result<ComparisonManifest, ExperimentError> {
    todo!("#293 red")
}

/// Wall time spread. Each figure is `None` until there are enough samples to support it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Spread {
    pub samples: usize,
    pub median: Option<f64>,
    pub p95: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VariantSummary {
    pub variant: String,
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub pass_rate: f64,
    pub wall_time_ms: Spread,
    /// Share of runs with a known input plus output token count (reported by the vendor).
    pub usage_coverage: f64,
    /// Input plus output tokens summed over the runs that have them, not scaled up for the rest.
    pub tokens: Option<f64>,
    /// Share of runs with a cost figure. The figure is a client-side estimate, not a bill.
    pub cost_coverage: f64,
    pub cost_usd_estimated: Option<f64>,
    pub tool_calls: u64,
    pub delegated_tasks: u64,
    pub tool_failures: u64,
    pub permission_denials: u64,
    pub failed_attempts: u64,
    pub failed_checks: usize,
    pub policy_failures: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Comparison {
    pub experiment_id: String,
    pub suite: String,
    pub suite_version: u32,
    pub cases: usize,
    pub repetitions: u32,
    /// Fewer than `MIN_BENCHMARK_CASES` cases: a smoke run, not a benchmark.
    pub benchmark_sized: bool,
    pub variants: Vec<VariantSummary>,
    /// Everything that limits what the numbers can mean.
    pub caveats: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "judgement", rename_all = "snake_case")]
pub enum Judgement {
    /// No gate regressed and something got better. An observation, not a proof of cause.
    Improvement { better: Vec<String> },
    /// Faster, cheaper or leaner, but the tests, policy checks or permissions got worse. Never
    /// offered as an improvement.
    Unsafe { regressed: Vec<String>, better: Vec<String> },
    Regression { regressed: Vec<String> },
    NoDifference,
    Inconclusive { reason: String },
}

fn spread(mut v: Vec<f64>) -> Spread {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Spread {
        samples: n,
        median: (n >= MEDIAN_MIN_SAMPLES).then(|| {
            if n % 2 == 1 {
                v[n / 2]
            } else {
                (v[n / 2 - 1] + v[n / 2]) / 2.0
            }
        }),
        // Nearest rank.
        p95: (n >= P95_MIN_SAMPLES).then(|| v[(n as f64 * 0.95).ceil() as usize - 1]),
    }
}

/// Reads evaluated runs back against the manifest.
pub fn compare(
    manifest: &ComparisonManifest,
    results: &[EvaluationResult],
) -> Result<Comparison, ExperimentError> {
    todo!("#293 red")
}

impl Comparison {
    /// Judges `candidate` against `baseline`. A gate that got worse (pass rate, failed checks,
    /// policy invariants, permission denials) outweighs any gain, and a gain is only counted from
    /// numbers both sides have.
    pub fn judge(&self, baseline: &str, candidate: &str) -> Result<Judgement, ExperimentError> {
    todo!("#293 red")
}
}
