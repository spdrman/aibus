//! #284: the views of a recorded run. Every spike trace (`fixtures/claude-code/traces-284/`, the
//! store's own files) against its golden graph, timeline, inspectors and replay, the acceptance
//! walk from the lead to two native children to their tools and the result, the marks for what
//! is missing, byte-for-byte replay, and hostile traces that must not panic, must not reach the
//! terminal as escape sequences and must not make a view open anything outside the run.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use aibus_trace::view::{self, Filter, Recorded, RunState};
use aibus_trace::{derived_id, AttemptRef, EventKind, Origin, TraceEvent, Visibility};
use serde_json::json;
use uuid::Uuid;

fn traces() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284")
}

fn goldens() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/views-284")
}

fn names() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(traces())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    v.sort();
    assert_eq!(v.len(), 20, "{v:?}");
    v
}

fn load(name: &str) -> Recorded {
    Recorded::load(&traces().join(name)).unwrap()
}

/// Every view of one run, the way the golden keeps it.
fn all_views(rec: &Recorded, name: &str) -> String {
    let mut s = view::graph(rec, name);
    s.push_str(&view::timeline(rec, &Filter::default()));
    let p = aibus_trace::project(&rec.events);
    let mut nodes: Vec<String> = p.agents.iter().map(|a| a.agent_id.clone()).collect();
    nodes.extend(p.tools.iter().map(|t| t.tool_use_id.clone()));
    for n in nodes {
        s.push_str("\ninspect ");
        s.push_str(&n);
        s.push('\n');
        s.push_str(&view::inspect(rec, &n).expect("every node has an inspector"));
    }
    s
}

fn check_golden(stale: &mut Vec<String>, file: &str, got: &str) {
    let path = goldens().join(file);
    if std::env::var_os("AIBUS_BLESS").is_some() {
        fs::create_dir_all(goldens()).unwrap();
        fs::write(&path, got).unwrap();
    } else if fs::read_to_string(&path).ok().as_deref() != Some(got) {
        stale.push(file.to_owned());
    }
}

/// Nothing a terminal would act on or hide: no control character but the newline, no bidi
/// override or isolate, no zero-width character.
fn assert_printable(what: &str, s: &str) {
    let bad = s.chars().find(|c| {
        (c.is_control() && *c != '\n')
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{061c}' | '\u{feff}')
    });
    assert!(bad.is_none(), "{what}: {bad:?} reaches the terminal");
}

#[test]
fn every_recorded_trace_renders_to_its_golden_views_and_replay() {
    let mut stale = Vec::new();
    for name in names() {
        let rec = load(&name);
        assert_eq!(rec.report, Default::default(), "{name}");
        check_golden(&mut stale, &format!("{name}.txt"), &all_views(&rec, &name));
        check_golden(
            &mut stale,
            &format!("{name}.replay.txt"),
            &view::replay(&rec, &name),
        );
    }
    check_golden(
        &mut stale,
        "fleet.txt",
        &view::render_fleet(&view::fleet(&traces())),
    );
    assert!(
        stale.is_empty(),
        "views differ from the goldens (AIBUS_BLESS=1 rewrites them): {stale:#?}"
    );
}

/// The acceptance walk: the lead, two native subagents it delegated to (observed, and
/// cross-checked once the hooks are in), each one's Read, and the attempt's result, and the raw
/// events behind each step can be opened.
#[test]
fn the_operator_follows_the_lead_to_two_native_children_to_their_tools_and_the_result() {
    let rec = load("11-foreground.hooks");
    let g = view::graph(&rec, "11-foreground.hooks");
    let lead = g
        .lines()
        .position(|l| l.contains("agent ") && l.contains(" lead, "))
        .expect("a lead line");
    let children: Vec<usize> = g
        .lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with("  | agent "))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(children.len(), 2, "{g}");
    let lines: Vec<&str> = g.lines().collect();
    for &c in &children {
        assert!(c > lead);
        assert!(
            lines[c + 1].contains("edge observed cross-checked (cli_stream, hook) via toolu_"),
            "{}",
            lines[c + 1]
        );
        assert!(
            lines[c + 2].contains("tool toolu_") && lines[c + 2].contains("Read: completed"),
            "{}",
            lines[c + 2]
        );
    }
    assert!(g.contains("attempt 00000000-0000-0000-0000-000002820002 completed"));
    assert!(g.contains("usage billed_cost_usd unknown usd (unknown, not reported by the CLI)"));
    assert!(g.contains("usage cost_usd 0.") && g.contains("(estimated, result.total_cost_usd"));

    // Open a child, then one of its raw events.
    let p = aibus_trace::project(&rec.events);
    let child = &p.edges[0].child;
    let ins = view::inspect(&rec, child).unwrap();
    assert!(ins.contains("edge observed cross-checked"));
    assert!(ins.contains("usage total_tokens"), "{ins}");
    assert!(ins.contains("(reported, task_notification.usage)"));
    let row = ins
        .lines()
        .find(|l| l.contains(" agent_started observed cli_stream"))
        .expect("its start event");
    assert!(
        row.contains("native system/task_started") && row.contains("raw sha256:"),
        "{row}"
    );
    let seq: u64 = row
        .trim_start()
        .trim_start_matches('#')
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let ev = view::event(&rec, seq).unwrap();
    assert!(ev.contains("\"kind\": \"agent_started\""), "{ev}");
    assert!(ev.contains("\"raw_sha256\""));
    assert!(ev.contains("payload: not retained"));
}

#[test]
fn a_retained_trace_shows_the_messages_and_tool_output_it_kept() {
    let rec = load("11-foreground.retained");
    let p = aibus_trace::project(&rec.events);
    let read = p
        .tools
        .iter()
        .find(|t| t.name.as_deref() == Some("Read"))
        .unwrap();
    let ins = view::inspect(&rec, &read.tool_use_id).unwrap();
    assert!(ins.contains("input and output:"), "{ins}");
    assert!(
        ins.contains("payload local://objects/") && ins.contains("(redacted when it was stored)")
    );
    let lead = p
        .agents
        .iter()
        .find(|a| a.role.as_deref() == Some("lead"))
        .unwrap();
    let ins = view::inspect(&rec, &lead.agent_id).unwrap();
    assert!(
        ins.contains("messages:") && ins.contains("\"text\""),
        "{ins}"
    );
}

#[test]
fn a_missing_child_message_and_an_unknown_cost_are_marked_never_filled_in() {
    // 07-deleg's children are background tasks: nothing they said reached the stream as a message
    // of their own in run 15, killed while they worked.
    let rec = load("15-killed-midrun");
    let g = view::graph(&rec, "15-killed-midrun");
    assert!(g.contains("messages none recorded (unavailable)"), "{g}");
    // A trace with no cost at all: the fleet and the graph say unknown, with no number.
    let t = Trace::new();
    let dir = t.write(&[
        t.ev(1, EventKind::AttemptStarted, |_| {}),
        t.ev(2, EventKind::AgentStarted, |e| {
            e.agent_id = Some("lead".into());
            e.data = json!({ "role": "lead" });
        }),
    ]);
    let rec = Recorded::load(dir.path()).unwrap();
    let g = view::graph(&rec, "t");
    assert!(g.contains("usage: not reported (unknown)"), "{g}");
    assert!(g.contains("messages none recorded (unavailable)"));
    assert!(g.contains("no end recorded"));
    let row = view::RunRow::of("t", &rec);
    assert_eq!(row.cost.value, None);
    assert_eq!(row.cost.quality, aibus_trace::DataQuality::Unknown);
    assert_eq!(row.state, RunState::Open);
    assert!(view::render_fleet(&[row]).contains("unknown usd (unknown)"));
}

#[test]
fn every_edge_and_node_line_says_its_evidence_level() {
    let levels = ["observed", "correlated", "inferred", "unavailable"];
    for name in names() {
        let g = view::graph(&load(&name), &name);
        for l in g.lines() {
            let t = l.trim_start_matches([' ', '|']);
            if t.starts_with("edge ") {
                assert!(
                    levels.iter().any(|v| t.starts_with(&format!("edge {v} "))),
                    "{name}: {l}"
                );
                assert!(
                    t.contains(" sources #"),
                    "{name}: an edge without its sources: {l}"
                );
            }
            if t.starts_with("agent ") || t.starts_with("tool ") || t.starts_with("attempt ") {
                assert!(
                    levels.iter().any(|v| t.contains(&format!("[{v} ")))
                        || t.contains("[no events]"),
                    "{name}: {l}"
                );
            }
            if t.starts_with("usage ") && !t.starts_with("usage:") {
                assert!(
                    ["(reported,", "(derived,", "(estimated,", "(unknown,"]
                        .iter()
                        .any(|q| t.contains(q)),
                    "{name}: {l}"
                );
            }
        }
    }
}

#[test]
fn replay_emits_the_recorded_trace_byte_for_byte_and_ends_in_the_same_graph() {
    for name in names() {
        let rec = load(&name);
        let file = fs::read(traces().join(&name).join("events.jsonl")).unwrap();
        assert_eq!(rec.raw.concat(), file, "{name}");
        let r = view::replay(&rec, &name);
        let (steps, end) = r.split_once("final state\n").unwrap();
        assert_eq!(
            end,
            view::graph(&rec, &name),
            "{name}: the replay ends somewhere else"
        );
        // One transition per recorded event, in sequence order.
        let seqs: Vec<u64> = steps
            .lines()
            .skip(1)
            .map(|l| {
                l.trim_start_matches('#')
                    .split(' ')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        let mut want: Vec<u64> = rec.events.iter().map(|e| e.sequence).collect();
        want.sort_unstable();
        assert_eq!(seqs, want, "{name}");
        assert!(steps.starts_with(&format!("replay of {name}: ")));
        assert!(steps
            .lines()
            .next()
            .unwrap()
            .contains("the recorded run, not a new one"));
    }
}

#[test]
fn the_timeline_filters_by_agent_tool_kind_and_level() {
    let rec = load("07-deleg.hooks");
    let p = aibus_trace::project(&rec.events);
    let child = p.edges[0].child.clone();
    let t = view::timeline(
        &rec,
        &Filter {
            agent: Some(child.clone()),
            ..Filter::default()
        },
    );
    assert!(
        t.lines()
            .skip(1)
            .all(|l| l.contains(&format!("agent={child}"))),
        "{t}"
    );
    let t = view::timeline(
        &rec,
        &Filter {
            level: Some(Visibility::Correlated),
            ..Filter::default()
        },
    );
    assert!(
        t.lines().skip(1).all(|l| l.contains(" correlated hook ")),
        "{t}"
    );
    let t = view::timeline(
        &rec,
        &Filter {
            kind: Some(EventKind::GapDetected),
            ..Filter::default()
        },
    );
    assert_eq!(t.lines().count(), 2, "{t}");
    let tool = p.tools[0].tool_use_id.clone();
    let t = view::timeline(
        &rec,
        &Filter {
            tool: Some(tool.clone()),
            ..Filter::default()
        },
    );
    assert!(
        t.lines().count() >= 3 && t.lines().skip(1).all(|l| l.contains(&tool)),
        "{t}"
    );
    let t = view::timeline(
        &rec,
        &Filter {
            agent: Some("nobody".into()),
            ..Filter::default()
        },
    );
    assert!(t.contains("no events match"));
}

#[test]
fn the_fleet_says_each_runs_state_and_how_much_of_its_trace_was_readable() {
    let rows = view::fleet(&traces());
    assert_eq!(rows.len(), 20);
    let by = |id: &str| rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(by("11-foreground").state, RunState::Completed);
    assert_eq!(by("10-killed").state, RunState::Interrupted);
    assert_eq!(by("11-foreground.hooks").edges, [2, 0, 0, 0]);
    // A torn tail and a corrupt line are counted, not hidden.
    let t = Trace::new();
    let dir = t.write(&[
        t.ev(1, EventKind::AttemptStarted, |_| {}),
        t.ev(2, EventKind::AuthRequired, |_| {}),
    ]);
    let mut raw = fs::read(dir.path().join("events.jsonl")).unwrap();
    raw.extend_from_slice(b"not json\n{\"schema_version\":9}\n{\"torn");
    fs::write(dir.path().join("events.jsonl"), raw).unwrap();
    let rec = Recorded::load(dir.path()).unwrap();
    let row = view::RunRow::of("x", &rec);
    assert_eq!(
        (row.corrupt_lines, row.unsupported_schema, row.torn_bytes),
        (1, 1, 6)
    );
    assert_eq!(
        row.state,
        RunState::Blocked,
        "a login was asked for and no end recorded"
    );
    assert!(view::render_fleet(&[row]).contains("1 corrupt lines, 6 torn bytes, 1 other schema"));
    assert!(view::graph(&rec, "x").contains(
        "data quality: 1 corrupt lines, 6 torn bytes, 1 lines of another schema were skipped"
    ));
}

#[test]
fn a_run_id_is_one_plain_name_or_a_unique_prefix() {
    let root = traces();
    assert_eq!(
        view::resolve(&root, "06-basic").unwrap(),
        root.join("06-basic")
    );
    assert_eq!(view::resolve(&root, "06").unwrap(), root.join("06-basic"));
    assert!(view::resolve(&root, "11-foreground.h").is_ok());
    assert!(view::resolve(&root, "11")
        .unwrap_err()
        .contains("names 3 runs"));
    for bad in [
        "../traces-284/06-basic",
        "/etc",
        "..",
        ".hidden",
        "a/b",
        "a\u{1b}[2J",
        "",
    ] {
        let e = view::resolve(&root, bad).unwrap_err();
        assert_printable("resolve", &e);
        assert!(e.contains("not a run id"), "{bad:?}: {e}");
    }
    assert!(view::resolve(&root, "99-nothing")
        .unwrap_err()
        .contains("no run 99-nothing"));
}

#[test]
fn a_recorded_plan_is_shown_as_intent_next_to_what_ran() {
    let t = Trace::new();
    let dir = t.write(&[
        t.ev(1, EventKind::WorkflowStarted, |_| {}),
        t.ev(2, EventKind::StepScheduled, |e| {
            e.data = json!({ "step": "plan" })
        }),
    ]);
    fs::write(
        dir.path().join("plan.json"),
        json!({
            "plan_hash": "sha256:abc",
            "steps": [
                { "key": "plan", "agent": "plan", "mode": "aibus_managed", "runtime": "aibus", "prompt": { "sha256": "sha256:p1" } },
                { "key": "code", "agent": "explore", "mode": "provider_native", "runtime": "claude-code", "prompt": { "sha256": "sha256:p2" } }
            ],
            "edges": [ { "from": "plan", "to": "code", "kind": "route", "when": "plan.completed" } ]
        })
        .to_string(),
    )
    .unwrap();
    let g = view::graph(&Recorded::load(dir.path()).unwrap(), "p");
    assert!(g.contains("plan (the definition graph: intent, not proof of execution)"));
    assert!(g.contains("  plan_hash sha256:abc"));
    assert!(
        g.contains("  step plan agent plan aibus_managed/aibus prompt sha256:p1  scheduled #2"),
        "{g}"
    );
    assert!(g.contains(
        "  step code agent explore provider_native/claude-code prompt sha256:p2  not in the trace"
    ));
    assert!(g.contains("  route plan -> code when plan.completed"));
    assert!(view::graph(&load("06-basic"), "06-basic")
        .contains("  not recorded: this run has no plan.json"));
}

#[test]
fn an_aibus_managed_child_attempt_is_labelled_as_linked_by_the_scheduler() {
    let t = Trace::new();
    let child = Uuid::from_u128(99);
    let dir = t.write(&[
        t.ev(1, EventKind::AttemptStarted, |_| {}),
        t.ev(2, EventKind::AttemptStarted, |e| {
            e.attempt_id = child;
            e.parent_attempt_id = Some(Uuid::from_u128(2));
        }),
    ]);
    let g = view::graph(&Recorded::load(dir.path()).unwrap(), "m");
    assert!(g.contains("lineage: aibus-managed child of attempt 00000000-0000-0000-0000-000000000002 (linked by the scheduler)"), "{g}");
    assert!(g.contains("lineage: provider-native"));
}

#[test]
fn a_missing_parent_is_an_unavailable_edge_with_a_gap_and_a_cycle_is_still_shown() {
    let t = Trace::new();
    let dir = t.write(&[
        t.ev(1, EventKind::AgentStarted, |e| {
            e.agent_id = Some("orphan".into());
        }),
        t.ev(2, EventKind::AgentStarted, |e| {
            e.agent_id = Some("a".into());
            e.parent_agent_id = Some("b".into());
        }),
        t.ev(3, EventKind::AgentStarted, |e| {
            e.agent_id = Some("b".into());
            e.parent_agent_id = Some("a".into());
        }),
        t.ev(4, EventKind::AgentStarted, |e| {
            e.agent_id = Some("lost".into());
            e.parent_agent_id = Some("ghost".into());
        }),
        t.ev(5, EventKind::AgentStarted, |e| {
            e.agent_id = Some("me".into());
            e.parent_agent_id = Some("me".into());
        }),
    ]);
    let rec = Recorded::load(dir.path()).unwrap();
    let g = view::graph(&rec, "c");
    assert!(
        g.contains("parent unavailable: nothing in the trace says who delegated; edge unavailable"),
        "{g}"
    );
    assert!(g.contains("#1 parent_unavailable (unavailable)"));
    assert!(g.contains("parent ghost is not in the trace; edge observed"));
    assert!(g.contains("not reachable from a root (the parent links form a cycle):"));
    assert!(g.contains("parent link points at itself; edge observed"));
    for id in ["orphan", "a", "b", "lost", "me"] {
        assert_eq!(
            g.matches(&format!("agent {id} ")).count(),
            1,
            "{id} once: {g}"
        );
    }
    let row = view::RunRow::of("c", &rec);
    assert_eq!(row.edges[3], 1);
}

/// A trace written by hand, one line per event, in a temporary run directory.
struct Trace {
    attempt: AttemptRef,
}

impl Trace {
    fn new() -> Self {
        Self {
            attempt: AttemptRef {
                workflow_run_id: Uuid::from_u128(1),
                attempt_id: Uuid::from_u128(2),
                parent_attempt_id: None,
                provider_extension: "org.aibus.test".into(),
            },
        }
    }

    fn ev(&self, n: u64, kind: EventKind, f: impl FnOnce(&mut TraceEvent)) -> TraceEvent {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_000 + n * 250);
        let mut e = TraceEvent::new(
            &self.attempt,
            derived_id(&[&n.to_le_bytes()]),
            kind,
            Origin::CliStream,
            Visibility::Observed,
            at,
            "test/1",
        );
        e.sequence = n;
        f(&mut e);
        e
    }

    fn write(&self, events: &[TraceEvent]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut body = Vec::new();
        for e in events {
            body.extend_from_slice(&serde_json::to_vec(e).unwrap());
            body.push(b'\n');
        }
        fs::write(dir.path().join("events.jsonl"), body).unwrap();
        dir
    }
}

/// Every view of `rec`, for every node and a few sequences, checked for what reaches a terminal.
fn every_view(what: &str, rec: &Recorded) -> usize {
    let mut out = vec![
        view::graph(rec, what),
        view::timeline(rec, &Filter::default()),
        view::replay(rec, what),
        view::render_fleet(&[view::RunRow::of(what, rec)]),
    ];
    let p = aibus_trace::project(&rec.events);
    for id in p
        .agents
        .iter()
        .map(|a| &a.agent_id)
        .chain(p.tools.iter().map(|t| &t.tool_use_id))
        .take(20)
    {
        out.extend(view::inspect(rec, id));
    }
    for e in rec.events.iter().take(5) {
        out.extend(view::event(rec, e.sequence));
        out.extend(view::inspect(rec, &e.attempt_id.to_string()));
    }
    for raw in rec.raw.iter().take(50) {
        let safe = view::terminal_safe_line(raw);
        let a: serde_json::Value = serde_json::from_slice(raw).unwrap();
        let b: serde_json::Value = serde_json::from_slice(&safe).expect("still JSON");
        assert_eq!(a, b, "{what}: the terminal-safe line says something else");
        assert_printable(
            what,
            std::str::from_utf8(&safe).unwrap().trim_end_matches('\n'),
        );
    }
    let mut n = 0;
    for s in out {
        assert_printable(what, &s);
        n += s.len();
    }
    n
}

const NASTY: [&str; 9] = [
    "\u{1b}[2J\u{1b}[H",
    "\u{1b}]52;c;cm0gLXJmIH4=\u{7}",
    "\u{9b}31m",
    "evil\u{202e}gnp.exe",
    "\u{200b}zero\u{feff}",
    "a\rb\u{8}c\u{7f}",
    "\u{1b}P+q\u{1b}\\",
    "\u{2066}iso\u{2069}",
    "plain",
];

#[test]
fn escape_sequences_in_names_ids_and_payloads_reach_the_terminal_as_text() {
    let t = Trace::new();
    let mut evs = Vec::new();
    for (i, nasty) in NASTY.iter().enumerate() {
        let n = i as u64 * 4 + 1;
        evs.push(t.ev(n, EventKind::AgentStarted, |e| {
            e.agent_id = Some(format!("agent{nasty}"));
            e.parent_agent_id = Some(format!("parent{nasty}"));
            e.observed_at = format!("2026{nasty}");
            e.provider_extension = format!("ext{nasty}");
            e.data = json!({ "role": nasty, "model": nasty });
        }));
        evs.push(t.ev(n + 1, EventKind::ToolFailed, |e| {
            e.agent_id = Some(format!("agent{nasty}"));
            e.native.tool_use_id = Some(format!("tool{nasty}"));
            e.native.native_type = Some(nasty.to_string());
            e.data = json!({ "tool": nasty, "reason": nasty });
            e.payload_ref = Some(format!("local://objects/{nasty}.json"));
        }));
        evs.push(t.ev(n + 2, EventKind::GapDetected, |e| {
            e.data = json!({ "reason": nasty })
        }));
        evs.push(t.ev(n + 3, EventKind::UsageReported, |e| {
            e.data = json!({ "scope": "attempt", "metrics": [{ "name": nasty, "value": 1.0, "unit": nasty, "quality": "reported", "source": nasty }] });
        }));
    }
    let dir = t.write(&evs);
    fs::write(dir.path().join("plan.json"), json!({ "plan_hash": NASTY[0], "steps": [{ "key": NASTY[1], "agent": NASTY[2] }], "edges": [{ "from": NASTY[3], "to": NASTY[4], "kind": NASTY[5], "when": NASTY[6] }] }).to_string()).unwrap();
    let rec = Recorded::load(dir.path()).unwrap();
    every_view("nasty", &rec);
    let g = view::graph(&rec, "nasty\u{1b}[31m");
    assert!(g.contains("agent\\u{1b}[2J\\u{1b}[H"), "{g}");
    assert!(g.contains("run nasty\\u{1b}[31m"));
    assert!(g.contains("evil\\u{202e}gnp.exe"));
}

#[test]
fn a_payload_reference_outside_the_objects_directory_is_never_opened() {
    let t = Trace::new();
    let secret = tempfile::tempdir().unwrap();
    fs::write(secret.path().join("canary.json"), "\"CANARY-284\"").unwrap();
    let refs = [
        format!(
            "local://objects/../../../../../../..{}/canary.json",
            secret.path().display()
        ),
        format!("file://{}/canary.json", secret.path().display()),
        format!("{}/canary.json", secret.path().display()),
        "local://objects/../events.jsonl".into(),
        format!("local://objects/{}.json", "A".repeat(64)),
    ];
    let evs: Vec<TraceEvent> = refs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            t.ev(i as u64 + 1, EventKind::ToolStarted, |e| {
                e.native.tool_use_id = Some(format!("t{i}"));
                e.payload_ref = Some(r.clone());
            })
        })
        .collect();
    let dir = t.write(&evs);
    let rec = Recorded::load(dir.path()).unwrap();
    for i in 0..refs.len() {
        let ins = view::inspect(&rec, &format!("t{i}")).unwrap();
        assert!(
            !ins.contains("CANARY-284") && !ins.contains("schema_version"),
            "{ins}"
        );
        assert!(
            ins.contains("is not a local object reference; not opened"),
            "{ins}"
        );
        let ev = view::event(&rec, i as u64 + 1).unwrap();
        assert!(!ev.contains("CANARY-284"));
    }
}

#[test]
fn hostile_traces_never_panic_and_stay_bounded() {
    let t = Trace::new();
    // Invalid UTF-8, a line nested past serde's limit, a 20 MB string, a duplicate id, duplicate
    // sequences, an unknown kind, a field of the wrong type, an empty line, a bare newline.
    let mut body = Vec::new();
    let mut push = |e: &TraceEvent| {
        body.extend_from_slice(&serde_json::to_vec(e).unwrap());
        body.push(b'\n');
    };
    let a = t.ev(1, EventKind::AgentStarted, |e| {
        e.agent_id = Some("lead".into())
    });
    push(&a);
    push(&a);
    push(&t.ev(1, EventKind::AgentStarted, |e| {
        e.agent_id = Some("lead".into())
    }));
    push(&t.ev(2, EventKind::ToolStarted, |e| {
        e.native.tool_use_id = Some("big".into());
        e.data = json!({ "tool": "x".repeat(20 << 20) });
    }));
    body.extend_from_slice(b"{\"schema_version\":1,\"kind\":\"\xff\xfe\"}\n");
    body.extend_from_slice(format!("{}1{}\n", "[".repeat(10_000), "]".repeat(10_000)).as_bytes());
    body.extend_from_slice(
        format!(
            "{{\"schema_version\":1,\"data\":{}{}}}\n",
            "{\"a\":".repeat(5_000),
            "}".repeat(5_000)
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"{\"schema_version\":1,\"kind\":\"no_such_kind\"}\n\n\n");
    body.extend_from_slice(b"{\"schema_version\":1,\"sequence\":\"seven\"}\n");
    body.extend_from_slice(&[0xff; 64]);
    body.push(b'\n');
    let rec = Recorded::from_bytes(Path::new("/nonexistent-284"), &body);
    assert_eq!(rec.events.len(), 4);
    let size = every_view("hostile", &rec);
    assert!(
        size < 2 << 20,
        "the 20 MB name is cut, not printed ({size} bytes)"
    );

    // A chain 20,000 agents deep, and 20,000 duplicate ids of one tool.
    let mut evs = Vec::new();
    for i in 0..20_000u64 {
        evs.push(t.ev(i + 1, EventKind::AgentStarted, |e| {
            e.agent_id = Some(format!("a{i}"));
            e.parent_agent_id = (i > 0).then(|| format!("a{}", i - 1));
            e.event_id = Uuid::from_u128(u128::from(i % 7));
        }));
    }
    for i in 0..20_000u64 {
        evs.push(t.ev(30_000 + i, EventKind::ToolCompleted, |e| {
            e.native.tool_use_id = Some("same".into())
        }));
    }
    let dir = t.write(&evs);
    let rec = Recorded::load(dir.path()).unwrap();
    let g = view::graph(&rec, "deep");
    assert!(
        g.contains("[depth 19999] agent a19999"),
        "the deepest agent is still placed"
    );
    assert!(
        g.lines().map(str::len).max().unwrap() < 400,
        "indentation stops growing"
    );
    assert_printable("deep", &g);
    let _ = view::replay(&rec, "deep");
    assert!(view::inspect(&rec, "same").unwrap().contains("tool same"));
    assert!(view::inspect(&rec, "nobody").is_none());
    assert!(view::event(&rec, 999_999).is_none());
    // A run directory with no trace in it, or a trace that is a directory.
    let empty = tempfile::tempdir().unwrap();
    assert!(Recorded::load(empty.path()).is_err());
    fs::create_dir(empty.path().join("events.jsonl")).unwrap();
    assert!(Recorded::load(empty.path()).is_err());
    assert!(view::fleet(empty.path()).is_empty());
}

/// xorshift64*: no dependency, the same inputs on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// 32,000 seeded traces: spike lines mutated byte by byte, spike events with their ids, parents,
/// sequences, kinds and strings scrambled (cycles, self-parents, missing parents, duplicate ids,
/// escape sequences), and random bytes. Every view of every one renders without a panic and
/// prints nothing a terminal would act on.
#[test]
fn fuzz_32000_traces_through_every_view() {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    for name in names() {
        let raw = fs::read(traces().join(&name).join("events.jsonl")).unwrap();
        lines.extend(raw.split_inclusive(|b| *b == b'\n').map(<[u8]>::to_vec));
    }
    let events: Vec<TraceEvent> = lines
        .iter()
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    let pool = [
        "lead",
        "a",
        "b",
        "c",
        "\u{1b}[2J",
        "x\u{202e}y",
        "\u{9b}0m",
        "",
    ];
    let mut rng = Rng(0x284_284_284);
    let mut rendered = 0usize;
    for round in 0..32_000 {
        let mut body = Vec::new();
        let n = 1 + rng.below(16);
        for _ in 0..n {
            match rng.below(3) {
                0 => {
                    let mut l = lines[rng.below(lines.len())].clone();
                    for _ in 0..1 + rng.below(4) {
                        let at = rng.below(l.len());
                        l[at] = rng.next() as u8;
                    }
                    if rng.below(4) == 0 {
                        l.truncate(rng.below(l.len()));
                    }
                    body.extend_from_slice(&l);
                    if !l.ends_with(b"\n") && rng.below(2) == 0 {
                        body.push(b'\n');
                    }
                }
                1 => {
                    let mut e = events[rng.below(events.len())].clone();
                    let mut name = || {
                        let s = pool[rng.below(pool.len())];
                        (rng.below(5) > 0).then(|| s.to_owned())
                    };
                    e.agent_id = name();
                    e.parent_agent_id = name();
                    e.parent_tool_use_id = name();
                    e.native.tool_use_id = name();
                    e.kind = kinds[rng.below(kinds.len())];
                    e.sequence = rng.below(8) as u64;
                    if rng.below(3) == 0 {
                        e.event_id = Uuid::from_u128(rng.below(4) as u128);
                    }
                    if rng.below(3) == 0 {
                        e.corroborates = Some(Uuid::from_u128(rng.below(4) as u128));
                    }
                    if rng.below(4) == 0 {
                        e.attempt_id = Uuid::from_u128(rng.below(3) as u128);
                        e.parent_attempt_id = Some(Uuid::from_u128(rng.below(3) as u128));
                    }
                    if rng.below(4) == 0 {
                        e.data = json!({ "role": pool[rng.below(pool.len())], "status": pool[rng.below(pool.len())], "reason": "permission_denied", "scope": "agent", "metrics": [{ "name": pool[rng.below(pool.len())], "value": null, "unit": "u", "quality": "unknown", "source": "s" }] });
                    }
                    if rng.below(6) == 0 {
                        e.payload_ref = Some(format!(
                            "local://objects/{}.json",
                            pool[rng.below(pool.len())]
                        ));
                    }
                    body.extend_from_slice(&serde_json::to_vec(&e).unwrap());
                    body.push(b'\n');
                }
                _ => {
                    for _ in 0..rng.below(64) {
                        body.push(rng.next() as u8);
                    }
                    body.push(b'\n');
                }
            }
        }
        let rec = Recorded::from_bytes(Path::new("/nonexistent-284"), &body);
        rendered += every_view(&format!("round {round}"), &rec);
    }
    assert!(rendered > 0);
}

/// Spec 7.3: a replay reads the trace and nothing else. The view module has no way to start a
/// process, open a socket or reach a provider.
#[test]
fn the_views_have_no_way_to_run_anything_or_reach_a_provider() {
    let src =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/view.rs")).unwrap();
    for word in [
        "std::process",
        "Command::new",
        "std::net",
        "TcpStream",
        "UdpSocket",
        "tokio",
        "reqwest",
        "aibus_provider",
        "unsafe",
    ] {
        assert!(!src.contains(word), "src/view.rs mentions {word}");
    }
    let toml =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let deps = toml
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    for word in [
        "tokio",
        "reqwest",
        "hyper",
        "aibus-provider",
        "aibus-supervisor",
    ] {
        assert!(!deps.contains(word), "aibus-trace depends on {word}");
    }
}
