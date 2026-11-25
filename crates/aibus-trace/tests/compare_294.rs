//! #294: comparing two recorded runs, the causal inspector view model and the reproducibility
//! report. The traces are built in the test (and the spike recordings are read for the no-panic
//! sweep); nothing here starts a process or reaches a provider.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aibus_trace::causal::{inspector, render_reproducibility, reproducibility, NodeKind};
use aibus_trace::compare::{compare_runs, render, side, Comparable, EnvVerdict, Known, RunInput};
use aibus_trace::eval::{
    Assessor, CheckKind, CheckOutcome, CheckResult, EvaluationResult, PrivacyPolicy, RunContext,
    Verdict, RESULT_VERSION,
};
use aibus_trace::view::Recorded;
use aibus_trace::{AttemptRef, DataQuality, EventIds, EventKind, Origin, TraceEvent, Visibility};
use serde_json::{json, Value};
use uuid::Uuid;

/// What one synthetic run is made of.
#[derive(Clone)]
struct Shape {
    cli_version: &'static str,
    model: &'static str,
    tools: Vec<(&'static str, &'static str)>,
    cost: Option<f64>,
    tokens: Option<f64>,
    wall_ms: u64,
    route: Option<Value>,
    plan: Option<&'static str>,
    artifact: bool,
    gap: bool,
    /// Keep the prompt of the delegation and the message text as payload objects.
    retain: bool,
    /// The sizes of the message chunks.
    chunks: Vec<u64>,
    /// A second attempt of the run, by this provider, that reports nothing at all.
    extra_attempt: Option<&'static str>,
    /// Control, bidi and C1 characters in the unit and source of every usage metric.
    hostile_usage: bool,
}

impl Shape {
    fn base() -> Self {
        Self {
            cli_version: "2.1.0",
            model: "model-a",
            tools: vec![("Read", "t1"), ("Grep", "t2"), ("Read", "t3")],
            cost: Some(0.5),
            tokens: Some(1000.0),
            wall_ms: 3_000,
            route: None,
            plan: None,
            artifact: false,
            gap: false,
            retain: false,
            chunks: vec![42],
            extra_attempt: None,
            hostile_usage: false,
        }
    }
}

const PAYLOAD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REPLY: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

struct Built {
    dir: tempfile::TempDir,
    rec: Recorded,
}

fn build(seed: u128, s: &Shape) -> Built {
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(seed),
        attempt_id: Uuid::from_u128(seed + 1),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    };
    let mut ids = EventIds::default();
    let mut n = 0u64;
    let mut evs: Vec<TraceEvent> = Vec::new();
    let mut add = |kind: EventKind,
                   agent: Option<&str>,
                   tool: Option<&str>,
                   at_ms: u64,
                   data: Value,
                   f: &dyn Fn(&mut TraceEvent)| {
        n += 1;
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_000 + at_ms);
        let mut e = TraceEvent::new(
            &attempt,
            ids.next(at, &[&n.to_le_bytes(), &seed.to_le_bytes()]),
            kind,
            Origin::CliStream,
            Visibility::Observed,
            at,
            "test/1",
        );
        e.agent_id = agent.map(str::to_owned);
        e.native.tool_use_id = tool.map(str::to_owned);
        e.native.session_id = agent.map(|a| format!("session-{a}"));
        e.native.native_type = Some("assistant".into());
        e.data = data;
        f(&mut e);
        evs.push(e);
    };
    let none = |_: &mut TraceEvent| {};
    add(
        EventKind::AttemptStarted,
        None,
        None,
        0,
        json!({ "cli_version": s.cli_version }),
        &none,
    );
    add(
        EventKind::AgentStarted,
        Some("lead"),
        None,
        10,
        json!({ "role": "lead", "model": s.model, "cli_version": s.cli_version, "permission_mode": "default" }),
        &none,
    );
    if let Some(r) = &s.route {
        add(EventKind::RouteDecided, None, None, 20, r.clone(), &none);
    }
    // The lead delegates to a child through toolu-kid; the child's prompt is the delegation payload.
    add(
        EventKind::TaskDelegated,
        Some("lead"),
        Some("toolu-kid"),
        30,
        json!({ "subagent_type": "reader", "tool": "Task" }),
        &|e| {
            if s.retain {
                e.payload_ref = Some(format!("local://objects/{PAYLOAD}.json"));
            }
        },
    );
    add(
        EventKind::AgentStarted,
        Some("kid"),
        None,
        40,
        json!({ "subagent_type": "reader" }),
        &|e| {
            e.parent_agent_id = Some("lead".into());
            e.parent_tool_use_id = Some("toolu-kid".into());
        },
    );
    for (i, (name, id)) in s.tools.iter().enumerate() {
        add(
            EventKind::ToolStarted,
            Some("lead"),
            Some(id),
            100 + i as u64,
            json!({ "tool": name }),
            &none,
        );
        add(
            EventKind::ToolCompleted,
            Some("lead"),
            Some(id),
            110 + i as u64,
            json!({ "bytes": 10 }),
            &none,
        );
    }
    for (i, bytes) in s.chunks.iter().enumerate() {
        add(
            EventKind::MessageChunk,
            Some("lead"),
            None,
            200 + i as u64,
            json!({ "bytes": bytes, "role": "assistant" }),
            &|e| {
                if s.retain && i == 0 {
                    e.payload_ref = Some(format!("local://objects/{REPLY}.json"));
                }
            },
        );
    }
    if s.artifact {
        add(
            EventKind::ArtifactCreated,
            Some("lead"),
            None,
            210,
            json!({ "name": "report.md" }),
            &none,
        );
    }
    if s.gap {
        add(
            EventKind::GapDetected,
            Some("kid"),
            None,
            220,
            json!({ "reason": "teams_not_visible" }),
            &none,
        );
    }
    let (unit_tail, source) = if s.hostile_usage {
        ("\u{9b}x", "result\u{202e}.usage")
    } else {
        ("", "result.usage")
    };
    let metric = |name: &str, v: Option<f64>, unit: &str, q: &str| json!({ "name": name, "value": v, "unit": format!("{unit}{unit_tail}"), "quality": q, "source": source });
    let mut metrics = Vec::new();
    if let Some(t) = s.tokens {
        metrics.push(metric("input_tokens", Some(t), "tokens", "reported"));
    }
    if let Some(c) = s.cost {
        metrics.push(metric("cost_usd", Some(c), "usd", "estimated"));
    }
    add(
        EventKind::UsageReported,
        Some("lead"),
        None,
        s.wall_ms - 1,
        json!({ "scope": "attempt", "metrics": metrics.clone() }),
        &none,
    );
    add(
        EventKind::UsageReported,
        Some("lead"),
        None,
        s.wall_ms - 2,
        json!({ "scope": "agent", "metrics": metrics }),
        &none,
    );
    add(
        EventKind::AttemptCompleted,
        None,
        None,
        s.wall_ms,
        json!({ "status": "completed" }),
        &none,
    );

    if let Some(provider) = s.extra_attempt {
        let second = AttemptRef {
            workflow_run_id: Uuid::from_u128(seed),
            attempt_id: Uuid::from_u128(seed + 2),
            parent_attempt_id: None,
            provider_extension: provider.into(),
        };
        for (k, kind) in [EventKind::AttemptStarted, EventKind::AttemptCompleted]
            .into_iter()
            .enumerate()
        {
            n += 1;
            let at = SystemTime::UNIX_EPOCH
                + Duration::from_millis(1_800_000_000_000 + 5_000 + k as u64);
            let id = ids.next(at, &[&n.to_le_bytes(), &seed.to_le_bytes()]);
            let mut e = TraceEvent::new(
                &second,
                id,
                kind,
                Origin::CliStream,
                Visibility::Observed,
                at,
                "test/1",
            );
            e.data = json!({ "status": "completed" });
            evs.push(e);
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let mut body = Vec::new();
    for e in &evs {
        body.extend_from_slice(&serde_json::to_vec(e).unwrap());
        body.push(b'\n');
    }
    fs::write(dir.path().join("events.jsonl"), body).unwrap();
    if s.retain {
        fs::create_dir(dir.path().join("objects")).unwrap();
        fs::write(
            dir.path().join("objects").join(format!("{PAYLOAD}.json")),
            json!({ "prompt": "read the docs and list the flags" }).to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("objects").join(format!("{REPLY}.json")),
            json!({ "text": "here are the flags" }).to_string(),
        )
        .unwrap();
    }
    if let Some(p) = s.plan {
        fs::write(dir.path().join("plan.json"), p).unwrap();
    }
    let rec = Recorded::load(dir.path()).unwrap();
    Built { dir, rec }
}

fn input<'a>(id: &'a str, b: &'a Built) -> RunInput<'a> {
    RunInput {
        id,
        rec: &b.rec,
        result: None,
    }
}

fn result(
    variant: &str,
    pass: usize,
    fail: usize,
    not_run: usize,
    revision: &str,
) -> EvaluationResult {
    let mut checks = Vec::new();
    for (n, outcome) in std::iter::repeat_n(CheckOutcome::Pass, pass)
        .chain(std::iter::repeat_n(CheckOutcome::Fail, fail))
        .chain(std::iter::repeat_n(CheckOutcome::NotRun, not_run))
        .enumerate()
    {
        checks.push(CheckResult {
            kind: CheckKind::UnitTests,
            name: format!("cargo test case{n}"),
            outcome,
            evidence: vec![],
        });
    }
    EvaluationResult {
        result_version: RESULT_VERSION,
        suite: "suite".into(),
        suite_version: 1,
        case_id: "case-1".into(),
        variant: variant.into(),
        workflow_run_ids: vec![],
        context: RunContext {
            repository_revision: revision.into(),
            extension_version: "1.0.0".into(),
            cli_version: Some("2.1.0".into()),
            concurrent_runs: 1,
            cache_note: None,
        },
        assessor: Assessor::Deterministic {
            tool: "cargo".into(),
            version: "1".into(),
        },
        verdict: if fail == 0 && not_run == 0 {
            Verdict::Pass
        } else {
            Verdict::Fail
        },
        checks,
        assessments: vec![],
        metrics: vec![],
        privacy: PrivacyPolicy::MetadataOnly,
    }
}

/// `r` as the result of the run built from `seed`.
fn bound(mut r: EvaluationResult, seed: u128) -> EvaluationResult {
    r.workflow_run_ids = vec![Uuid::from_u128(seed)];
    r
}

fn traces() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284")
}

fn env_row<'a>(
    c: &'a aibus_trace::compare::Comparison,
    name: &str,
) -> &'a aibus_trace::compare::EnvRow {
    c.environment
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("{name}: {:?}", c.environment))
}

#[test]
fn one_comparison_answers_what_ran_why_routed_what_each_made_what_changed_and_who_passed_more() {
    let mut sa = Shape::base();
    sa.route = Some(json!({ "chosen": "claude-code", "reason_code": "cheapest_capable" }));
    sa.artifact = true;
    sa.retain = true;
    let mut sb = Shape::base();
    sb.tools.push(("Bash", "t4"));
    let (a, b) = (build(10, &sa), build(20, &sb));
    let ra = bound(result("a", 3, 1, 0, "abc123"), 10);
    let rb = bound(result("b", 4, 0, 0, "abc123"), 20);
    let c = compare_runs(
        &RunInput {
            id: "run-a",
            rec: &a.rec,
            result: Some(&ra),
        },
        &RunInput {
            id: "run-b",
            rec: &b.rec,
            result: Some(&rb),
        },
    );
    let t = render(&c);
    for section in [
        "what ran",
        "routing",
        "outputs",
        "git and worktree",
        "quality",
        "tools",
        "environment",
    ] {
        assert!(
            t.contains(&format!("\n{section}\n")),
            "no section {section}:\n{t}"
        );
    }
    // What ran: both runs by id, and the tool counts that differ.
    assert!(t.contains("run-a") && t.contains("run-b"), "{t}");
    assert_eq!((c.a.tools.total, c.b.tools.total), (3, 4), "{t}");
    assert_eq!(c.a.tools.by_name["Read"], 2);
    assert_eq!(c.b.tools.by_name["Bash"], 1);
    // Why routed: run a recorded a decision, run b did not, and says so.
    match (&c.a.routing, &c.b.routing) {
        (Known::Value(rows), Known::Gap(why)) => {
            assert_eq!(rows[0].fields["reason_code"], "cheapest_capable");
            assert!(why.contains("no route_decided"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    // What each produced: message sizes, the artifact, the retained payloads.
    assert_eq!(c.a.outputs.message_chunks, 1);
    assert_eq!(c.a.outputs.message_bytes, Known::Value(42));
    assert_eq!(c.a.outputs.artifacts[0].name.as_deref(), Some("report.md"));
    assert!(c.a.outputs.payloads_retained > 0 && c.b.outputs.payloads_retained == 0);
    // Which variant passed more tests.
    assert!(t.contains("3 passed, 1 failed, 0 not run"), "{t}");
    assert!(t.contains("4 passed, 0 failed, 0 not run"), "{t}");
    // What changed: the revision is known from the results, the diff is not in any trace.
    assert_eq!(c.a.git_revision, Known::Value("abc123".into()));
    match &c.a.worktree_diff {
        Known::Gap(why) => assert!(
            why.contains("no git") || why.contains("not recorded"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unknown_cost_stays_unknown_in_the_text_and_in_the_json() {
    let mut sb = Shape::base();
    sb.cost = None;
    let (a, b) = (build(10, &Shape::base()), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    let cost = c
        .deltas
        .iter()
        .find(|d| d.metric == "cost_usd")
        .expect("a cost row is always there");
    assert_eq!(cost.a.quality, DataQuality::Estimated);
    assert_eq!(cost.b.quality, DataQuality::Unknown);
    assert_eq!(cost.b.value, None);
    assert!(
        matches!(&cost.change, Known::Gap(why) if why.contains("unknown")),
        "{:?}",
        cost.change
    );
    let t = render(&c);
    assert!(
        t.lines()
            .any(|l| l.contains("cost_usd") && l.contains("unknown")),
        "{t}"
    );
    let j = serde_json::to_value(&c).unwrap();
    let row = j["deltas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["metric"] == "cost_usd")
        .unwrap();
    assert_eq!(row["b"]["quality"], "unknown");
    assert!(row["change"]["gap"].is_string(), "{row}");
}

#[test]
fn a_change_is_computed_only_when_both_numbers_are_known_in_the_same_unit() {
    let mut sb = Shape::base();
    sb.wall_ms = 5_000;
    sb.cost = Some(0.75);
    let (a, b) = (build(10, &Shape::base()), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    let d = |name: &str| {
        c.deltas
            .iter()
            .find(|d| d.metric == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(d("cost_usd").change, Known::Value(0.25));
    assert_eq!(d("latency_ms").a.quality, DataQuality::Derived);
    assert_eq!(d("latency_ms").change, Known::Value(2_000.0));
    // Reported by neither side's trace under the same name: shown with unknown on the other side.
    assert!(matches!(d("input_tokens").change, Known::Value(v) if v == 0.0));
}

#[test]
fn runs_of_different_cli_versions_or_models_are_flagged_not_comparable() {
    let mut sb = Shape::base();
    sb.cli_version = "2.2.0";
    sb.model = "model-b";
    let (a, b) = (build(10, &Shape::base()), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    assert_eq!(env_row(&c, "cli_version").verdict, EnvVerdict::Differs);
    assert_eq!(env_row(&c, "model").verdict, EnvVerdict::Differs);
    match &c.comparable {
        Comparable::No(why) => {
            assert!(why.iter().any(|w| w.contains("cli_version")), "{why:?}");
            assert!(why.iter().any(|w| w.contains("model")), "{why:?}");
        }
        other => panic!("{other:?}"),
    }
    let t = render(&c);
    assert!(t.contains("not comparable"), "{t}");
    assert!(t.contains("2.1.0") && t.contains("2.2.0"), "{t}");
}

#[test]
fn a_different_workflow_version_is_flagged_by_its_plan_hash() {
    let mut sa = Shape::base();
    sa.plan = Some(r#"{"plan_hash":"sha256:one","steps":[]}"#);
    let mut sb = Shape::base();
    sb.plan = Some(r#"{"plan_hash":"sha256:two","steps":[]}"#);
    let (a, b) = (build(10, &sa), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    assert_eq!(env_row(&c, "workflow_plan").verdict, EnvVerdict::Differs);
    assert!(
        matches!(&c.comparable, Comparable::No(w) if w.iter().any(|x| x.contains("workflow_plan"))),
        "{:?}",
        c.comparable
    );
}

#[test]
fn runs_are_never_called_comparable_while_something_about_them_is_unknown() {
    // Same CLI, model and provider, but neither run has a plan or a repository revision.
    let (a, b) = (build(10, &Shape::base()), build(20, &Shape::base()));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    match &c.comparable {
        Comparable::Unknown(fields) => {
            assert!(
                fields.iter().any(|f| f.contains("workflow_plan")),
                "{fields:?}"
            );
            assert!(
                fields.iter().any(|f| f.contains("repository_revision")),
                "{fields:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        env_row(&c, "repository_revision").verdict,
        EnvVerdict::Unknown
    );
    assert!(
        render(&c).contains("comparability unknown"),
        "{}",
        render(&c)
    );

    // With the same plan and results from the same revision on both, they are comparable.
    let mut s = Shape::base();
    s.plan = Some(r#"{"plan_hash":"sha256:same","steps":[]}"#);
    let (a, b) = (build(10, &s), build(20, &s));
    let (ra, rb) = (
        bound(result("a", 1, 0, 0, "abc"), 10),
        bound(result("b", 1, 0, 0, "abc"), 20),
    );
    let c = compare_runs(
        &RunInput {
            id: "a",
            rec: &a.rec,
            result: Some(&ra),
        },
        &RunInput {
            id: "b",
            rec: &b.rec,
            result: Some(&rb),
        },
    );
    assert_eq!(c.comparable, Comparable::Yes, "{:?}", c.environment);
    // A different revision breaks it again.
    let rc = bound(result("b", 1, 0, 0, "def"), 20);
    let c = compare_runs(
        &RunInput {
            id: "a",
            rec: &a.rec,
            result: Some(&ra),
        },
        &RunInput {
            id: "b",
            rec: &b.rec,
            result: Some(&rc),
        },
    );
    assert!(
        matches!(&c.comparable, Comparable::No(w) if w.iter().any(|x| x.contains("repository_revision")))
    );
}

#[test]
fn quality_is_a_gap_without_a_result_and_not_run_checks_never_count_as_passes() {
    let (a, b) = (build(10, &Shape::base()), build(20, &Shape::base()));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    assert!(
        matches!(&c.a.quality, Known::Gap(why) if why.contains("no evaluation result")),
        "{:?}",
        c.a.quality
    );
    assert!(
        render(&c).contains("no evaluation result"),
        "{}",
        render(&c)
    );
    let ra = bound(result("a", 2, 0, 2, "abc"), 10);
    match side(&RunInput {
        id: "a",
        rec: &a.rec,
        result: Some(&ra),
    })
    .quality
    {
        Known::Value(q) => assert_eq!(
            (q.checks_passed, q.checks_failed, q.checks_not_run),
            (2, 0, 2)
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn tool_counts_split_by_outcome_and_name() {
    let mut s = Shape::base();
    s.tools = vec![("Read", "t1"), ("Read", "t2")];
    let b = build(10, &s);
    let t = side(&input("a", &b)).tools;
    assert_eq!((t.total, t.completed, t.failed, t.unknown), (2, 2, 0, 0));
    assert_eq!(t.by_name["Read"], 2);
}

#[test]
fn a_hostile_trace_cannot_put_an_escape_sequence_in_the_comparison() {
    let mut s = Shape::base();
    s.tools = vec![("Re\u{1b}[2Jad", "t1")];
    s.route = Some(json!({ "chosen": "x\u{1b}]0;owned\u{7}y", "reason_code": "r\u{202e}l" }));
    s.plan = Some(r#"{"plan_hash":"sha256:x"}"#);
    let (a, b) = (build(10, &s), build(20, &Shape::base()));
    let t = render(&compare_runs(&input("a\u{1b}[31m", &a), &input("b", &b)));
    assert!(
        !t.contains('\u{1b}') && !t.contains('\u{7}') && !t.contains('\u{202e}'),
        "{t:?}"
    );
    assert!(t.contains("\\u{1b}"), "{t}");
}

#[test]
fn every_pair_of_the_spike_recordings_compares_without_a_panic_and_serialises() {
    let mut names: Vec<String> = fs::read_dir(traces())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    let recs: Vec<(String, Recorded)> = names
        .iter()
        .map(|n| (n.clone(), Recorded::load(&traces().join(n)).unwrap()))
        .collect();
    for (na, ra) in &recs {
        for (nb, rb) in &recs {
            let c = compare_runs(
                &RunInput {
                    id: na,
                    rec: ra,
                    result: None,
                },
                &RunInput {
                    id: nb,
                    rec: rb,
                    result: None,
                },
            );
            let t = render(&c);
            assert!(!t.contains('\u{1b}'), "{na} {nb}");
            serde_json::to_string(&c).unwrap();
            // Nothing in a recording carries a git revision or diff.
            assert!(matches!(c.a.worktree_diff, Known::Gap(_)));
            assert!(!matches!(c.comparable, Comparable::Yes), "{na} {nb}");
        }
    }
}

#[test]
fn the_inspector_of_an_agent_lists_ids_evidence_gaps_usage_and_artifacts() {
    let mut s = Shape::base();
    s.artifact = true;
    s.gap = true;
    let b = build(10, &s);
    let v = inspector(&b.rec, "lead").unwrap();
    assert_eq!(v.kind, NodeKind::Agent);
    assert_eq!(v.attempt_id, Uuid::from_u128(11));
    assert!(
        v.native_ids
            .iter()
            .any(|n| n.name == "session_id" && n.value == "session-lead"),
        "{:?}",
        v.native_ids
    );
    assert!(!v.source_evidence.is_empty());
    assert!(v
        .source_evidence
        .iter()
        .all(|e| e.parser == "test/1" && e.position > 0));
    assert!(
        v.link.clone() == Known::Value("none: the lead is the process aibus started".into()),
        "{:?}",
        v.link
    );
    match &v.artifacts {
        Known::Value(a) => assert_eq!(a[0].name.as_deref(), Some("report.md")),
        other => panic!("{other:?}"),
    }
    assert_eq!(v.elapsed.quality, DataQuality::Derived);
    assert!(v.elapsed.value.unwrap() > 0.0);
    match &v.usage {
        Known::Value(ms) => assert!(ms
            .iter()
            .any(|m| m.name == "cost_usd" && m.quality == DataQuality::Estimated)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(&v.worktree_diff, Known::Gap(_)));
    // The child has the gap the trace recorded, and a delegation link with its evidence level.
    let kid = inspector(&b.rec, "kid").unwrap();
    assert_eq!(kid.gaps.len(), 1);
    assert_eq!(kid.gaps[0].reason, "teams_not_visible");
    assert!(
        matches!(&kid.link, Known::Value(l) if l.contains("lead") && l.contains("observed")),
        "{:?}",
        kid.link
    );
    assert!(
        matches!(&kid.usage, Known::Gap(_)),
        "no usage was reported for the child"
    );
    assert!(matches!(&kid.artifacts, Known::Gap(_)));
}

#[test]
fn a_prompt_and_an_output_are_shown_only_when_the_run_kept_them() {
    let (plain, kept) = (build(10, &Shape::base()), {
        let mut s = Shape::base();
        s.retain = true;
        build(20, &s)
    });
    let kid = inspector(&plain.rec, "kid").unwrap();
    assert!(
        matches!(&kid.prompt, Known::Gap(why) if why.contains("not retained")),
        "{:?}",
        kid.prompt
    );
    let lead = inspector(&plain.rec, "lead").unwrap();
    assert!(
        matches!(&lead.output, Known::Gap(why) if why.contains("1 message")),
        "{:?}",
        lead.output
    );

    let kid = inspector(&kept.rec, "kid").unwrap();
    match &kid.prompt {
        Known::Value(r) => {
            assert!(r.text.contains("read the docs and list the flags"), "{r:?}");
            assert!(r.from_event > 0);
        }
        other => panic!("{other:?}"),
    }
    let lead = inspector(&kept.rec, "lead").unwrap();
    assert!(
        matches!(&lead.output, Known::Value(r) if r.text.contains("here are the flags")),
        "{:?}",
        lead.output
    );
    // A payload outside the run's own directory is never opened.
    let _ = &kept.dir;
}

#[test]
fn the_inspector_of_a_tool_use_and_an_attempt_and_of_nothing() {
    let b = build(10, &Shape::base());
    let t = inspector(&b.rec, "t1").unwrap();
    assert_eq!(t.kind, NodeKind::Tool);
    assert!(t
        .native_ids
        .iter()
        .any(|n| n.name == "tool_use_id" && n.value == "t1"));
    assert!(
        matches!(&t.usage, Known::Gap(why) if why.contains("per tool")),
        "{:?}",
        t.usage
    );
    let a = inspector(&b.rec, &Uuid::from_u128(11).to_string()).unwrap();
    assert_eq!(a.kind, NodeKind::Attempt);
    assert!(matches!(&a.output, Known::Gap(_)));
    assert!(inspector(&b.rec, "nobody").is_none());
    serde_json::to_string(&t).unwrap();
}

#[test]
fn reproducibility_separates_replay_from_rerun_and_lists_what_the_trace_does_not_carry() {
    let b = build(10, &Shape::base());
    let r = reproducibility("run-a", &b.rec);
    assert!(r.replay.offline && r.replay.available);
    assert_eq!(r.replay.events, b.rec.events.len());
    assert!(
        r.replay.caveats.iter().any(|c| c.contains("payload")),
        "{:?}",
        r.replay.caveats
    );
    assert_eq!(r.rerun.label, "rerun");
    assert!(!r.rerun.available && r.rerun.outcome_may_differ);
    assert!(r.rerun.blocked_by.as_deref().unwrap().contains("#288"));
    let need = |n: &str| {
        r.rerun
            .needs
            .iter()
            .find(|x| x.name == n)
            .unwrap_or_else(|| panic!("{n}"))
            .value
            .clone()
    };
    assert_eq!(need("cli_version"), Known::Value("2.1.0".into()));
    assert_eq!(need("model"), Known::Value("model-a".into()));
    for gap in [
        "repository_revision",
        "workflow_plan",
        "prompts",
        "environment",
    ] {
        assert!(matches!(need(gap), Known::Gap(_)), "{gap}");
    }
    let t = render_reproducibility(&r);
    let (replay, rerun) = (
        t.find("\nreplay\n").expect(&t),
        t.find("\nrerun\n").expect(&t),
    );
    assert!(replay < rerun);
    assert!(
        t.contains("offline") && t.contains("may differ") && t.contains("not a replay"),
        "{t}"
    );
}

#[test]
fn a_damaged_trace_says_so_in_the_replay_caveats() {
    let b = build(10, &Shape::base());
    let mut raw = fs::read(b.dir.path().join("events.jsonl")).unwrap();
    raw.extend_from_slice(b"not json\n{\"torn");
    fs::write(b.dir.path().join("events.jsonl"), raw).unwrap();
    let rec = Recorded::load(b.dir.path()).unwrap();
    let r = reproducibility("x", &rec);
    assert!(
        r.replay.caveats.iter().any(|c| c.contains("1 corrupt")),
        "{:?}",
        r.replay.caveats
    );
    assert!(
        r.replay.caveats.iter().any(|c| c.contains("torn")),
        "{:?}",
        r.replay.caveats
    );
    let empty = tempfile::tempdir().unwrap();
    fs::write(empty.path().join("events.jsonl"), b"").unwrap();
    let r = reproducibility("e", &Recorded::load(empty.path()).unwrap());
    assert!(!r.replay.available);
}

// ---- round 1 review of #401 ----

/// Both runs with the same plan and a result each from the same revision: comparable.
fn yes_pair() -> (Built, Built, EvaluationResult, EvaluationResult) {
    let mut s = Shape::base();
    s.plan = Some(r#"{"plan_hash":"sha256:same","steps":[]}"#);
    (
        build(10, &s),
        build(20, &s),
        bound(result("a", 1, 0, 0, "abc"), 10),
        bound(result("b", 1, 0, 0, "abc"), 20),
    )
}

fn with<'a>(
    a: (&'a Built, &'a EvaluationResult),
    b: (&'a Built, &'a EvaluationResult),
) -> aibus_trace::compare::Comparison {
    compare_runs(
        &RunInput {
            id: "a",
            rec: &a.0.rec,
            result: Some(a.1),
        },
        &RunInput {
            id: "b",
            rec: &b.0.rec,
            result: Some(b.1),
        },
    )
}

#[test]
fn a_result_of_another_run_or_of_another_version_is_never_taken_for_the_run() {
    let (a, b, ra, rb) = yes_pair();
    assert_eq!(
        with((&a, &ra), (&b, &rb)).comparable,
        Comparable::Yes,
        "the bound pair is the baseline"
    );
    // A result whose workflow_run_ids name some other run.
    let foreign = bound(result("b", 1, 0, 0, "abc"), 999);
    let c = with((&a, &ra), (&b, &foreign));
    assert!(
        !matches!(c.comparable, Comparable::Yes),
        "{:?}",
        c.comparable
    );
    assert!(
        matches!(&c.b.quality, Known::Gap(why) if why.contains("workflow_run_ids")),
        "{:?}",
        c.b.quality
    );
    assert!(
        matches!(&c.b.git_revision, Known::Gap(_)),
        "a foreign result must not supply the revision: {:?}",
        c.b.git_revision
    );
    // A result of a version this build does not read.
    let mut old = bound(result("b", 1, 0, 0, "abc"), 20);
    old.result_version = RESULT_VERSION + 98;
    let c = with((&a, &ra), (&b, &old));
    assert!(
        !matches!(c.comparable, Comparable::Yes),
        "{:?}",
        c.comparable
    );
    assert!(
        matches!(&c.b.quality, Known::Gap(why) if why.contains("result_version")),
        "{:?}",
        c.b.quality
    );
    // A result that names no run at all is not bound to this one either.
    let mut unbound = result("b", 1, 0, 0, "abc");
    unbound.workflow_run_ids = vec![];
    let c = with((&a, &ra), (&b, &unbound));
    assert!(
        !matches!(c.comparable, Comparable::Yes),
        "{:?}",
        c.comparable
    );
}

#[test]
fn results_of_different_cases_make_the_runs_not_comparable() {
    let (a, b, ra, mut rb) = yes_pair();
    rb.case_id = "case-2".into();
    let c = with((&a, &ra), (&b, &rb));
    match &c.comparable {
        Comparable::No(why) => assert!(why.iter().any(|w| w.contains("case_id")), "{why:?}"),
        other => panic!("{other:?}"),
    }
    let (a, b, ra, mut rb) = yes_pair();
    rb.suite_version = 2;
    match &with((&a, &ra), (&b, &rb)).comparable {
        Comparable::No(why) => assert!(why.iter().any(|w| w.contains("suite_version")), "{why:?}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_attempt_that_reported_no_usage_makes_the_run_total_unknown() {
    let mut s = Shape::base();
    s.extra_attempt = Some("org.aibus.test");
    let (a, b) = (build(10, &s), build(20, &Shape::base()));
    let sa = side(&input("a", &a));
    assert_eq!(sa.attempts, 2);
    for name in ["cost_usd", "input_tokens"] {
        let m = sa.usage.iter().find(|m| m.name == name).unwrap();
        assert_eq!(
            m.value, None,
            "{name}: one attempt's number is not the run's"
        );
        assert_eq!(m.quality, DataQuality::Unknown, "{name}");
        assert!(m.source.contains("1 of 2"), "{name}: {}", m.source);
    }
    let c = compare_runs(&input("a", &a), &input("b", &b));
    let cost = c.deltas.iter().find(|d| d.metric == "cost_usd").unwrap();
    assert!(matches!(&cost.change, Known::Gap(_)), "{:?}", cost.change);
    let t = render(&c);
    assert!(
        t.lines()
            .any(|l| l.contains("cost_usd") && l.contains("1 of 2 attempts")),
        "{t}"
    );
}

#[test]
fn a_hostile_usage_metric_is_cleaned_in_the_inspector_json_and_text() {
    let mut s = Shape::base();
    s.hostile_usage = true;
    let b = build(10, &s);
    for node in ["lead".to_owned(), Uuid::from_u128(11).to_string()] {
        let v = inspector(&b.rec, &node).unwrap();
        let json = serde_json::to_string(&v).unwrap();
        for bad in ['\u{202e}', '\u{9b}', '\u{1b}'] {
            assert!(!json.contains(bad), "{node}: {bad:?} in {json}");
        }
        assert!(
            json.contains("\\u{202e}") && json.contains("\\u{9b}"),
            "{node}: {json}"
        );
    }
    let t = render(&compare_runs(&input("a", &b), &input("b", &b)));
    assert!(!t.contains('\u{202e}') && !t.contains('\u{9b}'), "{t:?}");
}

#[test]
fn a_trace_read_with_skipped_lines_says_so_in_the_comparison() {
    let (a, b, ra, rb) = yes_pair();
    assert_eq!(with((&a, &ra), (&b, &rb)).comparable, Comparable::Yes);
    let mut raw = fs::read(a.dir.path().join("events.jsonl")).unwrap();
    raw.extend_from_slice(b"not json\n{\"schema_version\":99}\n{\"torn");
    fs::write(a.dir.path().join("events.jsonl"), raw).unwrap();
    let a2 = Built {
        rec: Recorded::load(a.dir.path()).unwrap(),
        dir: a.dir,
    };
    let c = with((&a2, &ra), (&b, &rb));
    let t = render(&c);
    assert!(
        t.contains("1 corrupt") && t.contains("another schema") && t.contains("torn"),
        "{t}"
    );
    let j = serde_json::to_value(&c).unwrap();
    assert_eq!(j["a"]["corrupt_lines"], 1, "{j}");
    assert_eq!(j["a"]["unsupported_schema"], 1, "{j}");
    assert_eq!(j["b"]["corrupt_lines"], 0, "{j}");
    assert!(
        !matches!(c.comparable, Comparable::Yes),
        "a partly read trace is not known to be like the other: {:?}",
        c.comparable
    );
}

#[test]
fn message_sizes_that_overflow_are_a_gap_and_never_a_panic() {
    let mut s = Shape::base();
    s.chunks = vec![u64::MAX, u64::MAX];
    let b = build(10, &s);
    let o = side(&input("a", &b)).outputs;
    assert_eq!(o.message_chunks, 2);
    assert!(
        matches!(&o.message_bytes, Known::Gap(why) if why.contains("overflow")),
        "{:?}",
        o.message_bytes
    );
}

#[test]
fn environment_values_are_compared_whole_and_not_as_they_are_cut_for_display() {
    let long = |tail: char| -> &'static str {
        Box::leak(format!("{}{tail}", "m".repeat(200)).into_boxed_str())
    };
    let (mut sa, mut sb) = (Shape::base(), Shape::base());
    sa.model = long('a');
    sb.model = long('b');
    let (a, b) = (build(10, &sa), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    assert_eq!(env_row(&c, "model").verdict, EnvVerdict::Differs);
    // A hidden character is not the same value as its spelled-out escape.
    sa.model = "x\u{202e}y";
    sb.model = r"x\u{202e}y";
    let (a, b) = (build(10, &sa), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    assert_eq!(env_row(&c, "model").verdict, EnvVerdict::Differs);
}

#[test]
fn a_run_that_spans_two_providers_is_not_the_same_as_one_of_the_first() {
    let mut sb = Shape::base();
    sb.extra_attempt = Some("org.other");
    let (a, b) = (build(10, &Shape::base()), build(20, &sb));
    let c = compare_runs(&input("a", &a), &input("b", &b));
    assert_eq!(
        env_row(&c, "provider_extension").verdict,
        EnvVerdict::Differs
    );
}
