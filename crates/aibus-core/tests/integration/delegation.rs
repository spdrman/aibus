//! 10 section 4, bullets 1 to 4: background and foreground delegation, the depth limit and the
//! concurrency cap.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_core::*;
use aibus_graph::Graph;
use aibus_provider::fake::Step;
use aibus_provider::StopReason;
use aibus_testkit::FakeClock;
use serde_json::{json, Value};

use crate::common::{app, rid, A, R};
use crate::rig::*;

#[tokio::test]
async fn lead_delegates_to_a_background_worker_and_the_result_arrives_wrapped_and_capped() {
    let _clock = FakeClock::new();
    let long = "x".repeat(200);
    let lead = script(
        delegate_call("explore", "find it", true)
            .into_iter()
            .chain([delay(10), text("working"), end(), text("noted")]),
    );
    let set = Setup {
        tool_limits: ToolLimits {
            result_max_bytes: 64,
            ..ToolLimits::default()
        },
        ..Setup::default()
    };
    let mut rig = Rig::new(set, lead, move |_| script([delay(5), text(&long)]));
    let out = rig.run("go").await;

    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);
    let child = rig.launched()[0].run;
    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 3, "the report made the lead take another turn");

    // The tool result is the run id and nothing else, long before the child is done.
    let answer: Value = serde_json::from_str(&results(&reqs[1])[0]).unwrap();
    assert_eq!(answer, json!({ "run_id": child.to_string() }));

    // The report arrives in the next request, wrapped, capped, with a pointer to the full text.
    let report = user_text(&reqs[2])
        .into_iter()
        .find(|t| t.contains("sub-agent's report"))
        .expect("the report was injected");
    assert!(report.starts_with("The following is the sub-agent's report."), "{report}");
    assert!(report.contains(&format!("[run {child} · explore · completed")), "{report}");
    assert!(report.contains("summary cut at 64 bytes"), "{report}");
    let full = std::fs::read_to_string(rig.dir.join(format!("runs/{child}/result.md"))).unwrap();
    assert_eq!(full.len(), 200);
    assert!(report.contains(&full[..64]) && !report.contains(&full[..65]));
}

#[tokio::test]
async fn a_foreground_delegate_blocks_the_parents_tool_call_until_the_child_ends() {
    let clock = FakeClock::new();
    let lead = script(delegate_call("explore", "find it", false).into_iter().chain([text("thanks")]));
    let mut rig = Rig::new(Setup::default(), lead, |_| script([delay(5), text("child says hi")]));
    let out = rig.run("go").await;

    assert!(clock.elapsed() >= Duration::from_secs(5), "the lead waited for the child");
    let child = rig.launched()[0].run;
    let child_done = out.position(|m| matches!(m, Msg::RunFinished { run, .. } if *run == child));
    let call_done = out.position(|m| matches!(m, Msg::ToolCallFinished { run, .. } if *run == rig.lead));
    assert!(child_done < call_done, "the tool call ended after the child did");

    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 2);
    let result = &results(&reqs[1])[0];
    assert!(result.starts_with("The following is the sub-agent's report."), "{result}");
    assert!(result.contains("child says hi"));
}

#[tokio::test]
async fn nesting_stops_at_max_spawn_depth_and_delegate_is_absent_from_the_tool_list_there() {
    let _clock = FakeClock::new();
    let lead = script(delegate_call("general", "level 1", false).into_iter().chain([text("all done")]));
    let mut rig = Rig::new(Setup::default(), lead, |spec| match spec.depth {
        1 => script(delegate_call("general", "level 2", false).into_iter().chain([text("one done")])),
        2 => script(delegate_call("general", "level 3", false).into_iter().chain([text("two done")])),
        _ => script([text("three done")]),
    });
    let out = rig.run("go").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    let launched = rig.launched();
    assert_eq!(launched.iter().map(|s| s.depth).collect::<Vec<_>>(), [1, 2, 3]);
    let offered = |i: usize| tool_names(&rig.child_requests(launched[i].run)[0]);
    assert!(tool_names(&rig.lead_requests()[0]).contains(&"delegate".to_owned()));
    assert!(offered(0).contains(&"delegate".to_owned()), "depth 1");
    assert!(offered(1).contains(&"delegate".to_owned()), "depth 2");
    assert!(!offered(2).contains(&"delegate".to_owned()), "depth 3: {:?}", offered(2));
}

fn wired() -> (A, Rc<RefCell<Vec<Effect>>>) {
    let mut a = app();
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    a.effects = Effects::new(move |e| sink.borrow_mut().push(e));
    (a, log)
}

fn started(a: &mut A, run: RunId) {
    a.step(Msg::RunStarted {
        run,
        agent: "worker".parse().unwrap(),
        role: Role::Worker,
        parent: None,
        model: ModelId("m".into()),
        account: AccountName("acct".into()),
    })
    .unwrap();
}

fn runnable(a: &A) -> Vec<String> {
    let v = a.graph.read_key("model:orchestrator:runnable").unwrap();
    as_list(v).iter().map(|r| as_str(get(r, "run").unwrap()).unwrap().to_owned()).collect()
}

fn status(a: &A, run: RunId) -> String {
    as_str(a.graph.read_key(&format!("model:run:{run}:status")).unwrap()).unwrap().to_owned()
}

#[test]
fn the_ninth_run_stays_queued_until_one_finishes_and_runnable_says_so() {
    let (mut a, log) = wired();
    let spawns = |log: &Rc<RefCell<Vec<Effect>>>| {
        log.borrow().iter().filter(|e| matches!(e, Effect::Spawn(_))).count()
    };
    started(&mut a, R());
    // Eight asks on top of the lead: the lead plus seven are the eight allowed, the last waits.
    let kids: Vec<RunId> = (1..=8)
        .map(|i| {
            let run = rid(100 + i);
            a.step(Msg::Command(Command::Delegate {
                agent: "worker".parse().unwrap(),
                prompt: format!("task {i}"),
                background: true,
                isolation: Isolation::None,
                parent: None,
                run: Some(run),
                model: None,
                account: None,
            }))
            .unwrap();
            run
        })
        .collect();
    for k in &kids[..7] {
        started(&mut a, *k);
    }
    let counts = a.graph.read_key("model:orchestrator:counts").unwrap();
    assert_eq!(as_i64(get(counts, "running").unwrap()), Some(8));
    assert_eq!(status(&a, kids[7]), "queued");
    assert!(runnable(&a).is_empty(), "model:orchestrator:runnable excludes the ninth");
    assert_eq!(spawns(&log), 7, "nothing more was handed to the spawner");

    a.step(Msg::RunFinished {
        run: kids[0],
        result: RunResult {
            run_id: kids[0],
            agent: "worker".parse().unwrap(),
            status: RunStatus::Completed,
            turns: 1,
            usage: Default::default(),
            summary: String::new(),
            summary_file: None,
            duration: Default::default(),
            error: None,
            worktree: None,
        },
    })
    .unwrap();
    assert_eq!(runnable(&a), [kids[7].to_string()], "a slot freed up and the ninth is runnable");
    assert_eq!(spawns(&log), 8);
}

/// The same cap seen from the lead: the call past the limit gets the error as its result.
#[tokio::test]
async fn a_delegate_past_the_concurrency_cap_returns_the_error_to_the_lead() {
    let _clock = FakeClock::new();
    let ask = |prompt: &str| Step::ToolCall {
        name: "delegate".into(),
        input: json!({"agent": "explore", "prompt": prompt}),
    };
    let lead = script([ask("a"), ask("b"), Step::Stop(StopReason::ToolUse), text("both asked")]);
    let set = Setup {
        limits: Limits {
            max_concurrent_runs: 2,
            ..Limits::default()
        },
        ..Setup::default()
    };
    let mut rig = Rig::new(set, lead, |_| script([delay(30), text("x")]));
    let out = rig.run("go").await;
    let got = results(&rig.lead_requests()[1]);
    assert_eq!(got[1], r#"{"error":"concurrent run limit reached (2)"}"#);
    assert_eq!(rig.launched().len(), 1, "only the first got a run");
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);
}
