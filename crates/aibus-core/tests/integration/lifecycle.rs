//! 10 section 4, bullets 9 and 10: `max_turns` then `resume_run`, and compaction with its
//! auto-handoff.

use aibus_core::config;
use aibus_core::continuity::handoff::Handoff;
use aibus_core::continuity::{ledger, Layout};
use aibus_core::*;
use aibus_graph::Graph;
use aibus_provider::fake::Step;
use aibus_provider::ContentBlock;
use aibus_testkit::FakeClock;
use serde_json::json;

use crate::rig::*;

#[tokio::test]
async fn max_turns_yields_partial_and_resume_run_continues_with_history_intact() {
    let _clock = FakeClock::new();
    let mut set = Setup::default();
    set.defs.get_mut(&"explore".parse::<AgentName>().unwrap()).unwrap().max_turns = Some(1);
    let lead = script(
        delegate_call("explore", "list it", false)
            .into_iter()
            .chain(call("resume_run", json!({"run": "explore", "message": "carry on", "background": false})))
            .chain([text("fin")]),
    );
    let mut rig = Rig::new(set, lead, |s| {
        if s.resume {
            script([text("finished the job")])
        } else {
            script(call("ls", json!({"path": "."})))
        }
    });
    let out = rig.run("go").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    let launched = rig.launched();
    assert_eq!(launched.len(), 2);
    assert_eq!(launched[0].run, launched[1].run, "the same run id");
    let child = launched[0].run;
    // The first round ended Partial at its turn limit, with the last text it had.
    let partial = out
        .msgs
        .iter()
        .find_map(|m| match m {
            Msg::RunFinished { run, result } if *run == child => Some(result),
            _ => None,
        })
        .expect("the child finished once as partial");
    assert_eq!(partial.status, RunStatus::Partial);

    // The resumed run saw: the task, its `ls` call, the result, and the new message.
    let m = &rig.child_requests(child)[0].request.messages;
    assert_eq!(m.len(), 3, "{:?}", m.iter().map(|m| m.plain_text()).collect::<Vec<_>>());
    assert!(matches!(m[1].blocks[0], ContentBlock::ToolUse { .. }));
    assert!(m[2]
        .blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { content, .. } if content.contains("a.txt"))));
    assert!(m[2].plain_text().contains("carry on"));
}

#[tokio::test]
async fn compaction_writes_an_auto_handoff_and_a_boundary_and_the_next_request_is_shorter() {
    let _clock = FakeClock::new();
    let lead = script(
        [Step::Usage { input: 500, output: 5 }]
            .into_iter()
            .chain(call("ls", json!({"path": "."})))
            .chain([Step::MatchPrompt {
                contains: "Summarise the conversation below".into(),
                then: vec![text("SUMMARY: listed the directory"), end()],
            }])
            .chain([text("done after compacting")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |_| script([text("unused")]));
    rig.auto_compact_at = Some(100);
    let layout = Layout::new(&rig.dir, &config::Config::default(), &rig.dir, Some("auth")).unwrap();
    ledger::update(&layout, "goal", "Move auth to middleware", false, "2026-11-08").unwrap();
    ledger::update(&layout, "in progress", "run the migration\nadd a test", false, "2026-11-08").unwrap();
    rig.layout = Some(layout.clone());
    let out = rig.run("a long task").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    // The handoff comes first, from the ledger, and the graph knows it.
    let (written, path) = out
        .msgs
        .iter()
        .enumerate()
        .find_map(|(i, m)| match m {
            Msg::HandoffWritten { path } => Some((i, path.clone())),
            _ => None,
        })
        .expect("a HandoffWritten");
    let compacted = out.position(|m| matches!(m, Msg::Compacted { pre_tokens: 500, .. }));
    assert!(written < compacted);
    assert_eq!(path.parent().unwrap(), layout.handoff_dir());
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("auto-handoff-") && name.ends_with(".yaml"), "{name}");
    let h = Handoff::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!((h.goal(), h.now()), ("Move auth to middleware".to_owned(), "run the migration".to_owned()));
    let latest = rig.app.graph.read_key("model:handoff:latest").expect("model:handoff:latest");
    assert!(format!("{latest:?}").contains(&name), "{latest:?}");

    // The request after the boundary is the summary alone, so it is shorter than the history.
    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 3, "turn, summary, turn");
    let before = reqs[0].request.messages.len() + 2;
    let after = &reqs[2].request.messages;
    assert!(after.len() < before, "{} vs {before}", after.len());
    assert!(after[0].plain_text().contains("SUMMARY: listed the directory"));

    // Nothing is lost on disk: one boundary, and both the first message and what came after.
    let saved = conversation(&rig, rig.lead);
    assert_eq!(saved.matches("compact_boundary").count(), 1);
    assert!(saved.contains("a long task") && saved.contains("done after compacting"));
}
