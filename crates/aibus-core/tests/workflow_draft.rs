//! G6: what the reducer keeps when the workflow author finishes. The last fenced `toml` block of
//! its result is the draft `/workflow save` writes, and only a draft that validates against the
//! agents the session has replaces the one before it.

mod common;

use std::path::Path;
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::state::value::{as_str, get};
use aibus_core::state::WORKFLOW_DRAFT;
use aibus_core::*;
use aibus_graph::Graph;
use common::*;

const GOOD: &str = "name = \"pr-review\"\n\n[[step]]\nagent = \"explore\"\nprompt = \"Find it: {{input}}\"\n\n[[step]]\nagent = \"plan\"\nprompt = \"Review it: {{prev.summary}}\"\n";

fn finish(a: &mut A, n: u128, agent: &str, summary: &str) {
    let run = rid(n);
    a.step(Msg::RunStarted {
        run,
        agent: agent.parse().unwrap(),
        role: Role::Worker,
        parent: None,
        model: ModelId("m".into()),
        account: AccountName("acct".into()),
    })
    .unwrap();
    a.step(Msg::RunFinished {
        run,
        result: RunResult {
            run_id: run,
            agent: agent.parse().unwrap(),
            status: RunStatus::Completed,
            turns: 1,
            usage: Usage::default(),
            summary: summary.into(),
            summary_file: None,
            duration: Duration::from_secs(1),
            error: None,
            worktree: None,
        },
    })
    .unwrap();
}

fn draft(a: &A) -> Option<String> {
    let d = a.graph.read_key(WORKFLOW_DRAFT)?;
    get(d, "toml").and_then(as_str).map(str::to_owned)
}

#[test]
fn a_finished_author_sets_the_draft_only_when_it_validates() {
    let mut a = app();
    let found = discover(None, Path::new("/nonexistent"), None, &[], &[]).unwrap();
    a.step(Msg::AgentsReloaded { defs: found.agents.values().cloned().collect(), warnings: vec![] }).unwrap();
    assert_eq!(draft(&a), None);

    // Another agent's fenced block is not a draft.
    finish(&mut a, 2, "explore", &format!("```toml\n{GOOD}```"));
    assert_eq!(draft(&a), None);

    finish(&mut a, 3, "workflow-author", &format!("Here.\n```toml\n{GOOD}```\nwhy: explore finds it."));
    assert_eq!(draft(&a).as_deref(), Some(GOOD));
    let summary = get(a.graph.read_key(WORKFLOW_DRAFT).unwrap(), "summary").and_then(as_str).unwrap();
    assert!(summary.starts_with("valid, 2 steps"), "{summary}");

    // A later draft that names an agent nobody has leaves the good one where it was.
    let bad = GOOD.replace("agent = \"plan\"", "agent = \"ghost\"");
    finish(&mut a, 4, "workflow-author", &format!("```toml\n{bad}```"));
    assert_eq!(draft(&a).as_deref(), Some(GOOD));

    // The last block of a result wins, and a result with none changes nothing.
    let better = GOOD.replace("Find it", "Find out");
    finish(&mut a, 5, "workflow-author", &format!("```toml\n{GOOD}```\nbetter:\n```toml\n{better}```"));
    assert_eq!(draft(&a).as_deref(), Some(better.as_str()));
    finish(&mut a, 6, "workflow-author", "I could not.");
    assert_eq!(draft(&a).as_deref(), Some(better.as_str()));
}
