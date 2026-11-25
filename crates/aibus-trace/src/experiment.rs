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
    CheckKind, CheckOutcome, EvalError, EvalSuite, EvaluationResult, Fixture, Verdict,
    MIN_BENCHMARK_CASES, RESULT_VERSION,
};

pub const MANIFEST_VERSION: u32 = 1;

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
    suite.validate().map_err(ExperimentError::Suite)?;
    if variants.len() < 2 {
        return Err(ExperimentError::Plan(
            "an A/B needs at least two variants".into(),
        ));
    }
    if repetitions == 0 {
        return Err(ExperimentError::Plan(
            "repetitions must be at least 1".into(),
        ));
    }
    for (i, v) in variants.iter().enumerate() {
        if v.id.is_empty() || variants[..i].iter().any(|o| o.id == v.id) {
            return Err(ExperimentError::Plan(format!(
                "variant id {:?} is empty or repeated",
                v.id
            )));
        }
    }
    let fixed: Vec<(&str, &Fixture)> = suite
        .cases
        .iter()
        .map(|c| (c.case_id.as_str(), &c.fixture))
        .collect();
    let basis = serde_json::to_vec(&(
        &suite.name,
        suite.suite_version,
        repetitions,
        &variants,
        &fixed,
    ))
    .map_err(|e| ExperimentError::Plan(e.to_string()))?;
    let experiment_id = crate::sha256_hex(&basis);
    let cases = suite
        .cases
        .iter()
        .map(|c| {
            let h = Sha256::digest([experiment_id.as_bytes(), c.case_id.as_bytes()].concat());
            let start = u64::from_le_bytes(h[..8].try_into().unwrap_or([0; 8])) as usize;
            let orders = (0..repetitions as usize)
                .map(|r| {
                    (0..variants.len())
                        .map(|i| {
                            variants[(start % variants.len() + r + i) % variants.len()]
                                .id
                                .clone()
                        })
                        .collect()
                })
                .collect();
            ManifestCase {
                case_id: c.case_id.clone(),
                fixture: c.fixture.clone(),
                orders,
            }
        })
        .collect();
    Ok(ComparisonManifest {
        manifest_version: MANIFEST_VERSION,
        experiment_id,
        suite: suite.name.clone(),
        suite_version: suite.suite_version,
        repetitions,
        variants,
        cases,
    })
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
    Improvement {
        better: Vec<String>,
    },
    /// Faster, cheaper or leaner, but the tests, policy checks or permissions got worse. Never
    /// offered as an improvement.
    Unsafe {
        regressed: Vec<String>,
        better: Vec<String>,
    },
    Regression {
        regressed: Vec<String>,
    },
    NoDifference,
    Inconclusive {
        reason: String,
    },
}

/// Reads evaluated runs back against the manifest.
pub fn compare(
    manifest: &ComparisonManifest,
    results: &[EvaluationResult],
) -> Result<Comparison, ExperimentError> {
    let bad = |m: String| Err(ExperimentError::Mismatch(m));
    if manifest.manifest_version != MANIFEST_VERSION
        || manifest.repetitions == 0
        || manifest.variants.len() < 2
        || manifest.cases.is_empty()
    {
        return bad("the manifest is not a plan this build made".into());
    }
    if let Some(r) = results.iter().find(|r| r.result_version != RESULT_VERSION) {
        return bad(format!(
            "{}/{} has result_version {}, this build reads {RESULT_VERSION}",
            r.case_id, r.variant, r.result_version
        ));
    }
    // Wall time spread. A median needs 3 samples before it is reported, a p95 needs 20.
    let spread = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let n = v.len();
        Spread {
            samples: n,
            median: (n >= 3).then(|| {
                if n % 2 == 1 {
                    v[n / 2]
                } else {
                    (v[n / 2 - 1] + v[n / 2]) / 2.0
                }
            }),
            // Nearest rank.
            p95: (n >= 20).then(|| v[(n as f64 * 0.95).ceil() as usize - 1]),
        }
    };
    for r in results {
        if r.suite != manifest.suite || r.suite_version != manifest.suite_version {
            return bad(format!(
                "{}/{} comes from suite {} v{}",
                r.case_id, r.variant, r.suite, r.suite_version
            ));
        }
        let Some(case) = manifest.cases.iter().find(|c| c.case_id == r.case_id) else {
            return bad(format!("case {} is not in the manifest", r.case_id));
        };
        if !manifest.variants.iter().any(|v| v.id == r.variant) {
            return bad(format!("variant {} is not in the manifest", r.variant));
        }
        if r.context.repository_revision != case.fixture.commit {
            return bad(format!(
                "{}/{} ran at {}, not the fixture commit",
                r.case_id, r.variant, r.context.repository_revision
            ));
        }
    }
    let failed_checks = |r: &EvaluationResult, kind: Option<CheckKind>| {
        r.checks
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Fail && kind.is_none_or(|k| c.kind == k))
            .count()
    };
    let mut summaries = Vec::new();
    let mut caveats = Vec::new();
    for v in &manifest.variants {
        for c in &manifest.cases {
            let n = results
                .iter()
                .filter(|r| r.variant == v.id && r.case_id == c.case_id)
                .count();
            if n != manifest.repetitions as usize {
                return bad(format!(
                    "variant {} has {n} runs of case {}, the plan has {}",
                    v.id, c.case_id, manifest.repetitions
                ));
            }
        }
        let mine: Vec<&EvaluationResult> = results.iter().filter(|r| r.variant == v.id).collect();
        let runs = mine.len();
        let count =
            |name: &str| -> u64 { mine.iter().filter_map(|r| r.value(name)).sum::<f64>() as u64 };
        let tokens: Vec<f64> = mine
            .iter()
            .filter_map(|r| Some(r.value("input_tokens")? + r.value("output_tokens")?))
            .collect();
        let costs: Vec<f64> = mine.iter().filter_map(|r| r.value("cost_usd")).collect();
        let share = |n: usize| n as f64 / runs as f64;
        let verdicts = |w: Verdict| mine.iter().filter(|r| r.verdict == w).count();
        summaries.push(VariantSummary {
            variant: v.id.clone(),
            runs,
            passed: verdicts(Verdict::Pass),
            failed: verdicts(Verdict::Fail),
            incomplete: verdicts(Verdict::Incomplete),
            pass_rate: share(verdicts(Verdict::Pass)),
            wall_time_ms: spread(
                mine.iter()
                    .filter_map(|r| r.value("wall_time_ms"))
                    .collect(),
            ),
            usage_coverage: share(tokens.len()),
            tokens: (!tokens.is_empty()).then(|| tokens.iter().sum()),
            cost_coverage: share(costs.len()),
            cost_usd_estimated: (!costs.is_empty()).then(|| costs.iter().sum()),
            tool_calls: count("tool_calls"),
            delegated_tasks: count("delegated_tasks"),
            tool_failures: count("tool_failures"),
            permission_denials: count("permission_denials"),
            failed_attempts: count("failed_attempts"),
            failed_checks: mine.iter().map(|r| failed_checks(r, None)).sum(),
            policy_failures: mine
                .iter()
                .map(|r| failed_checks(r, Some(CheckKind::PolicyInvariant)))
                .sum(),
        });
        if mine.iter().any(|r| r.context.concurrent_runs > 1) {
            caveats.push(format!(
                "variant {}: runs overlapped, so wall times are not isolated",
                v.id
            ));
        }
    }
    let differs = |label: &str, pick: &dyn Fn(&EvaluationResult) -> String| {
        let mut seen: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for v in &manifest.variants {
            let versions: std::collections::BTreeSet<String> = results
                .iter()
                .filter(|r| r.variant == v.id)
                .map(pick)
                .collect();
            seen.entry(versions.into_iter().collect::<Vec<_>>().join("+"))
                .or_default()
                .push(&v.id);
        }
        (seen.len() > 1).then(|| {
            format!(
                "the variants ran with different {label}s: {}",
                seen.iter()
                    .map(|(k, v)| format!("{} on {k}", v.join(",")))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })
    };
    caveats.extend(differs("CLI version", &|r| {
        r.context
            .cli_version
            .clone()
            .unwrap_or_else(|| "unknown".into())
    }));
    caveats.extend(differs("extension version", &|r| {
        r.context.extension_version.clone()
    }));
    let benchmark_sized = manifest.cases.len() >= MIN_BENCHMARK_CASES;
    if !benchmark_sized {
        caveats.push(format!(
            "{} cases is below the {MIN_BENCHMARK_CASES} a benchmark needs",
            manifest.cases.len()
        ));
    }
    if manifest.repetitions == 1 {
        caveats.push("one run per case: no estimate of run to run variability".into());
    }
    Ok(Comparison {
        experiment_id: manifest.experiment_id.clone(),
        suite: manifest.suite.clone(),
        suite_version: manifest.suite_version,
        cases: manifest.cases.len(),
        repetitions: manifest.repetitions,
        benchmark_sized,
        variants: summaries,
        caveats,
    })
}

impl Comparison {
    /// Judges `candidate` against `baseline`. A gate that got worse (pass rate, failed checks,
    /// policy invariants, permission denials) outweighs any gain, and a gain is only counted from
    /// numbers both sides have.
    pub fn judge(&self, baseline: &str, candidate: &str) -> Result<Judgement, ExperimentError> {
        let find = |id: &str| {
            self.variants
                .iter()
                .find(|v| v.variant == id)
                .ok_or_else(|| {
                    ExperimentError::Mismatch(format!("no variant {id} in the comparison"))
                })
        };
        let (a, b) = (find(baseline)?, find(candidate)?);
        let mut regressed = Vec::new();
        if b.pass_rate < a.pass_rate {
            regressed.push(format!(
                "pass rate {:.2} against {:.2}",
                b.pass_rate, a.pass_rate
            ));
        }
        if b.failed_checks > a.failed_checks {
            regressed.push(format!(
                "failed checks {} against {}",
                b.failed_checks, a.failed_checks
            ));
        }
        if b.policy_failures > a.policy_failures {
            regressed.push(format!(
                "policy invariant failures {} against {}",
                b.policy_failures, a.policy_failures
            ));
        }
        if b.permission_denials > a.permission_denials {
            regressed.push(format!(
                "permission denials {} against {}",
                b.permission_denials, a.permission_denials
            ));
        }
        let mut better = Vec::new();
        if let (Some(x), Some(y)) = (a.wall_time_ms.median, b.wall_time_ms.median) {
            if y < x {
                better.push(format!("median wall time {y:.0} ms against {x:.0} ms"));
            }
        }
        if b.tool_calls < a.tool_calls {
            better.push(format!(
                "tool calls {} against {}",
                b.tool_calls, a.tool_calls
            ));
        }
        if let (Some(x), Some(y)) = (a.tokens, b.tokens) {
            if a.usage_coverage == 1.0 && b.usage_coverage == 1.0 && y < x {
                better.push(format!("tokens {y:.0} against {x:.0}"));
            }
        }
        if let (Some(x), Some(y)) = (a.cost_usd_estimated, b.cost_usd_estimated) {
            if a.cost_coverage == 1.0 && b.cost_coverage == 1.0 && y < x {
                better.push(format!("estimated cost {y:.4} usd against {x:.4} usd"));
            }
        }
        Ok(if !regressed.is_empty() && !better.is_empty() {
            Judgement::Unsafe { regressed, better }
        } else if !regressed.is_empty() {
            Judgement::Regression { regressed }
        } else if !self.benchmark_sized {
            Judgement::Inconclusive {
                reason: format!(
                    "{} cases, a benchmark needs {MIN_BENCHMARK_CASES}",
                    self.cases
                ),
            }
        } else if better.is_empty() {
            Judgement::NoDifference
        } else {
            Judgement::Improvement { better }
        })
    }
}
