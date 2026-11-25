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
    add(
        EventKind::MessageChunk,
        Some("lead"),
        None,
        200,
        json!({ "bytes": 42, "role": "assistant" }),
        &|e| {
            if s.retain {
                e.payload_ref = Some(format!("local://objects/{REPLY}.json"));
            }
        },
    );
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
    let metric = |name: &str, v: Option<f64>, unit: &str, q: &str| json!({ "name": name, "value": v, "unit": unit, "quality": q, "source": "result.usage" });
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
    let ra = result("a", 3, 1, 0, "abc123");
    let rb = result("b", 4, 0, 0, "abc123");
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
    let (ra, rb) = (result("a", 1, 0, 0, "abc"), result("b", 1, 0, 0, "abc"));
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
    let rc = result("b", 1, 0, 0, "def");
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
    let ra = result("a", 2, 0, 2, "abc");
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
