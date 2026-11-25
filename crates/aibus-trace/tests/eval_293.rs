//! #293: evaluation cases, versioned results with assessor provenance, repeatable experiments over
//! recorded traces and evidence-backed suggestions. The traces here are built in the test (the
//! fixtures are synthetic runs, not recordings of a provider), the commands' outcomes are handed in.

use std::time::{Duration, SystemTime};

use aibus_trace::eval::{
    evaluate, Assessment, Assessor, CheckKind, CheckOutcome, CheckResult, EvalError, EvalSuite,
    EvaluationResult, RecordedRun, RunContext, Verdict, RESULT_VERSION,
};
use aibus_trace::experiment::{compare, plan, Judgement, Variant};
use aibus_trace::suggest::{suggest, Basis, Category, Label};
use aibus_trace::{AttemptRef, DataQuality, EventIds, EventKind, Origin, TraceEvent, Visibility};
use serde_json::{json, Value};
use uuid::Uuid;

const SUITE: &str = include_str!("../../../fixtures/eval-293/suite.json");

fn suite() -> EvalSuite {
    EvalSuite::parse(SUITE).unwrap()
}

/// What a synthetic run looks like.
#[derive(Clone)]
struct Shape {
    greps: usize,
    reads: usize,
    wall_ms: u64,
    tokens: Option<(f64, f64)>,
    cost: Option<f64>,
    denied: usize,
    failed_edits: usize,
    artifact: bool,
    /// Children delegated to that run no tool.
    idle_kids: usize,
    /// Children that run one tool each, one after another.
    serial_kids: usize,
    /// Bash calls that ran (and were not denied).
    bash: usize,
    /// WebFetch and WebSearch calls that ran.
    web: usize,
    /// Bash calls the supervisor denied and that then completed under the same id.
    denied_then_ran: usize,
}

impl Shape {
    fn base() -> Self {
        Self {
            greps: 3,
            reads: 3,
            wall_ms: 3_000,
            tokens: Some((1000.0, 500.0)),
            cost: Some(0.5),
            denied: 0,
            failed_edits: 0,
            artifact: true,
            idle_kids: 0,
            serial_kids: 0,
            bash: 0,
            web: 0,
            denied_then_ran: 0,
        }
    }
}

struct Builder {
    evs: Vec<TraceEvent>,
    ids: EventIds,
    ms: u64,
    attempt: AttemptRef,
}

impl Builder {
    fn new(seed: u128) -> Self {
        Self {
            evs: Vec::new(),
            ids: EventIds::default(),
            ms: 0,
            attempt: AttemptRef {
                workflow_run_id: Uuid::from_u128(seed),
                attempt_id: Uuid::from_u128(seed + 1),
                parent_attempt_id: None,
                provider_extension: "org.aibus.test".into(),
            },
        }
    }

    fn add(
        &mut self,
        kind: EventKind,
        agent: Option<&str>,
        tool_use: Option<&str>,
        data: Value,
    ) -> Uuid {
        self.ms += 10;
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_000 + self.ms);
        let n = self.evs.len() as u64;
        let id = self.ids.next(
            at,
            &[
                &n.to_le_bytes()[..],
                &self.attempt.attempt_id.as_bytes()[..],
            ],
        );
        let mut e = TraceEvent::new(
            &self.attempt,
            id,
            kind,
            Origin::CliStream,
            Visibility::Observed,
            at,
            "test/1",
        );
        e.agent_id = agent.map(str::to_owned);
        e.native.tool_use_id = tool_use.map(str::to_owned);
        e.data = data;
        self.evs.push(e);
        id
    }

    fn tool(&mut self, agent: &str, name: &str, id: &str, fail: Option<&str>) {
        self.add(
            EventKind::ToolStarted,
            Some(agent),
            Some(id),
            json!({ "tool": name }),
        );
        match fail {
            None => self.add(
                EventKind::ToolCompleted,
                Some(agent),
                Some(id),
                json!({ "tool": name }),
            ),
            Some(r) => self.add(
                EventKind::ToolFailed,
                Some(agent),
                Some(id),
                json!({ "tool": name, "reason": r }),
            ),
        };
    }

    fn kid(&mut self, n: usize, with_tool: bool) {
        let (kid, tu) = (format!("kid{n}"), format!("tu-kid{n}"));
        self.add(
            EventKind::TaskDelegated,
            Some("lead"),
            Some(&tu),
            json!({ "subagent_type": "reader" }),
        );
        let id = self.add(
            EventKind::AgentStarted,
            Some(&kid),
            Some(&tu),
            json!({ "subagent_type": "reader" }),
        );
        let e = self.evs.iter_mut().find(|e| e.event_id == id).unwrap();
        e.parent_agent_id = Some("lead".into());
        e.parent_tool_use_id = Some(tu.clone());
        if with_tool {
            self.tool(&kid, "Read", &format!("tool-kid{n}"), None);
        }
        self.add(
            EventKind::AgentStopped,
            Some(&kid),
            None,
            json!({ "status": "stopped" }),
        );
    }
}

fn trace(seed: u128, s: &Shape) -> Vec<TraceEvent> {
    let mut b = Builder::new(seed);
    b.add(EventKind::AttemptStarted, None, None, json!({}));
    b.add(
        EventKind::AgentStarted,
        Some("lead"),
        None,
        json!({ "role": "lead" }),
    );
    for i in 0..s.greps {
        b.tool("lead", "Grep", &format!("g{i}"), None);
    }
    for i in 0..s.reads {
        b.tool("lead", "Read", &format!("r{i}"), None);
    }
    for i in 0..s.denied {
        b.tool("lead", "Bash", &format!("d{i}"), Some("permission_denied"));
    }
    for i in 0..s.bash {
        b.tool("lead", "Bash", &format!("b{i}"), None);
    }
    for i in 0..s.web {
        b.tool("lead", "WebFetch", &format!("wf{i}"), None);
        b.tool("lead", "WebSearch", &format!("ws{i}"), None);
    }
    for i in 0..s.denied_then_ran {
        // The same id is denied and then completes: the call did run.
        b.tool("lead", "Bash", &format!("dr{i}"), Some("permission_denied"));
        b.add(
            EventKind::ToolCompleted,
            Some("lead"),
            Some(&format!("dr{i}")),
            json!({ "tool": "Bash" }),
        );
    }
    for i in 0..s.failed_edits {
        b.tool("lead", "Edit", &format!("f{i}"), Some("error"));
    }
    for i in 0..s.idle_kids {
        b.kid(i, false);
    }
    for i in 0..s.serial_kids {
        b.kid(100 + i, true);
    }
    if s.artifact {
        b.add(
            EventKind::ArtifactCreated,
            Some("lead"),
            None,
            json!({ "name": "report.md" }),
        );
    }
    let metric = |name: &str, v: f64, unit: &str, q: &str| json!({ "name": name, "value": v, "unit": unit, "quality": q, "source": "result.usage" });
    let mut metrics = Vec::new();
    if let Some((i, o)) = s.tokens {
        metrics.push(metric("input_tokens", i, "tokens", "reported"));
        metrics.push(metric("output_tokens", o, "tokens", "reported"));
    }
    if let Some(c) = s.cost {
        metrics.push(metric("cost_usd", c, "usd", "estimated"));
    }
    b.add(
        EventKind::UsageReported,
        Some("lead"),
        None,
        json!({ "scope": "attempt", "metrics": metrics }),
    );
    // The last event sits exactly wall_ms after the first.
    b.ms = s.wall_ms;
    b.add(
        EventKind::AttemptCompleted,
        None,
        None,
        json!({ "status": "completed" }),
    );
    b.evs
}

fn context(commit: &str) -> RunContext {
    RunContext {
        repository_revision: commit.into(),
        extension_version: "1.0.0".into(),
        cli_version: Some("2.1.0".into()),
        concurrent_runs: 1,
        cache_note: None,
    }
}

fn run(
    suite: &EvalSuite,
    case: usize,
    variant: &str,
    seed: u128,
    s: &Shape,
    tests_pass: bool,
) -> RecordedRun {
    let c = &suite.cases[case];
    RecordedRun {
        case_id: c.case_id.clone(),
        variant: variant.into(),
        events: trace(seed, s),
        checks: vec![
            CheckResult {
                kind: CheckKind::UnitTests,
                name: c.test_commands[0].clone(),
                outcome: if tests_pass {
                    CheckOutcome::Pass
                } else {
                    CheckOutcome::Fail
                },
                evidence: vec![],
            },
            CheckResult {
                kind: CheckKind::PolicyInvariant,
                name: "no_writes_outside_fixture".into(),
                outcome: CheckOutcome::Pass,
                evidence: vec![],
            },
        ],
        assessments: vec![],
        context: context(&c.fixture.commit),
    }
}

fn variants() -> Vec<Variant> {
    ["baseline", "lean"]
        .into_iter()
        .map(|id| Variant {
            id: id.into(),
            description: format!("{id} workflow"),
            pins: [("workflow".to_owned(), format!("{id}.toml"))].into(),
        })
        .collect()
}

/// Ten cases, two variants, `reps` runs each. `lean` is shaped by `lean_for`.
fn experiment(
    reps: u32,
    lean_for: impl Fn(usize) -> (Shape, bool),
) -> (
    EvalSuite,
    aibus_trace::experiment::ComparisonManifest,
    Vec<EvaluationResult>,
) {
    let su = suite();
    let manifest = plan(&su, variants(), reps).unwrap();
    let mut results = Vec::new();
    for case in 0..su.cases.len() {
        for rep in 0..reps as u128 {
            let seed = 10_000 + (case as u128) * 100 + rep * 10;
            let mut base = Shape::base();
            base.wall_ms = 3_000 + case as u64 * 50;
            results.push(evaluate(&su, &run(&su, case, "baseline", seed, &base, true)).unwrap());
            let (lean, pass) = lean_for(case);
            results.push(evaluate(&su, &run(&su, case, "lean", seed + 5, &lean, pass)).unwrap());
        }
    }
    (su, manifest, results)
}

fn lean(case: usize) -> Shape {
    Shape {
        greps: 1,
        reads: 2,
        wall_ms: 2_000 + case as u64 * 50,
        tokens: Some((800.0, 400.0)),
        cost: Some(0.3),
        ..Shape::base()
    }
}

// ---- the suite ----

#[test]
fn the_fixture_suite_has_ten_valid_cases_and_one_per_agent_case() {
    let s = suite();
    assert_eq!(s.cases.len(), 10);
    assert!(s.cases.len() >= aibus_trace::eval::MIN_BENCHMARK_CASES);
    // #392's per-agent evals fit the same schema: the case names its agent, the rest is unchanged.
    assert_eq!(s.cases[9].agent.as_deref(), Some("reviewer"));
    assert!(s.cases[0].agent.is_none());
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(EvalSuite::parse(&json).unwrap(), s);
    assert!(!serde_json::to_string(&s.cases[0])
        .unwrap()
        .contains("agent"));
}

#[test]
fn a_bad_suite_is_refused() {
    let mut s = suite();
    s.cases[1].case_id = s.cases[0].case_id.clone();
    assert!(matches!(s.validate(), Err(EvalError::Invalid(m)) if m.contains("twice")));

    let mut s = suite();
    s.cases[0].test_commands.clear();
    s.cases[0].expected_artifacts.clear();
    assert!(
        matches!(s.validate(), Err(EvalError::Invalid(m)) if m.contains("no deterministic check"))
    );

    let mut s = suite();
    s.schema_version = 2;
    assert!(s.validate().is_err());

    let mut s = suite();
    s.cases[0].case_id = "has space".into();
    assert!(s.validate().is_err());

    let with_typo = SUITE.replacen("\"rubric\"", "\"rubrik\"", 1);
    assert!(matches!(
        EvalSuite::parse(&with_typo),
        Err(EvalError::Parse(_))
    ));
}

// ---- the result ----

#[test]
fn a_passing_run_gives_a_versioned_result_with_provenance() {
    let su = suite();
    let r = evaluate(&su, &run(&su, 0, "baseline", 1, &Shape::base(), true)).unwrap();
    assert_eq!(r.result_version, RESULT_VERSION);
    assert_eq!((r.suite.as_str(), r.suite_version), ("aibus-small", 1));
    assert_eq!(r.verdict, Verdict::Pass);
    assert!(
        matches!(&r.assessor, Assessor::Deterministic { tool, .. } if tool == "aibus-trace eval")
    );
    let names: Vec<&str> = r.checks.iter().map(|c| c.name.as_str()).collect();
    for want in [
        "cargo test -p case1",
        "report.md",
        "no_writes_outside_fixture",
        "within_envelope",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    // The artifact check points at the event that created it.
    let art = r.checks.iter().find(|c| c.name == "report.md").unwrap();
    assert_eq!(
        (art.kind, art.outcome, art.evidence.len()),
        (CheckKind::ExpectedArtifact, CheckOutcome::Pass, 1)
    );
    assert_eq!(r.context.repository_revision, su.cases[0].fixture.commit);
    assert_eq!(r.value("wall_time_ms"), Some(3_000.0));
    assert_eq!(r.value("tool_calls"), Some(6.0));
    let back: EvaluationResult = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn a_failed_command_or_a_missing_artifact_fails_and_a_missing_outcome_is_not_a_pass() {
    let su = suite();
    let failed = evaluate(&su, &run(&su, 0, "v", 1, &Shape::base(), false)).unwrap();
    assert_eq!(failed.verdict, Verdict::Fail);

    let no_artifact = Shape {
        artifact: false,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &no_artifact, true)).unwrap();
    assert_eq!(r.verdict, Verdict::Fail);
    assert!(r.checks.iter().any(|c| c.name == "report.md"
        && c.outcome == CheckOutcome::Fail
        && c.evidence.is_empty()));

    let mut unreported = run(&su, 0, "v", 1, &Shape::base(), true);
    unreported.checks.retain(|c| c.kind != CheckKind::UnitTests);
    let r = evaluate(&su, &unreported).unwrap();
    assert_eq!(r.verdict, Verdict::Incomplete);
    assert!(r
        .checks
        .iter()
        .any(|c| c.name == "cargo test -p case1" && c.outcome == CheckOutcome::NotRun));
}

#[test]
fn a_run_over_its_envelope_fails() {
    let su = suite();
    let slow = Shape {
        wall_ms: 601_000,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &slow, true)).unwrap();
    assert_eq!(r.verdict, Verdict::Fail);
    assert!(r
        .checks
        .iter()
        .any(|c| c.name == "within_envelope" && c.outcome == CheckOutcome::Fail));
    let dear = Shape {
        cost: Some(9.0),
        ..Shape::base()
    };
    assert_eq!(
        evaluate(&su, &run(&su, 0, "v", 1, &dear, true))
            .unwrap()
            .verdict,
        Verdict::Fail
    );
}

#[test]
fn a_run_at_another_revision_is_refused() {
    let su = suite();
    let mut r = run(&su, 0, "v", 1, &Shape::base(), true);
    r.context.repository_revision = "ffff".into();
    assert!(matches!(
        evaluate(&su, &r),
        Err(EvalError::RevisionMismatch { .. })
    ));
    r.case_id = "nope".into();
    assert!(matches!(evaluate(&su, &r), Err(EvalError::UnknownCase(_))));
}

#[test]
fn scores_are_secondary_and_a_model_judge_needs_the_suite_to_allow_it() {
    let mut su = suite();
    let human = Assessment {
        assessor: Assessor::Human {
            reviewer: "rom".into(),
        },
        rubric_item: "correctness".into(),
        score: 4.0,
        max_score: 5.0,
    };
    let model = Assessment {
        assessor: Assessor::Model {
            model: "judge-1".into(),
            prompt_sha256: "ab12".into(),
            version: "3".into(),
        },
        rubric_item: "minimal_diff".into(),
        score: 2.0,
        max_score: 5.0,
    };
    let mut r = run(&su, 0, "v", 1, &Shape::base(), true);
    r.assessments = vec![human.clone(), model.clone()];
    assert!(matches!(
        evaluate(&su, &r),
        Err(EvalError::ModelJudgeOff(_))
    ));

    su.model_judge = true;
    let ok = evaluate(&su, &r).unwrap();
    // A low model score does not change the deterministic verdict, and the provenance is kept.
    assert_eq!(ok.verdict, Verdict::Pass);
    assert_eq!(ok.assessments, vec![human, model]);

    r.assessments[1].assessor = Assessor::Model {
        model: "judge-1".into(),
        prompt_sha256: String::new(),
        version: "3".into(),
    };
    assert!(matches!(
        evaluate(&su, &r),
        Err(EvalError::BadAssessment(_))
    ));
    r.assessments[1] = Assessment {
        rubric_item: "not_in_rubric".into(),
        ..r.assessments[0].clone()
    };
    assert!(matches!(
        evaluate(&su, &r),
        Err(EvalError::BadAssessment(_))
    ));
    r.assessments[1] = Assessment {
        score: 9.0,
        ..r.assessments[0].clone()
    };
    assert!(matches!(
        evaluate(&su, &r),
        Err(EvalError::BadAssessment(_))
    ));
}

#[test]
fn usage_keeps_its_quality_and_a_gap_stays_unknown() {
    let su = suite();
    let r = evaluate(&su, &run(&su, 0, "v", 1, &Shape::base(), true)).unwrap();
    let t = r.metric("input_tokens").unwrap();
    assert_eq!((t.value, t.quality), (Some(1000.0), DataQuality::Reported));
    assert_eq!(
        r.metric("cost_usd").unwrap().quality,
        DataQuality::Estimated
    );
    assert_eq!(
        r.metric("tool_calls").unwrap().quality,
        DataQuality::Derived
    );

    let none = Shape {
        tokens: None,
        cost: None,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &none, true)).unwrap();
    assert!(
        r.metric("input_tokens").is_none(),
        "no usage report, no invented number"
    );
    assert!(r.value("cost_usd").is_none());
}

// ---- the experiment ----

#[test]
fn a_plan_is_repeatable_and_fair() {
    let su = suite();
    let a = plan(&su, variants(), 2).unwrap();
    assert_eq!(a, plan(&su, variants(), 2).unwrap());
    assert_ne!(
        a.experiment_id,
        plan(&su, variants(), 3).unwrap().experiment_id
    );
    let mut other = variants();
    other[1].pins.insert("model".into(), "small".into());
    assert_ne!(a.experiment_id, plan(&su, other, 2).unwrap().experiment_id);

    let mut first = std::collections::BTreeSet::new();
    for c in &a.cases {
        assert_eq!(c.orders.len(), 2);
        for o in &c.orders {
            let mut sorted = o.clone();
            sorted.sort();
            assert_eq!(
                sorted,
                ["baseline", "lean"],
                "every variant runs every case"
            );
        }
        assert_ne!(
            c.orders[0], c.orders[1],
            "the order flips between repetitions"
        );
        first.insert(c.orders[0][0].clone());
    }
    assert_eq!(first.len(), 2, "no variant always goes first");

    assert!(plan(&su, variants()[..1].to_vec(), 1).is_err());
    assert!(plan(&su, variants(), 0).is_err());
    let mut dup = variants();
    dup[1].id = "baseline".into();
    assert!(plan(&su, dup, 1).is_err());
}

#[test]
fn ten_cases_two_variants_report_what_was_observed() {
    let (_, manifest, results) = experiment(1, |c| (lean(c), true));
    let cmp = compare(&manifest, &results).unwrap();
    assert!(cmp.benchmark_sized);
    assert_eq!((cmp.cases, cmp.repetitions), (10, 1));
    let (a, b) = (&cmp.variants[0], &cmp.variants[1]);
    assert_eq!(
        (a.variant.as_str(), b.variant.as_str()),
        ("baseline", "lean")
    );
    assert_eq!((a.runs, a.passed, a.failed, a.pass_rate), (10, 10, 0, 1.0));
    assert_eq!(a.tool_calls, 60);
    assert_eq!(b.tool_calls, 30);
    assert_eq!(a.usage_coverage, 1.0);
    assert_eq!(a.tokens, Some(15_000.0));
    assert_eq!(a.cost_usd_estimated, Some(5.0));
    // Ten samples support a median, not a p95.
    assert_eq!(a.wall_time_ms.samples, 10);
    assert_eq!(a.wall_time_ms.median, Some(3_225.0));
    assert_eq!(a.wall_time_ms.p95, None);
    // One run per case says so, and nothing claims a cause.
    assert!(cmp.caveats.iter().any(|c| c.contains("one run per case")));
    match cmp.judge("baseline", "lean").unwrap() {
        Judgement::Improvement { better } => {
            assert!(better.iter().any(|b| b.starts_with("median wall time")))
        }
        j => panic!("{j:?}"),
    }
}

#[test]
fn a_p95_appears_only_with_enough_samples() {
    let (_, manifest, results) = experiment(2, |c| (lean(c), true));
    let cmp = compare(&manifest, &results).unwrap();
    assert_eq!(cmp.variants[0].wall_time_ms.samples, 20);
    assert!(cmp.variants[0].wall_time_ms.p95.is_some());
    assert!(!cmp.caveats.iter().any(|c| c.contains("one run per case")));
}

#[test]
fn missing_coverage_is_reported_not_filled_in() {
    let (_, manifest, results) = experiment(1, |c| {
        let mut s = lean(c);
        if c < 4 {
            // Cost stays: with a cost limit in the case, an unknown cost is Incomplete, which
            // an_unknown_cost_is_not_inside_a_cost_envelope covers.
            s.tokens = None;
        }
        (s, true)
    });
    let cmp = compare(&manifest, &results).unwrap();
    let b = &cmp.variants[1];
    assert_eq!(b.usage_coverage, 0.6);
    assert_eq!(
        b.tokens,
        Some(7_200.0),
        "summed over the six runs that have usage, not scaled to ten"
    );
    // With partial coverage, tokens are not compared.
    match cmp.judge("baseline", "lean").unwrap() {
        Judgement::Improvement { better } => {
            assert!(!better.iter().any(|b| b.starts_with("tokens")))
        }
        j => panic!("{j:?}"),
    }
}

#[test]
fn results_that_are_not_the_same_cases_for_every_variant_are_refused() {
    let (_, manifest, mut results) = experiment(1, |c| (lean(c), true));
    let at = results
        .iter()
        .position(|r| r.variant == "lean" && r.case_id.starts_with("case-03"))
        .unwrap();
    results.remove(at);
    assert!(
        matches!(compare(&manifest, &results), Err(aibus_trace::experiment::ExperimentError::Mismatch(m)) if m.contains("case-03"))
    );

    let (_, manifest, mut results) = experiment(1, |c| (lean(c), true));
    results[0].suite_version = 2;
    assert!(compare(&manifest, &results).is_err());
    let (_, manifest, mut results) = experiment(1, |c| (lean(c), true));
    results[0].variant = "ghost".into();
    assert!(compare(&manifest, &results).is_err());
}

#[test]
fn a_faster_variant_that_fails_tests_is_not_an_improvement() {
    let (_, manifest, results) = experiment(1, |c| (lean(c), c != 6));
    let cmp = compare(&manifest, &results).unwrap();
    assert_eq!(cmp.variants[1].passed, 9);
    match cmp.judge("baseline", "lean").unwrap() {
        Judgement::Unsafe { regressed, better } => {
            assert!(regressed.iter().any(|r| r.starts_with("pass rate")));
            assert!(!better.is_empty());
        }
        j => panic!("expected Unsafe, got {j:?}"),
    }
}

#[test]
fn a_faster_variant_that_gets_denied_permissions_is_not_an_improvement() {
    let (_, manifest, results) = experiment(1, |c| {
        (
            Shape {
                denied: usize::from(c == 2),
                ..lean(c)
            },
            true,
        )
    });
    let cmp = compare(&manifest, &results).unwrap();
    match cmp.judge("baseline", "lean").unwrap() {
        Judgement::Unsafe { regressed, .. } => assert!(regressed
            .iter()
            .any(|r| r.starts_with("permission denials"))),
        j => panic!("expected Unsafe, got {j:?}"),
    }
}

#[test]
fn a_variant_that_breaks_a_policy_check_is_not_an_improvement() {
    let su = suite();
    let manifest = plan(&su, variants(), 1).unwrap();
    let mut results = Vec::new();
    for case in 0..10 {
        let mut base = Shape::base();
        base.wall_ms = 3_000;
        results.push(
            evaluate(
                &su,
                &run(&su, case, "baseline", 100 + case as u128 * 10, &base, true),
            )
            .unwrap(),
        );
        let mut r = run(
            &su,
            case,
            "lean",
            105 + case as u128 * 10,
            &lean(case),
            true,
        );
        if case == 0 {
            r.checks[1].outcome = CheckOutcome::Fail;
        }
        results.push(evaluate(&su, &r).unwrap());
    }
    let cmp = compare(&manifest, &results).unwrap();
    assert_eq!(cmp.variants[1].policy_failures, 1);
    assert!(matches!(
        cmp.judge("baseline", "lean").unwrap(),
        Judgement::Unsafe { .. }
    ));
}

#[test]
fn a_small_or_overlapping_or_mixed_environment_run_says_so() {
    let su = suite();
    let mut small = su.clone();
    small.cases.truncate(4);
    let manifest = plan(&small, variants(), 1).unwrap();
    let mut results = Vec::new();
    for case in 0..4 {
        let mut a = run(
            &small,
            case,
            "baseline",
            1 + case as u128 * 10,
            &Shape::base(),
            true,
        );
        a.context.concurrent_runs = 3;
        let mut b = run(
            &small,
            case,
            "lean",
            5 + case as u128 * 10,
            &lean(case),
            true,
        );
        b.context.cli_version = Some("2.2.0".into());
        results.push(evaluate(&small, &a).unwrap());
        results.push(evaluate(&small, &b).unwrap());
    }
    let cmp = compare(&manifest, &results).unwrap();
    assert!(!cmp.benchmark_sized);
    assert!(cmp.caveats.iter().any(|c| c.contains("below the 10")));
    assert!(cmp.caveats.iter().any(|c| c.contains("overlapped")));
    assert!(cmp
        .caveats
        .iter()
        .any(|c| c.contains("CLI versions") && c.contains("2.2.0")));
    assert!(matches!(
        cmp.judge("baseline", "lean").unwrap(),
        Judgement::Inconclusive { .. }
    ));
}

// ---- the suggestions ----

fn result_for(events: &[TraceEvent], tests_pass: bool) -> EvaluationResult {
    let su = suite();
    let mut r = run(&su, 0, "v", 1, &Shape::base(), tests_pass);
    r.events = events.to_vec();
    evaluate(&su, &r).unwrap()
}

#[test]
fn every_suggestion_points_at_events_in_the_run_and_never_promises_a_saving() {
    let s = Shape {
        greps: 4,
        failed_edits: 2,
        idle_kids: 1,
        serial_kids: 2,
        ..Shape::base()
    };
    let events = trace(7, &s);
    let found = suggest(&result_for(&events, true), &events);
    let cats: Vec<Category> = found.iter().map(|s| s.category).collect();
    for want in [
        Category::RepeatedSearch,
        Category::RepeatedFailure,
        Category::UnusedDelegation,
        Category::SerializableDelegation,
    ] {
        assert!(cats.contains(&want), "{want:?} missing from {cats:?}");
    }
    let ids: Vec<Uuid> = events.iter().map(|e| e.event_id).collect();
    for s in &found {
        assert!(!s.trace_event_ids.is_empty());
        assert!(
            s.trace_event_ids.iter().all(|i| ids.contains(i)),
            "{s:?} cites an event that is not in the run"
        );
        assert!(s.safe_to_recommend);
        let text = format!("{} {}", s.observation, s.suggestion);
        assert!(
            !text.chars().any(|c| c == '%' || c == '$'),
            "a number of savings in {text:?}"
        );
    }
    let by = |c| found.iter().find(|s| s.category == c).unwrap();
    // A pattern the trace cannot show is a heuristic hypothesis; a plain repeat failure is evidence.
    assert_eq!(
        (
            by(Category::RepeatedSearch).basis,
            by(Category::RepeatedSearch).label
        ),
        (Basis::Heuristic, Label::Hypothesis)
    );
    assert_eq!(
        (
            by(Category::RepeatedFailure).basis,
            by(Category::RepeatedFailure).label
        ),
        (Basis::Evidence, Label::Recommendation)
    );
    assert_eq!(by(Category::UnusedDelegation).basis, Basis::Evidence);
    assert_eq!(by(Category::UnusedDelegation).label, Label::Hypothesis);
    assert_eq!(by(Category::SerializableDelegation).basis, Basis::Heuristic);
}

#[test]
fn a_clean_lean_run_gets_no_suggestions() {
    let events = trace(
        8,
        &Shape {
            greps: 1,
            reads: 2,
            ..Shape::base()
        },
    );
    assert!(suggest(&result_for(&events, true), &events).is_empty());
}

#[test]
fn a_suggestion_from_a_run_that_failed_is_not_safe_to_recommend() {
    let events = trace(
        9,
        &Shape {
            greps: 4,
            ..Shape::base()
        },
    );
    let failed = suggest(&result_for(&events, false), &events);
    assert!(!failed.is_empty() && failed.iter().all(|s| !s.safe_to_recommend));
    let denied_events = trace(
        10,
        &Shape {
            greps: 4,
            denied: 1,
            ..Shape::base()
        },
    );
    let denied = suggest(&result_for(&denied_events, true), &denied_events);
    assert!(!denied.is_empty() && denied.iter().all(|s| !s.safe_to_recommend));
}

#[test]
fn parallel_children_are_not_called_serializable() {
    let mut b = Builder::new(50);
    b.add(EventKind::AttemptStarted, None, None, json!({}));
    b.add(
        EventKind::AgentStarted,
        Some("lead"),
        None,
        json!({ "role": "lead" }),
    );
    // Both children start before either stops.
    for n in 0..2 {
        let tu = format!("tu{n}");
        b.add(EventKind::TaskDelegated, Some("lead"), Some(&tu), json!({}));
        let id = b.add(
            EventKind::AgentStarted,
            Some(&format!("kid{n}")),
            Some(&tu),
            json!({ "subagent_type": "reader" }),
        );
        let e = b.evs.iter_mut().find(|e| e.event_id == id).unwrap();
        e.parent_agent_id = Some("lead".into());
        e.parent_tool_use_id = Some(tu.clone());
        b.tool(&format!("kid{n}"), "Read", &format!("t{n}"), None);
    }
    for n in 0..2 {
        b.add(
            EventKind::AgentStopped,
            Some(&format!("kid{n}")),
            None,
            json!({ "status": "stopped" }),
        );
    }
    b.add(
        EventKind::AttemptCompleted,
        None,
        None,
        json!({ "status": "completed" }),
    );
    let found = suggest(&result_for(&b.evs, true), &b.evs);
    assert!(
        found
            .iter()
            .all(|s| s.category != Category::SerializableDelegation),
        "{found:?}"
    );
}

// ---- round 1 review of #398 ----

#[test]
fn every_record_of_a_command_counts_so_a_later_failure_is_not_hidden() {
    use CheckOutcome::{Fail, NotRun, Pass};
    let su = suite();
    let name = su.cases[0].test_commands[0].clone();
    for (records, want) in [
        ([Pass, Fail], Verdict::Fail),
        ([Fail, Pass], Verdict::Fail),
        ([Pass, NotRun], Verdict::Incomplete),
        ([Pass, Pass], Verdict::Pass),
    ] {
        let mut r = run(&su, 0, "v", 1, &Shape::base(), true);
        r.checks.retain(|c| c.kind != CheckKind::UnitTests);
        r.checks.extend(records.map(|outcome| CheckResult {
            kind: CheckKind::UnitTests,
            name: name.clone(),
            outcome,
            evidence: vec![],
        }));
        let got = evaluate(&su, &r).unwrap();
        assert_eq!(got.verdict, want, "{records:?}");
        assert_eq!(got.checks.iter().filter(|c| c.name == name).count(), 1);
    }
}

#[test]
fn an_unknown_cost_is_not_inside_a_cost_envelope() {
    let su = suite();
    assert!(su.cases[0].envelope.max_cost_usd.is_some());
    let no_cost = Shape {
        cost: None,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &no_cost, true)).unwrap();
    assert_eq!(r.verdict, Verdict::Incomplete);
    assert!(r
        .checks
        .iter()
        .any(|c| c.name == "within_envelope" && c.outcome == CheckOutcome::NotRun));
}

#[test]
fn a_tool_the_case_does_not_allow_fails_the_case_and_the_check_is_always_in_the_result() {
    let su = suite();
    assert_eq!(su.cases[0].allowed_side_effects, ["Edit", "Write"]);
    let side = |r: &EvaluationResult| {
        r.checks
            .iter()
            .find(|c| c.name == "allowed_side_effects")
            .cloned()
    };
    let clean = evaluate(&su, &run(&su, 0, "v", 1, &Shape::base(), true)).unwrap();
    let c = side(&clean).expect("the side-effect check is listed");
    assert_eq!(
        (c.kind, c.outcome),
        (CheckKind::PolicyInvariant, CheckOutcome::Pass)
    );

    let bash = Shape {
        bash: 1,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &bash, true)).unwrap();
    assert_eq!(r.verdict, Verdict::Fail);
    let c = side(&r).unwrap();
    assert_eq!(c.outcome, CheckOutcome::Fail);
    assert_eq!(c.evidence.len(), 1, "the Bash call is the evidence");

    // A call the supervisor denied did not happen, so it is not a side effect.
    let denied = Shape {
        denied: 2,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &denied, true)).unwrap();
    assert_eq!(side(&r).unwrap().outcome, CheckOutcome::Pass);
    assert_eq!(r.verdict, Verdict::Pass);
}

#[test]
fn network_tools_are_side_effects_and_a_denied_id_that_completed_is_not_hidden() {
    let su = suite();
    let side = |r: &EvaluationResult| {
        r.checks
            .iter()
            .find(|c| c.name == "allowed_side_effects")
            .cloned()
            .unwrap()
    };
    // WebFetch and WebSearch reach the network, and the case allows only Edit and Write.
    let web = Shape {
        web: 1,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &web, true)).unwrap();
    assert_eq!(side(&r).outcome, CheckOutcome::Fail);
    assert_eq!(side(&r).evidence.len(), 2, "one WebFetch and one WebSearch");
    assert_eq!(r.verdict, Verdict::Fail);

    // A denial does not hide a call under the same id that then completed.
    let ran = Shape {
        denied_then_ran: 1,
        ..Shape::base()
    };
    let r = evaluate(&su, &run(&su, 0, "v", 1, &ran, true)).unwrap();
    assert_eq!(side(&r).outcome, CheckOutcome::Fail);
    assert_eq!(r.verdict, Verdict::Fail);
}

#[test]
fn the_result_records_the_privacy_policy_it_was_made_under() {
    let su = suite();
    let r = evaluate(&su, &run(&su, 0, "v", 1, &Shape::base(), true)).unwrap();
    assert_eq!(
        serde_json::to_value(&r).unwrap()["privacy"],
        json!("metadata_only")
    );
}

#[test]
fn a_dot_case_id_and_an_unnamed_human_reviewer_are_refused() {
    for id in ["..", ".", ".hidden"] {
        let mut s = suite();
        s.cases[0].case_id = id.into();
        assert!(s.validate().is_err(), "{id}");
    }
    let su = suite();
    let mut r = run(&su, 0, "v", 1, &Shape::base(), true);
    r.assessments.push(Assessment {
        assessor: Assessor::Human {
            reviewer: " ".into(),
        },
        rubric_item: su.cases[0].rubric[0].clone(),
        score: 1.0,
        max_score: 2.0,
    });
    assert!(matches!(
        evaluate(&su, &r),
        Err(EvalError::BadAssessment(_))
    ));
}

#[test]
fn a_manifest_that_is_not_a_plan_is_refused_by_compare() {
    let (_, manifest, results) = experiment(1, |c| (lean(c), true));
    for break_it in [
        (|m: &mut aibus_trace::experiment::ComparisonManifest| m.repetitions = 0) as fn(&mut _),
        |m| m.cases.clear(),
        |m| m.manifest_version = 99,
        |m| {
            m.variants.truncate(1);
        },
    ] {
        let mut m = manifest.clone();
        break_it(&mut m);
        assert!(compare(&m, &results).is_err());
    }
}
