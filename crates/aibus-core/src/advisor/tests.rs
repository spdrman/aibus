use super::*;
use crate::msg::Checkpoint as C;

fn all() -> Triggers {
    let specs: Vec<CheckpointSpec> = ["before_plan", "same_error_x2", "before_done"]
        .into_iter()
        .map(|n| CheckpointSpec { name: n.into(), on: None, matcher: None })
        .collect();
    let on: Vec<String> = specs.iter().map(|s| s.name.to_string()).collect();
    Triggers::new(&specs, &on)
}

fn call(name: &str) -> Message {
    Message {
        role: MessageRole::Assistant,
        blocks: vec![ContentBlock::ToolUse {
            id: ToolCallId(ulid::Ulid::new()),
            name: name.into(),
            input: serde_json::json!({}),
        }],
        at: std::time::SystemTime::UNIX_EPOCH,
    }
}

#[test]
fn errors_normalise_whitespace_digits_and_paths() {
    let a = normalise_error("open  /tmp/a1/x.rs:12:5 failed\n after 30 ms (pid 4411)");
    let b = normalise_error("open /home/me/y.rs:99:1 failed after 7 ms (pid 2)");
    assert_eq!(a, b);
    assert_eq!(a, "open <path> failed after # ms (pid #)");
    assert_ne!(normalise_error("no such file"), normalise_error("permission denied"));
}

#[test]
fn before_plan_fires_on_the_plan_tool_the_heading_and_plan_mode() {
    let mut t = all();
    assert!(matches!(t.on_assistant(&call("plan")), Some((C::BeforePlan, _))));
    assert!(matches!(t.on_assistant(&call("enter_plan_mode")), Some((C::BeforePlan, _))));
    let text = |s: &str| Message::text(MessageRole::Assistant, s);
    assert!(t.on_assistant(&text("ok\n## Plan\n1. x")).is_some());
    assert!(t.on_assistant(&text("# PLAN: split it")).is_some());
    assert!(t.on_assistant(&text("I planned nothing")).is_none());
    assert!(t.on_assistant(&call("grep")).is_none());
}

#[test]
fn same_error_fires_once_per_pair_and_only_for_the_same_tool_and_text() {
    let mut t = all();
    assert!(t.on_result("bash", "exit 1 at /a/1", true).is_empty());
    let hit = t.on_result("bash", "exit 2 at /b/2", true);
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].0, C::SameErrorX2);
    // The third failure starts a new pair, it does not complete the old one.
    assert!(t.on_result("bash", "exit 3 at /c/3", true).is_empty());
    assert_eq!(t.on_result("bash", "exit 4 at /d/4", true).len(), 1);
    // A different tool, a different text or a success in between breaks the pair.
    assert!(t.on_result("read", "boom", true).is_empty());
    assert!(t.on_result("bash", "boom", true).is_empty());
    assert!(t.on_result("bash", "fine", false).is_empty());
    assert!(t.on_result("bash", "boom", true).is_empty());
}

#[test]
fn before_done_needs_a_completed_worker_and_fires_again_only_for_new_work() {
    let mut t = all();
    assert!(t.on_idle(0).is_none());
    assert!(t.on_idle(1).is_some());
    assert!(t.on_idle(1).is_none());
    assert!(t.on_idle(2).is_some());
}

#[test]
fn custom_checkpoints_match_the_tool_or_the_result_and_builtins_can_be_left_off() {
    let specs = vec![
        CheckpointSpec {
            name: "secrets".into(),
            on: Some("tool_result".into()),
            matcher: Some("password|^bash$".into()),
        },
        CheckpointSpec { name: "before_done".into(), on: None, matcher: None },
    ];
    let mut t = Triggers::new(&specs, &["before_done".to_owned()]);
    let hit = t.on_result("read", "the password is", false);
    assert_eq!(hit[0].0, C::Custom("secrets".into()));
    assert_eq!(t.on_result("bash", "", false).len(), 1);
    assert!(t.on_result("read", "plain", false).is_empty());
    // `before_plan` is not listed, so it is off.
    assert!(t.on_assistant(&call("plan")).is_none());
    // `same_error_x2` is not listed either.
    assert!(t.on_result("x", "e", true).is_empty() && t.on_result("x", "e", true).is_empty());
}

fn session(l: &Ledger, big: usize) -> (RunId, RunId) {
    let (lead, worker) = (RunId::new(), RunId::new());
    l.begin(lead, &"lead".parse().unwrap(), Role::Lead, "fix it", 1);
    l.begin(worker, &"search".parse().unwrap(), Role::Worker, &"w".repeat(900), 1);
    let id = ToolCallId(ulid::Ulid::new());
    let mut msgs = vec![
        Message::text(MessageRole::User, "task"),
        Message {
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock::Text("L".repeat(1000)),
                ContentBlock::ToolUse { id, name: "grep".into(), input: serde_json::json!({"p": "x"}) },
            ],
            at: std::time::SystemTime::UNIX_EPOCH,
        },
        Message {
            role: MessageRole::User,
            blocks: vec![ContentBlock::ToolResult { id, content: "R".repeat(big), is_error: false }],
            at: std::time::SystemTime::UNIX_EPOCH,
        },
    ];
    l.sync(lead, &msgs);
    msgs[1].blocks[0] = ContentBlock::Text("Z".repeat(1000));
    l.sync(worker, &msgs);
    (lead, worker)
}

#[test]
fn the_packet_keeps_the_leads_text_whole_and_cuts_results_and_worker_text() {
    let l = Ledger::default();
    session(&l, 5000);
    let p = l.packet(400_000);
    assert!(p.text.contains(&"L".repeat(1000)), "the lead's text is whole");
    assert!(!p.text.contains(&"Z".repeat(1000)), "a worker's text is cut");
    assert!(p.text.contains(&"Z".repeat(400)));
    assert!(p.text.contains("tool grep {\"p\":\"x\"}"));
    assert!(p.text.contains(&"R".repeat(400)) && !p.text.contains(&"R".repeat(401)));
    assert!(!p.text.contains(&"w".repeat(401)));
}

#[test]
fn a_big_session_loses_old_results_first_and_then_old_messages() {
    let l = Ledger::default();
    let (lead, _) = session(&l, 5000);
    let full = l.packet(400_000);
    // Room for everything but the results.
    let tight = l.packet(full.tokens - 150);
    assert!(tight.text.contains("[result elided]"), "{}", tight.text);
    assert!(tight.text.contains(&"L".repeat(1000)));
    // Room for little: the oldest messages go.
    let tiny = l.packet(400);
    assert!(tiny.text.contains("elided"));
    assert!(tiny.tokens <= 400 + 100, "{}", tiny.tokens);
    assert!(!tiny.text.contains("user: fix it"));
    let _ = lead;
}

#[test]
fn completed_workers_counts_only_workers_that_completed() {
    let l = Ledger::default();
    let (lead, worker) = session(&l, 10);
    assert_eq!(l.completed_workers(), 0);
    l.finish(lead, RunStatus::Completed);
    assert_eq!(l.completed_workers(), 0);
    l.finish(worker, RunStatus::Failed);
    assert_eq!(l.completed_workers(), 0);
    l.finish(worker, RunStatus::Completed);
    assert_eq!(l.completed_workers(), 1);
}
