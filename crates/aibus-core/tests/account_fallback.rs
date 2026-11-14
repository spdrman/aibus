//! F9: a run moves to the next account when its own fails before the first token of a turn.
//! Two accounts, each a fake provider with its own script; the registry is the real one, so the
//! cooldown a switch leaves behind is the one `select` reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::config::{Config, FallbackKind, Hooks};
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{ProviderError, ProviderErrorKind, ProviderRegistry};
use aibus_testkit::{fake_credential, xdg_temp, FakeClock, Xdg};
use tokio::sync::mpsc;

const CONFIG: &str = r#"
[[account]]
name = "a"
provider = "fake"
auth = { none = true }
fallback = ["b"]

[[account]]
name = "b"
provider = "fake"
auth = { none = true }
"#;

fn name(n: &str) -> AccountName {
    AccountName(n.into())
}

fn err(kind: ProviderErrorKind, message: &str) -> Step {
    Step::Error(ProviderError::new(kind, message))
}

fn script(steps: impl IntoIterator<Item = Step>) -> Script {
    steps.into_iter().fold(Script::new(), Script::then)
}

fn registry() -> Arc<AccountRegistry> {
    let cfg: Config = toml::from_str(CONFIG).unwrap();
    let providers =
        ProviderRegistry::new(&[Box::new(aibus_provider::fake::Factory)], &BTreeMap::new())
            .unwrap();
    Arc::new(AccountRegistry::new(&cfg, Arc::new(providers)))
}

struct Done {
    a: Arc<FakeProvider>,
    b: Arc<FakeProvider>,
    registry: Arc<AccountRegistry>,
    msgs: Vec<Msg>,
    dir: Xdg,
    run: RunId,
}

impl Done {
    fn finished(&self) -> &RunResult {
        self.msgs
            .iter()
            .find_map(|m| match m {
                Msg::RunFinished { result, .. } => Some(result),
                _ => None,
            })
            .expect("a RunFinished")
    }

    fn switches(&self) -> Vec<(AccountName, AccountName)> {
        self.msgs
            .iter()
            .filter_map(|m| match m {
                Msg::RunSwitched { from, to, .. } => Some((from.clone(), to.clone())),
                _ => None,
            })
            .collect()
    }

    fn transcript(&self) -> String {
        let path = self
            .dir
            .path()
            .join("runs")
            .join(self.run.to_string())
            .join("conversation.jsonl");
        std::fs::read_to_string(path).unwrap_or_default()
    }
}

/// Runs a lead on account `a` with `b` as its fallback for `on`.
async fn run(a: Script, b: Script, on: &[FallbackKind]) -> Done {
    let _clock = FakeClock::new();
    let (a, b) = (
        Arc::new(FakeProvider::new(a)),
        Arc::new(FakeProvider::new(b)),
    );
    let registry = registry();
    let dir = xdg_temp();
    std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
    let path_env = vec![(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    )];
    let tools = Arc::new(ToolExecutor::new(ToolRegistry::standard(Arc::new(
        Bash::new(
            20,
            vec![],
            path_env.clone(),
            dir.path().to_path_buf(),
            "s".into(),
        ),
    ))));
    let hooks = Arc::new(HookRunner::new(
        &Hooks::default(),
        path_env,
        dir.path().to_path_buf(),
        "01SESSION".into(),
        Some(dir.path().join("logs/hooks")),
        None,
    ));
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let mut def = found.agents[&"explore".parse::<AgentName>().unwrap()].clone();
    def.permission_mode = PermissionMode::Bypass;
    let to_b = ModelSwitch {
        provider: b.clone(),
        creds: Arc::new(fake_credential()),
        model: ModelId("fake-small".into()),
        account: name("b"),
    };
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let run = RunId::new();
    let handle = spawn(
        RunConfig {
            run,
            def,
            prompt: "map this repo".into(),
            parent: None,
            depth: 0,
            model: ModelId("fake-small".into()),
            account: name("a"),
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
        },
        RunDeps {
            provider: a.clone(),
            creds: Arc::new(fake_credential()),
            tools,
            tx,
            hooks,
            compactor: None,
            assist: None,
            fallback: Some(Arc::new(Fallback {
                chain: vec![to_b],
                on: on.to_vec(),
                accounts: registry.clone(),
            })),
        },
    );
    let mut msgs = Vec::new();
    while let Some(m) = rx.recv().await {
        let last = m.name() == "RunFinished";
        msgs.push(m);
        if last {
            break;
        }
    }
    handle.join.await.unwrap();
    Done {
        a,
        b,
        registry,
        msgs,
        dir,
        run,
    }
}

const DEFAULT_ON: [FallbackKind; 3] = [
    FallbackKind::Quota,
    FallbackKind::Auth,
    FallbackKind::Overloaded,
];

#[tokio::test]
async fn a_quota_before_the_first_token_continues_on_the_next_account_with_the_same_conversation() {
    let done = run(
        script([err(ProviderErrorKind::Quota, "monthly cap")]),
        script([Step::Text("from b".into())]),
        &DEFAULT_ON,
    )
    .await;
    let r = done.finished();
    assert_eq!(
        (r.status, r.summary.as_str()),
        (RunStatus::Completed, "from b")
    );
    assert_eq!(done.switches(), [(name("a"), name("b"))]);
    let (first, second) = (done.a.requests(), done.b.requests());
    assert_eq!((first.len(), second.len()), (1, 1));
    assert_eq!(
        first[0].request.messages, second[0].request.messages,
        "b sees the same conversation"
    );
    // The transcript keeps the account the run began on and records the switch.
    let t = done.transcript();
    assert!(
        t.contains(r#""account":"a""#)
            && t.contains(r#""type":"switch""#)
            && t.contains(r#""account":"b""#),
        "{t}"
    );
    // The account that gave way sits out of selection.
    assert!(done.registry.cooled(&name("a")) && !done.registry.cooled(&name("b")));
}

#[tokio::test]
async fn a_quota_after_the_first_token_does_not_switch() {
    let done = run(
        script([
            Step::Text("partial".into()),
            err(ProviderErrorKind::Quota, "monthly cap"),
        ]),
        script([Step::Text("from b".into())]),
        &DEFAULT_ON,
    )
    .await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Quota);
    assert!(done.switches().is_empty());
    assert!(done.b.requests().is_empty());
    assert!(
        !done.registry.cooled(&name("a")),
        "only a switch cools an account"
    );
}

#[tokio::test]
async fn an_overload_retries_five_times_on_the_account_and_then_moves() {
    let a: Vec<Step> = (0..6)
        .map(|_| err(ProviderErrorKind::Overloaded, "busy"))
        .collect();
    let done = run(
        script(a),
        script([Step::Text("from b".into())]),
        &DEFAULT_ON,
    )
    .await;
    assert_eq!(done.finished().status, RunStatus::Completed);
    assert_eq!(done.a.requests().len(), 6, "the first try and five retries");
    assert_eq!(done.b.requests().len(), 1);
    assert_eq!(done.switches(), [(name("a"), name("b"))]);
}

#[tokio::test]
async fn fallback_on_without_overloaded_keeps_the_retry_in_place_and_the_end_as_before() {
    let a: Vec<Step> = (0..6)
        .map(|_| err(ProviderErrorKind::Overloaded, "busy"))
        .collect();
    let done = run(
        script(a),
        script([Step::Text("from b".into())]),
        &[FallbackKind::Quota],
    )
    .await;
    assert_eq!(done.finished().status, RunStatus::Failed);
    assert_eq!(done.a.requests().len(), 6);
    assert!(done.b.requests().is_empty() && done.switches().is_empty());
}

#[tokio::test]
async fn an_auth_failure_moves_on_and_the_account_stays_out_until_it_is_tested() {
    let done = run(
        script([err(ProviderErrorKind::Auth, "bad key")]),
        script([Step::Text("from b".into())]),
        &DEFAULT_ON,
    )
    .await;
    assert_eq!(done.finished().status, RunStatus::Completed);
    tokio::time::advance(Duration::from_secs(24 * 3600)).await;
    assert!(done.registry.cooled(&name("a")));
    done.registry.tested(&name("a"));
    assert!(!done.registry.cooled(&name("a")));
}

#[tokio::test]
async fn a_chain_that_is_used_up_ends_the_run_with_the_last_error() {
    let done = run(
        script([err(ProviderErrorKind::Quota, "a is spent")]),
        script([err(ProviderErrorKind::Quota, "b is spent")]),
        &DEFAULT_ON,
    )
    .await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Quota);
    assert_eq!(
        (done.a.requests().len(), done.b.requests().len()),
        (1, 1),
        "never back to a"
    );
}

// ---- the advisor and router seats ----------------------------------------------------------

fn request() -> aibus_provider::ChatRequest {
    aibus_provider::ChatRequest {
        model: ModelId("fake-small".into()),
        system: "advise".into(),
        messages: vec![Message::text(MessageRole::User, "is this safe?")],
        tools: Vec::new(),
        tool_choice: aibus_provider::ToolChoice::Auto,
        max_output_tokens: 256,
        temperature: None,
        effort: None,
        thinking: None,
        cache_hint: aibus_provider::CacheHint::AfterSystem,
        logprobs: false,
        metadata: aibus_provider::RequestMetadata::default(),
    }
}

#[tokio::test]
async fn a_seat_falls_back_without_the_caller_doing_anything_and_a_seat_without_a_chain_just_fails()
{
    let _clock = FakeClock::new();
    let (a, b) = (
        Arc::new(FakeProvider::new(script([err(
            ProviderErrorKind::Quota,
            "spent",
        )]))),
        Arc::new(FakeProvider::new(script([Step::Text("go ahead".into())]))),
    );
    let registry = registry();
    let to_b = ModelSwitch {
        provider: b.clone(),
        creds: Arc::new(fake_credential()),
        model: ModelId("fake-small".into()),
        account: name("b"),
    };
    let seat = Seat {
        provider: a.clone(),
        creds: Arc::new(fake_credential()),
        model: ModelId("fake-small".into()),
        account: name("a"),
        fallback: Some(Arc::new(Fallback {
            chain: vec![to_b],
            on: DEFAULT_ON.to_vec(),
            accounts: registry.clone(),
        })),
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let (resp, _) = aibus_core::assist::complete(&seat, request(), &cancel)
        .await
        .unwrap();
    assert!(format!("{:?}", resp.message).contains("go ahead"));
    assert_eq!((a.requests().len(), b.requests().len()), (1, 1));
    assert!(registry.cooled(&name("a")));

    // No chain: the error comes back as it did before F9, for the caller to report.
    let lone = Seat {
        provider: Arc::new(FakeProvider::new(script([err(
            ProviderErrorKind::Quota,
            "spent",
        )]))),
        fallback: None,
        ..seat
    };
    let e = aibus_core::assist::complete(&lone, request(), &cancel)
        .await
        .unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Quota);
}
