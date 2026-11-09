//! B2.3: the advisor's checkpoints and the router's three questions, on the fake provider. The
//! lead, the advisor and the router each get their own fake, so every script is its own.

use std::path::Path;
use std::sync::Arc;

use aibus_core::agent::discover::discover;
use aibus_core::config::Hooks;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{MessageRole, StopReason, ToolChoice};
use aibus_testkit::fake_credential;
use serde_json::{json, Value};
use aibus_testkit::{xdg_temp, Xdg};
use tokio::sync::mpsc;

fn path_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

fn def(name: &str, hooks: &[(&str, &str)]) -> AgentDef {
    let dir = xdg_temp();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let mut d = found.agents[&name.parse::<AgentName>().unwrap()].clone();
    d.permission_mode = PermissionMode::Bypass;
    for (event, command) in hooks {
        d.hooks.entry((*event).into()).or_default().push(HookSpec {
            matcher: None,
            command: (*command).into(),
            timeout_secs: None,
            kind: HookKind::Command,
        });
    }
    d
}

fn script(steps: impl IntoIterator<Item = Step>) -> Script {
    steps.into_iter().fold(Script::new(), Script::then)
}

fn call(name: &str, input: Value) -> [Step; 2] {
    [Step::ToolCall { name: name.into(), input }, Step::Stop(StopReason::ToolUse)]
}

fn say(text: &str) -> [Step; 2] {
    [Step::Text(text.into()), Step::Stop(StopReason::EndTurn)]
}

fn seat(fake: &Arc<FakeProvider>, account: &str) -> Seat {
    Seat {
        provider: fake.clone(),
        creds: Arc::new(fake_credential()),
        model: ModelId("fake-small".into()),
        account: AccountName(account.into()),
    }
}

struct World {
    lead: Arc<FakeProvider>,
    advisor: Arc<FakeProvider>,
    router: Arc<FakeProvider>,
    assist: Arc<Assist>,
    dir: Xdg,
    /// Where the runs' saved conversations go: outside the project, as in a real session.
    runs: Xdg,
    max_per_turn: u32,
}

struct Setup<'a> {
    lead: Script,
    advisor: Option<Script>,
    router: Option<Script>,
    hooks: &'a [(&'a str, &'a str)],
    max_per_turn: u32,
    prompt: &'a str,
    files: &'a [(&'a str, &'a str)],
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Self {
            lead: Script::new(),
            advisor: None,
            router: None,
            hooks: &[],
            max_per_turn: 10,
            prompt: "do the work",
            files: &[],
        }
    }
}

async fn go(s: Setup<'_>, before: impl FnOnce(&World)) -> (World, Vec<Msg>) {
    let dir = xdg_temp();
    for (name, text) in s.files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let lead = Arc::new(FakeProvider::new(s.lead));
    let (has_adv, has_rt) = (s.advisor.is_some(), s.router.is_some());
    let advisor = Arc::new(FakeProvider::new(s.advisor.unwrap_or_default()));
    let router = Arc::new(FakeProvider::new(s.router.unwrap_or_default()));
    let adv = has_adv.then(|| AdvisorSeat {
        seat: seat(&advisor, "adv"),
        def: def("advisor", &[]),
        enabled: ["before_plan", "same_error_x2", "before_done"].map(String::from).to_vec(),
        packet_max_tokens: 400_000,
    });
    let rt = has_rt.then(|| RouterSeat {
        seat: seat(&router, "rtr"),
        system: "route".into(),
        threshold: 0.85,
        max_per_turn: s.max_per_turn,
        label: "ROUTER".into(),
    });
    let assist = Assist::new(adv, rt);
    let world = World { lead: lead.clone(), advisor, router, assist: assist.clone(), dir, runs: xdg_temp(), max_per_turn: s.max_per_turn };
    before(&world);
    let bash = Arc::new(Bash::new(20, vec![], path_env(), world.dir.path().to_path_buf(), "s".into()));
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let cfg = RunConfig {
        run: RunId::new(),
        def: def("lead", s.hooks),
        prompt: s.prompt.into(),
        parent: None,
        depth: 0,
        model: ModelId("fake-small".into()),
        account: AccountName("fake".into()),
        cwd: world.dir.path().to_path_buf(),
        runs_dir: world.runs.path().to_path_buf(),
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
    };
    let deps = RunDeps {
        provider: lead,
        creds: Arc::new(fake_credential()),
        tools: Arc::new(ToolExecutor::new(ToolRegistry::standard(bash))),
        tx,
        hooks: Arc::new(HookRunner::new(
            &Hooks::default(),
            path_env(),
            world.dir.path().to_path_buf(),
            "01SESSION".into(),
            Some(world.dir.path().join("logs/hooks")),
            None,
        )),
        compactor: None,
        assist: Some(assist),
    };
    let handle = spawn(cfg, deps);
    let mut msgs = Vec::new();
    while let Some(m) = rx.recv().await {
        let last = m.name() == "RunFinished";
        msgs.push(m);
        if last {
            break;
        }
    }
    handle.join.await.unwrap();
    let _ = world.max_per_turn;
    (world, msgs)
}

fn count(msgs: &[Msg], name: &str) -> usize {
    msgs.iter().filter(|m| m.name() == name).count()
}

fn finished(msgs: &[Msg]) -> &RunResult {
    msgs.iter()
        .find_map(|m| match m {
            Msg::RunFinished { result, .. } => Some(result),
            _ => None,
        })
        .expect("a RunFinished")
}

fn answers(msgs: &[Msg]) -> Vec<(&RouterQuestion, &RouterAnswer)> {
    msgs.iter()
        .filter_map(|m| match m {
            Msg::RouterAnswered { question, answer, .. } => Some((question, answer)),
            _ => None,
        })
        .collect()
}

/// The system notes the lead's request `n` carried.
fn notes(w: &World, n: usize) -> Vec<String> {
    w.lead.requests()[n]
        .request
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::System)
        .map(|m| m.plain_text())
        .collect()
}

fn repo_has(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

// ---- advisor ---------------------------------------------------------------------------

#[tokio::test]
async fn before_plan_runs_the_advisor_and_the_next_request_carries_its_advice() {
    let s = Setup {
        lead: script(say("# Plan\n1. change auth").into_iter().chain(say("done"))),
        advisor: Some(script(say("plan skips auth"))),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    let hit = msgs.iter().find_map(|m| match m {
        Msg::CheckpointHit { checkpoint, .. } => Some(checkpoint.clone()),
        _ => None,
    });
    assert_eq!(hit, Some(Checkpoint::BeforePlan));
    let spoke = msgs.iter().find_map(|m| match m {
        Msg::AdvisorSpoke { advice, tokens_read, .. } => Some((advice.clone(), *tokens_read)),
        _ => None,
    });
    let (advice, read) = spoke.expect("AdvisorSpoke");
    assert_eq!(advice, "plan skips auth");
    assert!(read > 0);
    assert_eq!(notes(&w, 1), ["[advisor] plan skips auth"]);
    assert_eq!(finished(&msgs).status, RunStatus::Completed);
    // The advisor read the lead's text in full, and it has no editing tools.
    let req = &w.advisor.requests()[0];
    let packet = req.request.messages[0].plain_text();
    assert!(packet.contains("# Plan\n1. change auth"), "{packet}");
    let tools: Vec<String> = req.request.tools.iter().map(|t| t.name.to_string()).collect();
    for banned in ["delegate", "edit", "write", "bash"] {
        assert!(!tools.iter().any(|t| t == banned), "{tools:?}");
    }
    assert!(tools.iter().any(|t| t == "read"));
}

#[tokio::test]
async fn the_plan_tool_is_a_checkpoint_too() {
    let s = Setup {
        lead: script(call("plan", json!({"plan": "x"})).into_iter().chain(say("done"))),
        advisor: Some(script(say("split in 3 PRs"))),
        ..Setup::default()
    };
    let (_, msgs) = go(s, |_| {}).await;
    assert_eq!(count(&msgs, "CheckpointHit"), 1);
    assert_eq!(count(&msgs, "AdvisorSpoke"), 1);
}

#[tokio::test]
async fn same_error_x2_fires_once_for_a_pair_of_equal_failures() {
    let bad = |n: u32| call("read", json!({"path": format!("/nope/file{n}.txt")}));
    let s = Setup {
        lead: script(bad(1).into_iter().chain(bad(2)).chain(bad(3)).chain(say("giving up"))),
        advisor: Some(script(say("wrong fixture"))),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    let hits: Vec<_> = msgs
        .iter()
        .filter_map(|m| match m {
            Msg::CheckpointHit { checkpoint, .. } => Some(checkpoint.name().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(hits, ["same_error_x2"], "two failures make a pair, the third starts another");
    assert_eq!(count(&msgs, "AdvisorSpoke"), 1);
    assert_eq!(count(&msgs, "IoError"), 0);
    assert_eq!(notes(&w, 2), ["[advisor] wrong fixture"]);
}

#[tokio::test]
async fn a_checkpoint_hook_that_exits_two_skips_the_advisor() {
    let s = Setup {
        lead: script(say("# Plan\n1. x").into_iter().chain(say("done"))),
        advisor: Some(script(say("never heard"))),
        hooks: &[("checkpoint", "echo not now >&2; exit 2")],
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert_eq!(count(&msgs, "CheckpointHit"), 0);
    assert_eq!(count(&msgs, "AdvisorSpoke"), 0);
    assert!(w.advisor.requests().is_empty());
    let fired = msgs.iter().any(|m| {
        matches!(m, Msg::HookFired { event: HookEvent::Checkpoint, outcome, .. } if outcome.exit_code == 2)
    });
    assert!(fired);
    assert!(notes(&w, 0).is_empty());
}

#[tokio::test]
async fn before_done_needs_a_completed_worker() {
    let worker_done = |w: &World| {
        let run = RunId::new();
        w.assist.ledger.begin(run, &"search".parse().unwrap(), Role::Worker, "map it", 1);
        w.assist.ledger.finish(run, RunStatus::Completed);
    };
    let s = || Setup {
        lead: script(say("all done")),
        advisor: Some(script(say("run the migration"))),
        ..Setup::default()
    };
    let (_, msgs) = go(s(), |_| {}).await;
    assert_eq!(count(&msgs, "CheckpointHit"), 0, "no worker, no checkpoint");

    let mut with_worker = s();
    with_worker.lead = script(say("all done").into_iter().chain(say("fine")));
    let (w, msgs) = go(with_worker, worker_done).await;
    assert_eq!(count(&msgs, "CheckpointHit"), 1);
    assert_eq!(notes(&w, 1), ["[advisor] run the migration"]);
}

#[tokio::test]
async fn the_advisor_failing_does_not_stop_the_lead() {
    let s = Setup {
        lead: script(say("# Plan\n1. x").into_iter().chain(say("done"))),
        advisor: Some(Script::new()),
        ..Setup::default()
    };
    let (_, msgs) = go(s, |_| {}).await;
    assert_eq!(count(&msgs, "IoError"), 1);
    assert_eq!(count(&msgs, "AdvisorSpoke"), 1, "the pending checkpoint is cleared");
    assert_eq!(finished(&msgs).status, RunStatus::Completed);
}

#[test]
fn pending_is_set_once_per_checkpoint_and_cleared_by_the_advisor_spoke() {
    use aibus_core::state::CHECKPOINT_PENDING;
    use aibus_graph::mini::MiniGraph;
    use aibus_graph::{Graph, Value as V};
    use std::cell::RefCell;
    use std::rc::Rc;

    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let mut app = App::new(MiniGraph::new(), || 0);
    app.effects = Effects::new(move |e| {
        if let Effect::Advise { checkpoint } = e {
            sink.borrow_mut().push(checkpoint.to_string());
        }
    });
    let hit = || Msg::CheckpointHit { checkpoint: Checkpoint::SameErrorX2, detail: "x".into() };
    app.step(hit()).unwrap();
    assert_eq!(app.graph.read_key(CHECKPOINT_PENDING), Some(&V::Str("same_error_x2".into())));
    // Anything else that commits meanwhile starts nothing again.
    app.step(Msg::Tick).unwrap();
    app.step(hit()).unwrap();
    assert_eq!(*seen.borrow(), ["same_error_x2"]);
    app.step(Msg::AdvisorSpoke { checkpoint: Checkpoint::SameErrorX2, advice: "a".into(), tokens_read: 5 })
        .unwrap();
    assert_eq!(app.graph.read_key(CHECKPOINT_PENDING), Some(&V::Null));
    // The next pair is a new trigger.
    app.step(hit()).unwrap();
    assert_eq!(*seen.borrow(), ["same_error_x2", "same_error_x2"]);
}

#[test]
fn advisor_state_accumulates_tokens_read_and_calls() {
    use aibus_core::state::ADVISOR_STATE;
    use aibus_core::state::value::{as_i64, get};
    use aibus_graph::mini::MiniGraph;
    use aibus_graph::Graph;

    let mut app = App::new(MiniGraph::new(), || 0);
    for read in [122_000, 40_000] {
        app.step(Msg::AdvisorSpoke { checkpoint: Checkpoint::BeforePlan, advice: "a".into(), tokens_read: read })
            .unwrap();
    }
    let st = app.graph.read_key(ADVISOR_STATE).unwrap();
    assert_eq!(get(st, "calls").and_then(as_i64), Some(2));
    assert_eq!(get(st, "tokens_read").and_then(as_i64), Some(162_000));
}

#[test]
fn the_router_state_counts_forks_and_keeps_the_last_answer_per_kind() {
    use aibus_core::state::{router_last, ROUTER_FORKS};
    use aibus_core::state::value::{as_i64, as_str, get};
    use aibus_graph::mini::MiniGraph;
    use aibus_graph::Graph;

    let mut app = App::new(MiniGraph::new(), || 7);
    for (fork, source) in [(1, PSource::Logprob), (2, PSource::SelfReport)] {
        app.step(Msg::RouterAnswered {
            fork: ForkId(fork),
            question: RouterQuestion { kind: RouterKind::WhichFile, context: String::new(), choices: vec![] },
            answer: RouterAnswer {
                choice: "src/a.rs".into(),
                p: 0.94,
                verdict: Verdict::Sharp,
                latency: std::time::Duration::from_millis(120),
                source,
            },
        })
        .unwrap();
    }
    assert_eq!(app.graph.read_key(ROUTER_FORKS).and_then(as_i64), Some(2));
    let last = app.graph.read_key(&router_last("which_file")).unwrap();
    assert_eq!(get(last, "choice").and_then(as_str), Some("src/a.rs"));
    assert_eq!(get(last, "source").and_then(as_str), Some("self_report"));
    assert_eq!(get(last, "latency_ms").and_then(as_i64), Some(120));
}

// ---- router ----------------------------------------------------------------------------

fn pick(choice: &str, p: f32) -> Script {
    script([Step::RouterAnswer { choice: choice.into(), p }, Step::Stop(StopReason::EndTurn)])
}

/// A command that appends a line to `log` and then times out (an `Err` the router is asked
/// about).
fn hangs() -> Value {
    json!({"command": "echo run >> log; sleep 5", "timeout": 1})
}

#[tokio::test]
async fn a_sharp_retry_once_runs_the_failed_call_a_second_time() {
    let s = Setup {
        lead: script(call("bash", hangs()).into_iter().chain(say("ok"))),
        router: Some(pick("retry once", 0.95)),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert_eq!(repo_has(w.dir.path(), "log").lines().count(), 2);
    let a = answers(&msgs);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].0.kind, RouterKind::RetryOrStop);
    assert_eq!((a[0].1.verdict, a[0].1.source), (Verdict::Sharp, PSource::Logprob));
    assert_eq!(w.router.requests()[0].request.max_output_tokens, 16);
}

#[tokio::test]
async fn a_sharp_stop_and_ask_ends_the_turn_with_the_error() {
    let s = Setup {
        lead: script(call("bash", json!({"command": " "})).into_iter().chain(say("never"))),
        router: Some(pick("stop + ask", 0.9)),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    let r = finished(&msgs);
    assert_eq!(r.status, RunStatus::Partial);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Tool);
    assert!(r.error.as_ref().unwrap().message.contains("empty command"));
    assert_eq!(w.lead.requests().len(), 1);
}

#[tokio::test]
async fn a_split_sends_the_question_and_the_guess_to_the_lead_as_a_note() {
    let s = Setup {
        lead: script(call("bash", json!({"command": " "})).into_iter().chain(say("ok"))),
        router: Some(pick("stop + ask", 0.58)),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert_eq!(answers(&msgs)[0].1.verdict, Verdict::Split);
    let n = notes(&w, 1);
    assert_eq!(n.len(), 1);
    assert!(n[0].starts_with("[router] retry_or_stop"), "{n:?}");
    assert!(n[0].contains("stop + ask") && n[0].contains("split") && n[0].contains("0.58"), "{n:?}");
    assert_eq!(finished(&msgs).status, RunStatus::Completed);
}

#[tokio::test]
async fn which_tool_prefills_the_tool_choice_when_sharp() {
    let s = Setup {
        lead: script(
            say("Found the module. Now I'll look at the files.")
                .into_iter()
                .chain(call("ls", json!({"path": "."})))
                .chain(say("done")),
        ),
        router: Some(pick("ls", 0.95)),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert_eq!(answers(&msgs)[0].0.kind, RouterKind::WhichTool);
    let reqs = w.lead.requests();
    assert_eq!(reqs[1].request.tool_choice, ToolChoice::Named("ls".into()));
    assert_eq!(reqs[2].request.tool_choice, ToolChoice::Auto);
    assert!(notes(&w, 1)[0].contains("call ls next"));
    // The question offered the lead's tool names.
    let asked = w.router.requests()[0].request.messages[0].plain_text();
    assert!(asked.contains("- ls") && asked.contains("- grep"), "{asked}");
}

#[tokio::test]
async fn which_tool_split_leaves_the_choice_to_the_lead() {
    let s = Setup {
        lead: script(say("Let me check the logs").into_iter().chain(say("done"))),
        router: Some(pick("grep", 0.4)),
        ..Setup::default()
    };
    let (w, _) = go(s, |_| {}).await;
    let reqs = w.lead.requests();
    assert_eq!(reqs[1].request.tool_choice, ToolChoice::Auto);
    assert!(notes(&w, 1)[0].contains("best guess is grep"));
}

#[tokio::test]
async fn an_answer_is_not_an_intention_so_nobody_is_asked() {
    let s = Setup {
        lead: script(say("The tests pass.")),
        router: Some(pick("ls", 0.95)),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert!(answers(&msgs).is_empty());
    assert!(w.router.requests().is_empty());
}

#[tokio::test]
async fn which_file_pre_reads_the_file_when_sharp_and_asks_the_lead_when_split() {
    let files = [("a.rs", "fn parse_config() { /* real */ }"), ("b.rs", "use a::parse_config;")];
    let sharp = Setup {
        lead: script(say("ok")),
        router: Some(pick("a.rs", 0.97)),
        prompt: "fix parse_config please",
        files: &files,
        ..Setup::default()
    };
    let (w, msgs) = go(sharp, |_| {}).await;
    assert_eq!(answers(&msgs)[0].0.kind, RouterKind::WhichFile);
    assert_eq!(answers(&msgs)[0].0.choices, ["a.rs", "b.rs"]);
    let n = notes(&w, 0);
    assert!(n[0].contains("a.rs") && n[0].contains("fn parse_config() { /* real */ }"), "{n:?}");

    let split = Setup {
        lead: script(say("ok")),
        router: Some(pick("b.rs", 0.5)),
        prompt: "fix parse_config please",
        files: &files,
        ..Setup::default()
    };
    let (w, _) = go(split, |_| {}).await;
    let n = notes(&w, 0);
    assert!(n[0].contains("best guess is b.rs") && !n[0].contains("real"), "{n:?}");
}

#[tokio::test]
async fn the_per_turn_cap_stops_further_questions() {
    let two = [
        Step::ToolCall { name: "bash".into(), input: json!({"command": " "}) },
        Step::ToolCall { name: "bash".into(), input: json!({"command": "  "}) },
        Step::Stop(StopReason::ToolUse),
    ];
    let s = Setup {
        lead: script(two.into_iter().chain(say("ok"))),
        router: Some(pick("switch tool", 0.5)),
        max_per_turn: 1,
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert_eq!(answers(&msgs).len(), 1);
    assert_eq!(w.router.requests().len(), 1);
    assert_eq!(count(&msgs, "IoError"), 0);
}

#[tokio::test]
async fn a_router_that_cannot_answer_costs_the_lead_nothing() {
    let s = Setup {
        lead: script(call("bash", json!({"command": " "})).into_iter().chain(say("ok"))),
        router: Some(Script::new()),
        ..Setup::default()
    };
    let (w, msgs) = go(s, |_| {}).await;
    assert_eq!(count(&msgs, "IoError"), 1);
    assert!(notes(&w, 1).is_empty());
    assert_eq!(finished(&msgs).status, RunStatus::Completed);
}

#[tokio::test]
async fn results_that_are_not_errors_never_reach_the_router() {
    let s = Setup {
        lead: script(call("ls", json!({"path": "."})).into_iter().chain(say("ok"))),
        router: Some(pick("retry once", 0.9)),
        ..Setup::default()
    };
    let (_, msgs) = go(s, |_| {}).await;
    assert!(answers(&msgs).is_empty());
}
