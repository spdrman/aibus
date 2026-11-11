//! G2: a run reads its model per request. A switch that arrives while a request is in flight
//! lets that request finish on the old model; the next one carries the new model, and goes to
//! the new model's provider.

use std::sync::Arc;
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::StopReason;
use aibus_testkit::{fake_credential, xdg_temp};
use tokio::sync::mpsc;

fn script(steps: impl IntoIterator<Item = Step>) -> Script {
    steps.into_iter().fold(Script::new(), Script::then)
}

#[tokio::test(start_paused = true)]
async fn the_request_after_set_model_carries_the_new_model_and_the_in_flight_one_does_not() {
    let dir = xdg_temp();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let mut def = found.agents[&"general".parse::<AgentName>().unwrap()].clone();
    def.permission_mode = PermissionMode::Bypass;

    // Request 1 stays in flight for a second, then asks for a tool; request 2 is the next one.
    let old = Arc::new(FakeProvider::new(script([
        Step::Delay(Duration::from_secs(1)),
        Step::ToolCall { name: "bash".into(), input: serde_json::json!({"command": "true"}) },
        Step::Stop(StopReason::ToolUse),
    ])));
    let new = Arc::new(FakeProvider::new(script([Step::Text("done".into()), Step::Stop(StopReason::EndTurn)])));

    let path = vec![("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())];
    let bash = Arc::new(Bash::new(20, vec![], path, dir.path().to_path_buf(), "s".into()));
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let cfg = RunConfig {
        run: RunId::new(),
        def,
        prompt: "go".into(),
        parent: None,
        depth: 0,
        model: ModelId("fake-small".into()),
        account: AccountName("fake".into()),
        cwd: dir.path().to_path_buf(),
        runs_dir: dir.path().join("runs"),
        limits: ToolLimits::default(),
        budgets: Budgets::default(),
        date: "2026-11-11".into(),
        data_home: None,
        context: Vec::new(),
        background: false,
        max_spawn_depth: 3,
        forced: None,
        start: Start::New,
        worktree: None,
    };
    let deps = RunDeps {
        provider: old.clone(),
        creds: Arc::new(fake_credential()),
        tools: Arc::new(ToolExecutor::new(ToolRegistry::standard(bash))),
        tx,
        hooks: Arc::new(HookRunner::default()),
        compactor: None,
        assist: None,
    };
    let handle = spawn(cfg, deps);

    // Half a second in, request 1 is in flight.
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle
        .mailbox
        .send(RunMail::Model(ModelSwitch {
            provider: new.clone(),
            creds: Arc::new(fake_credential()),
            model: ModelId("fake-large".into()),
            account: AccountName("fake-2".into()),
        }))
        .await
        .unwrap();

    while let Some(m) = rx.recv().await {
        if m.name() == "RunFinished" {
            break;
        }
    }
    handle.join.await.unwrap();

    let first = old.requests();
    assert_eq!(first.len(), 1, "the old provider served only the request that was in flight");
    assert_eq!(first[0].request.model, ModelId("fake-small".into()));
    let second = new.requests();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].request.model, ModelId("fake-large".into()), "the next request carries the new model");
}
