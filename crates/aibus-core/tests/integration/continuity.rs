//! 10 section 4, bullet 11: `session_end` writes a handoff with the outcome, extracts
//! learnings, indexes them, and `recall` finds them. The binary does the same steps in
//! `crates/aibus/src/continuity.rs`; this test strings the core functions together the same way
//! and then starts a second session on the result.

use std::sync::Arc;
use std::time::SystemTime;

use aibus_core::config::Config;
use aibus_core::continuity::handoff::{self, Front, Handoff};
use aibus_core::continuity::recall::MemoryIndex;
use aibus_core::continuity::{learn, memory, Layout};
use aibus_core::persist::index::Index;
use aibus_core::persist::time::date;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Step};
use aibus_provider::StopReason;
use aibus_testkit::fake_credential;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::rig::{path_env, script};
use crate::solo::{bypass, Solo};

const LEARNINGS: &str = r#"[{"type":"working_solution","confidence":"high","content":"Run the failing test alone first to see the real error"},{"type":"fact","confidence":"medium","content":"Refresh tokens rotate on every use"},{"type":"gossip","confidence":"high","content":"dropped"}]"#;

#[tokio::test]
async fn session_end_writes_a_handoff_with_the_outcome_extracts_learnings_and_recall_finds_them() {
    let s = Solo::new();
    let layout = Layout::new(&s.dir, &Config::default(), &s.xdg.paths.data, Some("auth")).unwrap();
    let redactor = Redactor::new();

    // The session: the lead keeps the ledger up to date through the tool, as a run does.
    let bash = Arc::new(Bash::new(20, vec![], path_env(), s.dir.clone(), "s".into()));
    let mut registry = ToolRegistry::standard(bash);
    registry.register(Arc::new(LedgerUpdate::new(layout.clone(), redactor.clone())));
    let tools = Arc::new(ToolExecutor::new(registry));
    let lead = script([
        Step::ToolCall {
            name: "ledger_update".into(),
            input: json!({"section": "Goal", "text": "Move auth to middleware"}),
        },
        Step::ToolCall {
            name: "ledger_update".into(),
            input: json!({"section": "In Progress", "text": "run the migration\nadd a test"}),
        },
        Step::Stop(StopReason::ToolUse),
        Step::Text("The ledger is up to date.".into()),
    ]);
    let done = s.go(lead, bypass("lead"), |_, d| d.tools = tools).await;
    let result = done.finished();
    assert_eq!(result.status, RunStatus::Completed);
    let saved = aibus_core::run::transcript::load(
        &s.xdg.paths.state.join(format!("runs/{}/conversation.jsonl", result.run_id)),
    )
    .unwrap();

    // session_end, first half: the handoff, built from the ledger and the conversation.
    let ledger = aibus_core::continuity::ledger::read(&layout).unwrap();
    let mut body = serde_json::Value::Object(handoff::summarise(ledger.as_ref(), Some(&saved.conv)));
    redactor.redact_json(&mut body);
    let now = SystemTime::now();
    let front = Front {
        session: &layout.topic,
        date: &date(now),
        status: handoff::status_of(result.status),
        outcome: Some("PARTIAL_PLUS"),
    };
    let h = Handoff::build(&front, body.as_object().unwrap()).unwrap();
    let path = handoff::write(&layout, &h, false, now).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    for want in ["session: auth\n", "outcome: PARTIAL_PLUS\n", "goal: Move auth to middleware\n", "now: run the migration\n"] {
        assert!(text.contains(want), "{want}: {text}");
    }
    assert_eq!(Handoff::parse(&text).unwrap().to_json()["next"], json!(["add a test"]));

    // Second half: one model call for the learnings, the invalid one dropped, then the index.
    let extractor = ModelCompactor {
        provider: Arc::new(FakeProvider::new(script([Step::Text(LEARNINGS.into()), Step::Stop(StopReason::EndTurn)]))),
        creds: Arc::new(fake_credential()),
        model: ModelId("fake-small".into()),
        handoff: None,
    };
    let found = extractor.extract(&saved.conv, &CancellationToken::new()).await.unwrap();
    assert_eq!(found.len(), 2);
    let file = memory::scope_dir(MemoryScope::User, "lead", &layout.project, &layout.data).join("LEARNINGS.md");
    learn::append(&file, &found, &format!("{} session s1", date(now)), &redactor).unwrap();
    let mut index = Index::open(&layout.index()).unwrap();
    index.sync(&layout).unwrap();

    let hits = index.recall(&layout, "failing test", 3).unwrap();
    assert!(hits[0].path.ends_with("agent-memory/lead/LEARNINGS.md"), "{hits:?}");
    assert!(hits[0].snippet.contains("[failing]"), "{hits:?}");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("dropped"));

    // The next session's lead is shown the handoff of the topic.
    let newest = handoff::latest(&layout.handoffs(), Some("auth")).expect("a handoff to resume from");
    let shown = handoff::context(&newest).unwrap();
    assert!(shown.contains("Move auth to middleware") && shown.contains("run the migration"), "{shown}");
}
