//! B1.3: the hardened run loop on the fake provider. Hook commands are real `sh -c` processes
//! in a temp project, so the tests that fire hooks run on the real clock; the retry and budget
//! tests have no hooks and run on a paused one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::config::Hooks;
use aibus_core::run::result::wrap;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{
    ContentBlock, ProviderError, ProviderErrorKind, StopReason, StreamChunk, ToolSpec,
};
use aibus_testkit::{fake_credential, FakeClock};
use serde_json::{json, Value};
use aibus_testkit::{xdg_temp, Xdg};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn path_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

/// A built-in definition, in `bypass` so that tool calls are not parked on a prompt, with
/// `hooks` as `(event, matcher, command)`.
fn def(name: &str, hooks: &[(&str, Option<&str>, &str)]) -> AgentDef {
    let dir = xdg_temp();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let mut d = found.agents[&name.parse::<AgentName>().unwrap()].clone();
    d.permission_mode = PermissionMode::Bypass;
    for (event, matcher, command) in hooks {
        d.hooks.entry((*event).into()).or_default().push(HookSpec {
            matcher: matcher.map(Into::into),
            command: (*command).into(),
            timeout_secs: None,
            kind: HookKind::Command,
        });
    }
    d
}

fn repo() -> Xdg {
    let d = xdg_temp();
    std::fs::write(d.path().join("a.txt"), "alpha\n").unwrap();
    d
}

fn runner(dir: &Path) -> Arc<HookRunner> {
    Arc::new(HookRunner::new(
        &Hooks::default(),
        path_env(),
        dir.to_path_buf(),
        "01SESSION".into(),
        Some(dir.join("logs/hooks")),
        None,
    ))
}

fn executor(dir: &Path) -> Arc<ToolExecutor> {
    let bash = Arc::new(Bash::new(20, vec![], path_env(), dir.to_path_buf(), "s".into()));
    Arc::new(ToolExecutor::new(ToolRegistry::standard(bash)))
}

fn err(kind: ProviderErrorKind, message: &str) -> Step {
    Step::Error(ProviderError::new(kind, message))
}

fn call(name: &str, input: Value) -> [Step; 2] {
    [
        Step::ToolCall {
            name: name.into(),
            input,
        },
        Step::Stop(StopReason::ToolUse),
    ]
}

fn script(steps: impl IntoIterator<Item = Step>) -> Script {
    steps.into_iter().fold(Script::new(), Script::then)
}

struct Plan {
    cfg: RunConfig,
    deps: RunDeps,
    fake: Arc<FakeProvider>,
    dir: Xdg,
}

fn plan(script: Script, def: AgentDef) -> (Plan, mpsc::Receiver<Msg>) {
    let fake = Arc::new(FakeProvider::new(script));
    let (tx, rx) = mpsc::channel(MSG_CAPACITY);
    let dir = repo();
    let p = Plan {
        cfg: RunConfig {
            run: RunId::new(),
            def,
            prompt: "map this repo".into(),
            parent: None,
            depth: 0,
            model: ModelId("fake-small".into()),
            account: AccountName("fake".into()),
            cwd: dir.path().to_path_buf(),
            runs_dir: dir.path().join("runs"),
            limits: ToolLimits::default(),
            budgets: Budgets::default(),
            date: "2026-11-08".into(),
            data_home: None,
            context: Vec::new(),
            background: false,
            max_spawn_depth: 3,
            forced: None,
            start: Start::New,
            worktree: None,
        },
        deps: RunDeps {
            provider: fake.clone(),
            creds: Arc::new(fake_credential()),
            tools: executor(dir.path()),
            tx,
            hooks: runner(dir.path()),
            compactor: None,
            assist: None,
        },
        fake,
        dir,
    };
    (p, rx)
}

struct Done {
    fake: Arc<FakeProvider>,
    msgs: Vec<Msg>,
    dir: Xdg,
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

    fn count(&self, name: &str) -> usize {
        self.msgs.iter().filter(|m| m.name() == name).count()
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }

    /// The tool results the model was shown on request `n`.
    fn results(&self, n: usize) -> Vec<String> {
        let req = &self.fake.requests()[n].request;
        req.messages
            .iter()
            .flat_map(|m| &m.blocks)
            .filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    fn waiting(&self) -> Vec<String> {
        self.msgs
            .iter()
            .filter_map(|m| match m {
                Msg::Stream { chunks, .. } => Some(chunks),
                _ => None,
            })
            .flatten()
            .filter_map(|c| match c {
                StreamChunk::Waiting(w) => Some(w.clone()),
                _ => None,
            })
            .collect()
    }
}

async fn collect(p: Plan, mut rx: mpsc::Receiver<Msg>, before: impl FnOnce(&RunHandle)) -> Done {
    let Plan {
        cfg, deps, fake, dir,
    } = p;
    let handle = spawn(cfg, deps);
    before(&handle);
    let mut msgs = Vec::new();
    while let Some(m) = rx.recv().await {
        let last = m.name() == "RunFinished";
        msgs.push(m);
        if last {
            break;
        }
    }
    handle.join.await.unwrap();
    Done { fake, msgs, dir }
}

async fn run(script: Script, def: AgentDef) -> Done {
    let (p, rx) = plan(script, def);
    collect(p, rx, |_| {}).await
}

// ---- hooks around tool calls ------------------------------------------------------------

#[tokio::test]
async fn pre_tool_use_exit_two_blocks_the_call_and_the_model_reads_the_stderr() {
    let d = def("general", &[("pre_tool_use", Some("bash"), "echo psql is off limits >&2; exit 2")]);
    let s = script(call("bash", json!({"command": "touch marker"})).into_iter().chain([Step::Text("ok".into())]));
    let done = run(s, d).await;

    assert!(!done.dir.path().join("marker").exists(), "the command never ran");
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Completed);
    let seen = done.results(1);
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("psql is off limits"), "{}", seen[0]);
    let finished = done.msgs.iter().find_map(|m| match m {
        Msg::ToolCallFinished { result, .. } => Some(result),
        _ => None,
    });
    assert!(finished.unwrap().is_error);
    let fired: Vec<_> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::HookFired { event, outcome, .. } => Some((*event, outcome.exit_code)),
            _ => None,
        })
        .collect();
    assert_eq!(fired, [(HookEvent::PreToolUse, 2)]);
}

#[tokio::test]
async fn updated_input_replaces_the_arguments_before_the_tool_runs() {
    let d = def(
        "general",
        &[("pre_tool_use", Some("bash"), r#"echo '{"updated_input":{"command":"echo changed > out.txt"}}'"#)],
    );
    let s = script(call("bash", json!({"command": "touch marker"})).into_iter().chain([Step::Text("ok".into())]));
    let done = run(s, d).await;
    assert!(!done.dir.path().join("marker").exists());
    assert_eq!(done.file("out.txt").trim(), "changed");
    let requested = done.msgs.iter().find_map(|m| match m {
        Msg::ToolCallRequested { call, .. } => Some(call.input.clone()),
        _ => None,
    });
    assert_eq!(requested.unwrap()["command"], "echo changed > out.txt");
}

#[tokio::test]
async fn post_tool_use_stderr_and_failures_reach_the_model() {
    let d = def(
        "general",
        &[
            ("post_tool_use", Some("bash"), "echo lint failed >&2; exit 2"),
            ("post_tool_use_failure", Some("read"), "cat > failure.json; echo check the path >&2; exit 2"),
        ],
    );
    let s = script(
        call("bash", json!({"command": "echo hi"}))
            .into_iter()
            .chain(call("read", json!({"path": "missing.txt"})))
            .chain([Step::Text("ok".into())]),
    );
    let done = run(s, d).await;
    let seen = done.results(2);
    assert!(seen[0].contains("hi") && seen[0].contains("[hook echo] lint failed"), "{}", seen[0]);
    assert!(seen[1].contains("[hook cat] check the path"), "{}", seen[1]);
    let v: Value = serde_json::from_str(&done.file("failure.json")).unwrap();
    assert_eq!(v["hook_event_name"], "post_tool_use_failure");
    assert_eq!(v["tool_name"], "read");
    assert!(v["error"].as_str().unwrap().len() > 3);
}

#[tokio::test]
async fn a_hook_ask_prompts_once_even_when_the_executor_would_ask_too() {
    let mut d = def("general", &[("pre_tool_use", Some("bash"), r#"echo '{"decision":"ask"}'"#)]);
    d.permission_mode = PermissionMode::Default;
    let (mut p, rx) = plan(
        script(call("bash", json!({"command": "echo asked"})).into_iter().chain([Step::Text("ok".into())])),
        d,
    );
    // A terminal is present, so the prompt waits for an answer.
    let bash = Arc::new(Bash::new(20, vec![], path_env(), p.dir.path().to_path_buf(), "s".into()));
    let gate = Gate { tty: true, ..Gate::default() };
    p.deps.tools = Arc::new(ToolExecutor::with_gate(ToolRegistry::standard(bash), gate).unwrap());
    let Plan { cfg, deps, fake, dir } = p;
    let mut rx = rx;
    let handle = spawn(cfg, deps);
    let mut msgs = Vec::new();
    let mut asked = 0;
    while let Some(m) = rx.recv().await {
        if let Msg::PermissionNeeded { request } = &m {
            asked += 1;
            handle
                .mailbox
                .send(RunMail::Permission { id: request.id, decision: Decision::Allow })
                .await
                .unwrap();
        }
        let last = m.name() == "RunFinished";
        msgs.push(m);
        if last {
            break;
        }
    }
    let done = Done { fake, msgs, dir };
    assert_eq!(asked, 1);
    assert_eq!(done.finished().status, RunStatus::Completed);
    assert!(done.results(1)[0].contains("asked"));
}

// ---- hooks around the run ---------------------------------------------------------------

#[tokio::test]
async fn the_prompt_hook_adds_context_and_a_block_denies_the_run() {
    let d = def("lead", &[("user_prompt_submit", None, "echo remember the milk")]);
    let done = run(script([Step::Text("ok".into())]), d).await;
    let first = &done.fake.requests()[0].request.messages[0];
    let texts: Vec<String> = first
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.last().unwrap(), "map this repo", "the task stays last");
    assert!(texts.iter().any(|t| t == "[hook echo]\nremember the milk"));

    let d = def("lead", &[("user_prompt_submit", None, "echo not today >&2; exit 2")]);
    let done = run(script([Step::Text("never".into())]), d).await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Denied);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Denied);
    assert!(r.error.as_ref().unwrap().message.contains("not today"));
    assert_eq!(done.fake.requests().len(), 0);
}

#[tokio::test]
async fn session_start_text_goes_in_front_of_the_task_and_mailed_prompts_pass_the_hook() {
    let d = def(
        "lead",
        &[("user_prompt_submit", None, "grep -q forbidden && { echo no >&2; exit 2; }; exit 0")],
    );
    let (mut p, rx) = plan(script(call("ls", json!({})).into_iter().chain([Step::Text("ok".into())])), d);
    p.cfg.context = vec!["[hook load-context.sh]\nbranch: main".into()];
    let done = collect(p, rx, |h| {
        h.mailbox.try_send(RunMail::UserMessage("forbidden thing".into())).unwrap();
        h.mailbox.try_send(RunMail::UserMessage("fine thing".into())).unwrap();
    })
    .await;
    let req = &done.fake.requests()[0].request;
    let all: Vec<String> = req.messages.iter().map(|m| m.plain_text()).collect();
    assert!(all[0].contains("branch: main") && all[0].ends_with("map this repo"));
    assert!(all.iter().any(|t| t == "fine thing"));
    assert!(!all.iter().any(|t| t.contains("forbidden")));
}

#[tokio::test]
async fn a_lead_stop_hook_that_exits_two_forces_another_turn() {
    let d = def(
        "lead",
        &[("stop", None, "cat > stop.json; [ -f seen ] && exit 0; touch seen; echo keep going >&2; exit 2")],
    );
    let s = script([
        Step::Text("first".into()),
        Step::Stop(StopReason::EndTurn),
        Step::Text("second".into()),
    ]);
    let done = run(s, d).await;
    let r = done.finished();
    assert_eq!((r.status, r.turns, r.summary.as_str()), (RunStatus::Completed, 2, "second"));
    let last = done.fake.requests()[1].request.messages.last().unwrap().plain_text();
    assert!(last.contains("keep going"), "{last}");
    let v: Value = serde_json::from_str(&done.file("stop.json")).unwrap();
    assert_eq!(v["hook_event_name"], "stop");
    assert_eq!(v["last_assistant_message"], "second");
}

#[tokio::test]
async fn a_workers_stop_hook_is_subagent_stop_and_exit_two_keeps_it_running() {
    let d = def(
        "general",
        &[
            ("stop", None, "[ -f seen ] && exit 0; touch seen; echo not yet >&2; exit 2"),
            ("subagent_start", Some("general"), "cat > start.json"),
        ],
    );
    let s = script([
        Step::Text("draft".into()),
        Step::Stop(StopReason::EndTurn),
        Step::Text("final".into()),
    ]);
    let done = run(s, d).await;
    assert_eq!(done.finished().summary, "final");
    assert_eq!(done.fake.requests().len(), 2);
    let events: Vec<_> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::HookFired { event, .. } => Some(event.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(events, ["subagent_start", "subagent_stop", "subagent_stop"]);
    let v: Value = serde_json::from_str(&done.file("start.json")).unwrap();
    assert_eq!(v["hook_event_name"], "subagent_start");
    assert_eq!(v["role"], "worker");
}

#[tokio::test]
async fn hooks_do_not_run_when_the_runner_is_the_default_one() {
    let d = def("general", &[("pre_tool_use", None, "touch ran")]);
    let (mut p, rx) = plan(script(call("ls", json!({})).into_iter().chain([Step::Text("ok".into())])), d);
    p.deps.hooks = Arc::new(HookRunner::default());
    let done = collect(p, rx, |_| {}).await;
    assert_eq!(done.finished().status, RunStatus::Completed);
    assert!(!done.dir.path().join("ran").exists());
    assert_eq!(done.count("HookFired"), 0);
}

// ---- retries ----------------------------------------------------------------------------

/// Virtual time to within the millisecond tokio rounds each sleep up by.
fn about(got: Duration, want: Duration) {
    assert!(got >= want && got < want + Duration::from_millis(50), "{got:?} vs {want:?}");
}

fn servers(n: usize) -> Vec<Step> {
    (0..n).map(|_| err(ProviderErrorKind::Server, "upstream fell over")).collect()
}

#[tokio::test]
async fn five_retries_then_success_on_the_documented_backoff() {
    let clock = FakeClock::new();
    let s = script(servers(5).into_iter().chain([Step::Text("made it".into())]));
    let done = run(s, def("explore", &[])).await;
    let r = done.finished();
    assert_eq!((r.status, r.summary.as_str()), (RunStatus::Completed, "made it"));
    assert_eq!(done.fake.requests().len(), 6);
    // 500 ms, then 1, 2, 4 and 8 s.
    about(clock.elapsed(), Duration::from_millis(15_500));
    let waits = done.waiting();
    assert!(waits[0].starts_with("retrying in 1 s (1/5)"), "{waits:?}");
    assert!(waits.iter().filter(|w| !w.is_empty()).count() == 5 && waits.last().unwrap().is_empty());
}

#[tokio::test]
async fn retry_after_is_honoured_and_a_huge_one_is_capped() {
    let clock = FakeClock::new();
    let mut slow = ProviderError::new(ProviderErrorKind::RateLimit, "slow down");
    slow.retry_after = Some(Duration::from_secs(20));
    let done = run(script([Step::Error(slow), Step::Text("ok".into())]), def("explore", &[])).await;
    assert_eq!(done.finished().status, RunStatus::Completed);
    about(clock.elapsed(), Duration::from_secs(20));

    let before = clock.elapsed();
    let mut huge = ProviderError::new(ProviderErrorKind::RateLimit, "come back tomorrow");
    huge.retry_after = Some(Duration::from_secs(86_400));
    let done = run(script([Step::Error(huge), Step::Text("ok".into())]), def("explore", &[])).await;
    assert_eq!(done.finished().status, RunStatus::Completed);
    about(clock.elapsed() - before, Duration::from_secs(300));
}

#[tokio::test]
async fn the_sixth_failure_fails_the_run_as_a_provider_error() {
    let clock = FakeClock::new();
    let done = run(script(servers(6)), def("explore", &[])).await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    let e = r.error.as_ref().unwrap();
    assert_eq!(e.kind, RunErrorKind::Provider);
    assert_eq!(e.message, "run ended early: upstream fell over");
    assert_eq!(done.fake.requests().len(), 6);
    about(clock.elapsed(), Duration::from_millis(15_500));
    assert_eq!(done.count("RunFailed"), 1);
}

#[tokio::test]
async fn a_non_retryable_error_fails_at_once() {
    let clock = FakeClock::new();
    let done = run(
        script([err(ProviderErrorKind::InvalidRequest, "bad schema"), Step::Text("never".into())]),
        def("explore", &[]),
    )
    .await;
    assert_eq!(done.finished().status, RunStatus::Failed);
    assert_eq!(done.fake.requests().len(), 1);
    assert_eq!(clock.elapsed(), Duration::ZERO);
}

#[tokio::test]
async fn auth_never_retries_and_commits_the_account_banner() {
    let clock = FakeClock::new();
    let banner = "account fake: 401, run aibus account test fake";
    let done = run(
        script([err(ProviderErrorKind::Auth, "401"), Step::Text("never".into())]),
        def("explore", &[]),
    )
    .await;
    assert_eq!(done.fake.requests().len(), 1);
    assert_eq!(clock.elapsed(), Duration::ZERO);
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    assert_eq!(r.error.as_ref().unwrap().message, banner);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Auth);
    let failed = done.msgs.iter().find_map(|m| match m {
        Msg::RunFailed { error, .. } => Some(error.message.clone()),
        _ => None,
    });
    assert_eq!(failed.as_deref(), Some(banner));
}

#[tokio::test]
async fn quota_never_retries_and_ends_with_a_banner_that_names_the_account() {
    let clock = FakeClock::new();
    let done = run(
        script([err(ProviderErrorKind::Quota, "insufficient_quota"), Step::Text("never".into())]),
        def("explore", &[]),
    )
    .await;
    assert_eq!(done.fake.requests().len(), 1);
    assert_eq!(clock.elapsed(), Duration::ZERO);
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    let e = r.error.as_ref().unwrap();
    assert_eq!(e.kind, RunErrorKind::Quota);
    assert!(!e.retryable);
    assert!(e.message.starts_with("account fake: quota or billing cap reached"), "{}", e.message);
    assert!(e.message.contains("aibus account test fake"), "{}", e.message);
    assert_eq!(done.count("RunFailed"), 1);
}

#[tokio::test]
async fn a_content_filter_never_retries_and_does_not_blame_the_account() {
    let clock = FakeClock::new();
    let done = run(
        script([err(ProviderErrorKind::ContentFilter, "blocked by policy"), Step::Text("never".into())]),
        def("explore", &[]),
    )
    .await;
    assert_eq!(done.fake.requests().len(), 1);
    assert_eq!(clock.elapsed(), Duration::ZERO);
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    let e = r.error.as_ref().unwrap();
    assert_eq!(e.kind, RunErrorKind::ContentFilter);
    assert!(!e.retryable);
    assert!(e.message.contains("content filter") && e.message.contains("blocked by policy"), "{}", e.message);
    assert!(!e.message.contains("account"), "{}", e.message);
    assert_eq!(done.count("RunFailed"), 1);
}

#[tokio::test]
async fn a_run_with_text_keeps_it_when_the_provider_gives_up() {
    let _clock = FakeClock::new();
    let s = script(
        [Step::Text("half an answer".into())]
            .into_iter()
            .chain(call("ls", json!({})))
            .chain(servers(6)),
    );
    let done = run(s, def("explore", &[])).await;
    let r = done.finished();
    assert_eq!((r.status, r.summary.as_str()), (RunStatus::Partial, "half an answer"));
    assert!(r.error.as_ref().unwrap().message.starts_with("run ended early"));
}

// ---- compaction -------------------------------------------------------------------------

struct Counting(AtomicU32);

#[async_trait::async_trait]
impl Compactor for Counting {
    async fn compact(&self, conv: &mut Conversation, _: &CancellationToken) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        conv.compact_boundary = Some(0);
        Ok(())
    }
}

#[tokio::test]
async fn context_too_long_compacts_once_then_goes_on() {
    let _clock = FakeClock::new();
    let d = def("explore", &[]);
    let long = || err(ProviderErrorKind::ContextTooLong, "too many tokens");

    let (mut p, rx) = plan(script([long(), Step::Text("fits now".into())]), d.clone());
    let compactor = Arc::new(Counting(AtomicU32::new(0)));
    p.deps.compactor = Some(compactor.clone());
    let done = collect(p, rx, |_| {}).await;
    assert_eq!(done.finished().summary, "fits now");
    assert_eq!(compactor.0.load(Ordering::SeqCst), 1);
    assert_eq!(done.count("Compacted"), 1);

    // A second one in a row is a failure: it compacts once per request.
    let (mut p, rx) = plan(script([long(), long(), Step::Text("never".into())]), d.clone());
    let compactor = Arc::new(Counting(AtomicU32::new(0)));
    p.deps.compactor = Some(compactor.clone());
    let done = collect(p, rx, |_| {}).await;
    assert_eq!(done.finished().status, RunStatus::Failed);
    assert_eq!(compactor.0.load(Ordering::SeqCst), 1);
    assert_eq!(done.fake.requests().len(), 2);

    // Nothing to compact with: a failure, not a loop.
    let done = run(script([long(), Step::Text("never".into())]), d).await;
    assert_eq!(done.finished().status, RunStatus::Failed);
    assert_eq!(done.fake.requests().len(), 1);
}

#[tokio::test]
async fn compact_mail_fires_pre_compact_and_compacts_at_the_next_boundary() {
    let d = def("explore", &[("pre_compact", Some("manual"), "cat > precompact.json")]);
    let (mut p, rx) = plan(script([Step::Text("ok".into())]), d);
    let compactor = Arc::new(Counting(AtomicU32::new(0)));
    p.deps.compactor = Some(compactor.clone());
    let done = collect(p, rx, |h| h.mailbox.try_send(RunMail::Compact).unwrap()).await;
    assert_eq!(compactor.0.load(Ordering::SeqCst), 1);
    assert_eq!(done.count("Compacted"), 1);
    let v: Value = serde_json::from_str(&done.file("precompact.json")).unwrap();
    assert_eq!(v["hook_event_name"], "pre_compact");
    assert_eq!(v["trigger"], "manual");
    assert!(v["pre_tokens"].is_number());
}

// ---- budgets and usage ------------------------------------------------------------------

fn usage(input: u64, output: u64) -> Step {
    Step::Usage { input, output }
}

#[tokio::test]
async fn max_tokens_total_ends_the_run_partial_before_the_next_tool_runs() {
    let _clock = FakeClock::new();
    let mut d = def("explore", &[]);
    d.max_tokens_total = Some(1_000);
    let s = script(
        [Step::Text("looking".into()), usage(600, 500)]
            .into_iter()
            .chain(call("ls", json!({})))
            .chain([Step::Text("never".into())]),
    );
    let done = run(s, d).await;
    let r = done.finished();
    assert_eq!((r.status, r.summary.as_str()), (RunStatus::Partial, "looking"));
    let e = r.error.as_ref().unwrap();
    assert_eq!(e.kind, RunErrorKind::Budget);
    assert!(e.message.contains("max_tokens_total"), "{}", e.message);
    assert_eq!(done.fake.requests().len(), 1);
    assert_eq!(done.count("ToolCallRequested"), 0, "the tool did not run");
}

#[tokio::test]
async fn max_cost_usd_ends_the_run_with_a_budget_error() {
    let _clock = FakeClock::new();
    let mut d = def("explore", &[]);
    d.max_cost_usd = Some(0.5);
    // The fake prices input at $1 per million tokens.
    let s = script(
        [Step::Text("pricey".into()), usage(1_000_000, 0)]
            .into_iter()
            .chain(call("ls", json!({})))
            .chain([Step::Text("never".into())]),
    );
    let done = run(s, d).await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Partial);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Budget);
    assert!(r.error.as_ref().unwrap().message.contains("max_cost_usd"));
    assert!((r.usage.cost_usd - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn a_final_answer_is_kept_even_when_it_used_up_the_budget() {
    let _clock = FakeClock::new();
    let mut d = def("explore", &[]);
    d.max_tokens_total = Some(10);
    let done = run(script([Step::Text("done".into()), usage(500, 500)]), d).await;
    assert_eq!(done.finished().status, RunStatus::Completed);
}

#[tokio::test]
async fn the_config_budgets_apply_when_the_definition_sets_none() {
    let _clock = FakeClock::new();
    let (mut p, rx) = plan(
        script(
            [Step::Text("a".into())]
                .into_iter()
                .chain(call("ls", json!({})))
                .chain([Step::Text("b".into())])
                .chain(call("ls", json!({})))
                .chain([Step::Text("c".into())]),
        ),
        def("explore", &[]),
    );
    p.cfg.budgets.max_turns = 2;
    let done = collect(p, rx, |_| {}).await;
    let r = done.finished();
    assert_eq!((r.status, r.turns, r.summary.as_str()), (RunStatus::Partial, 2, "b"));
}

#[tokio::test]
async fn a_usage_update_follows_every_turn_and_the_last_one_is_the_total() {
    let _clock = FakeClock::new();
    let s = script(
        [usage(100, 10)]
            .into_iter()
            .chain(call("ls", json!({})))
            .chain([Step::Text("done".into()), usage(200, 20)]),
    );
    let done = run(s, def("explore", &[])).await;
    let totals: Vec<u64> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::UsageUpdate { usage, .. } => Some(usage.input_tokens + usage.output_tokens),
            _ => None,
        })
        .collect();
    assert_eq!(totals, [110, 330]);
    let r = done.finished();
    assert_eq!(r.usage.input_tokens + r.usage.output_tokens, 330);
}

// ---- results ----------------------------------------------------------------------------

#[tokio::test]
async fn a_finished_run_drops_its_conversation() {
    // The task owns the only `Conversation`; once `RunFinished` is out the task has ended, so
    // the join resolves and nothing is left holding it.
    let _clock = FakeClock::new();
    let (p, rx) = plan(script([Step::Text("ok".into())]), def("explore", &[]));
    let done = collect(p, rx, |_| {}).await;
    assert_eq!(done.count("RunFinished"), 1);
}

/// Stands in for `delegate` (B2.1): runs a child to its end and hands back the wrapped result.
struct Relay {
    fake: Arc<FakeProvider>,
    def: AgentDef,
    tools: Arc<ToolExecutor>,
    dir: PathBuf,
    depth: u8,
}

#[async_trait::async_trait]
impl Tool for Relay {
    fn name(&self) -> &'static str {
        "delegate"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate".into(),
            description: "run a child".into(),
            schema: json!({"type": "object"}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, _: Value) -> Result<ToolOutput, ToolError> {
        let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
        let handle = spawn(
            RunConfig {
                run: RunId::new(),
                def: self.def.clone(),
                prompt: "go".into(),
                parent: Some(cx.run),
                depth: self.depth,
                model: ModelId("fake-small".into()),
                account: AccountName("fake".into()),
                cwd: self.dir.clone(),
                runs_dir: self.dir.join("runs"),
                limits: ToolLimits::default(),
                budgets: Budgets::default(),
                date: "2026-11-08".into(),
                data_home: None,
                context: Vec::new(),
                background: false,
                max_spawn_depth: 3,
                forced: None,
                start: Start::New,
                worktree: None,
            },
            RunDeps {
                provider: self.fake.clone(),
                creds: Arc::new(fake_credential()),
                tools: self.tools.clone(),
                tx,
                hooks: Arc::new(HookRunner::default()),
                compactor: None,
                assist: None,
            },
        );
        let mut out = None;
        while let Some(m) = rx.recv().await {
            if let Msg::RunFinished { result, .. } = m {
                out = Some(result);
                break;
            }
        }
        let _ = handle.join.await;
        Ok(ToolOutput {
            content: wrap(&out.expect("the child finishes")),
            ..Default::default()
        })
    }
}

fn with_relay(relay: Relay) -> Arc<ToolExecutor> {
    let mut reg = ToolRegistry::read_only();
    reg.register(Arc::new(relay));
    Arc::new(ToolExecutor::new(reg))
}

#[tokio::test]
async fn a_grandparent_sees_only_what_the_parent_chose_to_say() {
    let _clock = FakeClock::new();
    let dir = repo();
    let none = || Arc::new(ToolExecutor::new(ToolRegistry::read_only()));

    let grandchild = Arc::new(FakeProvider::new(script([Step::Text(
        "GRANDCHILD-WORDS found 4 modules".into(),
    )])));
    let parent_tools = with_relay(
        Relay {
            fake: grandchild,
            def: def("explore", &[]),
            tools: none(),
            dir: dir.path().to_path_buf(),
            depth: 2,
        },
    );
    let parent = Arc::new(FakeProvider::new(script(
        call("delegate", json!({"prompt": "go"})).into_iter().chain([Step::Text(
            "PARENT-WORDS the search is done".into(),
        )]),
    )));
    let lead_tools = with_relay(
        Relay {
            fake: parent.clone(),
            def: def("general", &[]),
            tools: parent_tools,
            dir: dir.path().to_path_buf(),
            depth: 1,
        },
    );
    let (mut p, rx) = plan(
        script(call("delegate", json!({"prompt": "go"})).into_iter().chain([Step::Text("all done".into())])),
        def("general", &[]),
    );
    p.deps.tools = lead_tools;
    let done = collect(p, rx, |_| {}).await;

    assert_eq!(done.finished().status, RunStatus::Completed);
    let top = done.results(1).join("\n");
    assert!(top.contains("PARENT-WORDS the search is done"), "{top}");
    assert!(top.contains("The following is the sub-agent's report."));
    assert!(!top.contains("GRANDCHILD-WORDS"), "{top}");
    // One level down, the parent did see its own child's words.
    let mid = parent.requests()[1]
        .request
        .messages
        .iter()
        .map(|m| m.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(mid.contains("GRANDCHILD-WORDS found 4 modules"), "{mid}");
}
