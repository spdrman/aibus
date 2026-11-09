//! B2.1: delegation end to end on the fake provider. A real lead run, the `delegate` tool, the
//! orchestrator, the effects executor and a launcher that hands every child its own scripted
//! fake. Time is virtual (`FakeClock`), so the delays in the scripts cost nothing.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_graph::Graph;
use aibus_provider::fake::{FakeProvider, RecordedRequest, Script, Step};
use aibus_provider::{ContentBlock, MessageRole, StopReason};
use aibus_testkit::{fake_credential, FakeClock};
use common::{app, A};
use serde_json::{json, Value};
use aibus_testkit::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn path_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

fn text(t: &str) -> Step {
    Step::Text(t.into())
}

fn delay(secs: u64) -> Step {
    Step::Delay(Duration::from_secs(secs))
}

fn end() -> Step {
    Step::Stop(StopReason::EndTurn)
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

fn delegate_call(agent: &str, prompt: &str, background: bool) -> [Step; 2] {
    call(
        "delegate",
        json!({"agent": agent, "prompt": prompt, "background": background}),
    )
}

fn script(steps: impl IntoIterator<Item = Step>) -> Script {
    steps.into_iter().fold(Script::new(), Script::then)
}

fn defs() -> BTreeMap<AgentName, AgentDef> {
    let dir = TempDir::new().unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    found
        .agents
        .iter()
        .map(|(k, v)| {
            let mut d = v.clone();
            // Nothing in these tests may park on a prompt unless the test asks for one.
            if d.name.as_str() != "general" {
                d.permission_mode = PermissionMode::Bypass;
            }
            (k.clone(), d)
        })
        .collect()
}

/// Gives every child its own fake, scripted by what the spec asks for.
struct TestLauncher {
    make: Box<dyn Fn(&SpawnSpec) -> Script + Send + Sync>,
    defs: BTreeMap<AgentName, AgentDef>,
    tools: Arc<ToolExecutor>,
    dir: PathBuf,
    limits: ToolLimits,
    /// An agent that cannot be built.
    fail_for: Option<&'static str>,
    launched: Mutex<Vec<SpawnSpec>>,
    fakes: Mutex<HashMap<RunId, Arc<FakeProvider>>>,
}

impl Launcher for TestLauncher {
    fn launch(
        &self,
        spec: &SpawnSpec,
        prompt: String,
        tx: mpsc::Sender<Msg>,
    ) -> Result<(RunConfig, RunDeps), String> {
        if self.fail_for == Some(spec.agent.as_str()) {
            return Err("no provider is reachable".into());
        }
        let def = self.defs[&spec.agent].clone();
        let fake = Arc::new(FakeProvider::new((self.make)(spec)));
        self.launched.lock().unwrap().push(spec.clone());
        self.fakes.lock().unwrap().insert(spec.run, fake.clone());
        Ok((
            RunConfig {
                run: spec.run,
                def,
                prompt,
                parent: spec.parent,
                depth: spec.depth,
                model: ModelId("fake-small".into()),
                account: AccountName("fake".into()),
                cwd: self.dir.clone(),
                runs_dir: self.dir.join("runs"),
                limits: self.limits,
                budgets: Budgets::default(),
                date: "2026-11-08".into(),
                data_home: None,
                context: Vec::new(),
                background: spec.background,
                max_spawn_depth: 3,
                forced: None,
                start: Start::New,
                worktree: None,
            },
            RunDeps {
                provider: fake,
                creds: Arc::new(fake_credential()),
                tools: self.tools.clone(),
                tx,
                hooks: Arc::new(HookRunner::default()),
                compactor: None,
                assist: None,
            },
        ))
    }
}

struct Setup {
    limits: Limits,
    tool_limits: ToolLimits,
    tty: bool,
    defs: BTreeMap<AgentName, AgentDef>,
    fail_for: Option<&'static str>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            tool_limits: ToolLimits::default(),
            tty: false,
            defs: defs(),
            fail_for: None,
        }
    }
}

struct Rig {
    app: A,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    orch: Arc<Orchestrator>,
    launcher: Arc<TestLauncher>,
    dir: TempDir,
    lead: RunId,
    lead_fake: Arc<FakeProvider>,
}

struct Out {
    msgs: Vec<Msg>,
}

impl Out {
    fn position(&self, f: impl Fn(&Msg) -> bool) -> usize {
        self.msgs.iter().position(f).expect("the message was sent")
    }

    fn finished(&self, run: RunId) -> &RunResult {
        self.msgs
            .iter()
            .find_map(|m| match m {
                Msg::RunFinished { run: r, result } if *r == run => Some(result),
                _ => None,
            })
            .expect("a RunFinished")
    }
}

impl Rig {
    /// `lead` scripts the lead, `children` the runs it starts.
    fn new(
        set: Setup,
        lead: Script,
        children: impl Fn(&SpawnSpec) -> Script + Send + Sync + 'static,
    ) -> Self {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        let (tx, rx) = mpsc::channel(MSG_CAPACITY);
        let orch = Orchestrator::new(set.limits.clone(), tx.clone());
        let bash = Arc::new(Bash::new(20, vec![], path_env(), dir.path().to_path_buf(), "s".into()));
        let mut registry = ToolRegistry::standard(bash);
        registry.register(Arc::new(Delegate::new(orch.clone())));
        let gate = Gate {
            tty: set.tty,
            ..Gate::default()
        };
        let tools = Arc::new(ToolExecutor::with_gate(registry, gate).unwrap());
        let launcher = Arc::new(TestLauncher {
            make: Box::new(children),
            defs: set.defs.clone(),
            tools,
            dir: dir.path().to_path_buf(),
            limits: set.tool_limits,
            fail_for: set.fail_for,
            launched: Mutex::default(),
            fakes: Mutex::default(),
        });
        let mut app = app();
        app.effects = Effects::orchestrated(orch.clone(), launcher.clone(), app.store.clone(), |_| {});
        app.set_limits(&set.limits).unwrap();
        let all: Vec<AgentDef> = set.defs.values().cloned().collect();
        app.step(Msg::AgentsReloaded {
            defs: all.clone(),
            warnings: vec![],
        })
        .unwrap();
        orch.set_roster(Roster::of(all.iter()));
        Self {
            app,
            tx,
            rx,
            orch,
            launcher,
            dir,
            lead: RunId::new(),
            lead_fake: Arc::new(FakeProvider::new(lead)),
        }
    }

    /// The graph hears about the lead first, so the orchestrator's counts include it by the time
    /// the lead asks for a child.
    fn lead_started(&mut self) {
        self.app
            .step(Msg::RunStarted {
                run: self.lead,
                agent: "lead".parse().unwrap(),
                role: Role::Lead,
                parent: None,
                model: ModelId("fake-small".into()),
                account: AccountName("fake".into()),
            })
            .unwrap();
    }

    fn start_lead(&mut self, prompt: &str, forced: Option<(&str, &str)>) {
        self.lead_started();
        let handle = spawn(
            RunConfig {
                run: self.lead,
                def: defs()[&"lead".parse::<AgentName>().unwrap()].clone(),
                prompt: prompt.into(),
                parent: None,
                depth: 0,
                model: ModelId("fake-small".into()),
                account: AccountName("fake".into()),
                cwd: self.dir.path().to_path_buf(),
                runs_dir: self.dir.path().join("runs"),
                limits: self.launcher.limits,
                budgets: Budgets::default(),
                date: "2026-11-08".into(),
                data_home: None,
                context: Vec::new(),
                background: false,
                max_spawn_depth: 3,
                forced: forced.map(|(a, r)| {
                    ("delegate".into(), json!({"agent": a, "prompt": r, "background": false}))
                }),
                start: Start::New,
                worktree: None,
            },
            RunDeps {
                provider: self.lead_fake.clone(),
                creds: Arc::new(fake_credential()),
                tools: self.launcher.tools.clone(),
                tx: self.tx.clone(),
                hooks: Arc::new(HookRunner::default()),
                compactor: None,
                assist: None,
            },
        );
        self.orch
            .adopt(self.lead, handle.mailbox.clone(), handle.cancel.clone());
    }

    /// Steps every message into the app until the lead ends. `on` sees each one after the app
    /// has, and may step more.
    async fn pump(&mut self, mut on: impl FnMut(&mut A, &Orchestrator, &Msg)) -> Out {
        let mut msgs = Vec::new();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(3600), self.rx.recv())
                .await
                .expect("the session hung")
                .expect("the channel closed");
            self.app.step(msg.clone()).unwrap();
            on(&mut self.app, &self.orch, &msg);
            let done = matches!(&msg, Msg::RunFinished { run, .. } if *run == self.lead);
            msgs.push(msg);
            if done {
                return Out { msgs };
            }
        }
    }

    async fn run(&mut self, prompt: &str) -> Out {
        self.start_lead(prompt, None);
        self.pump(|_, _, _| {}).await
    }

    fn lead_requests(&self) -> Vec<RecordedRequest> {
        self.lead_fake.requests()
    }

    fn launched(&self) -> Vec<SpawnSpec> {
        self.launcher.launched.lock().unwrap().clone()
    }

    fn child_requests(&self, run: RunId) -> Vec<RecordedRequest> {
        self.launcher.fakes.lock().unwrap()[&run].requests()
    }

    fn status(&self, run: RunId) -> String {
        match self.app.graph.read_key(&format!("model:run:{run}:status")) {
            Some(aibus_graph::Value::Str(s)) => s.to_string(),
            other => panic!("no status for {run}: {other:?}"),
        }
    }
}

/// Every tool result the model saw in a request.
fn results(r: &RecordedRequest) -> Vec<String> {
    r.request
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

/// Every plain text block the model saw in a request, tool results aside.
fn user_text(r: &RecordedRequest) -> Vec<String> {
    r.request
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn tool_names(r: &RecordedRequest) -> Vec<String> {
    r.request.tools.iter().map(|t| t.name.to_string()).collect()
}

// ---- background -----------------------------------------------------------------------------

#[tokio::test]
async fn a_background_delegate_answers_at_once_and_the_report_arrives_wrapped_and_capped() {
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
    assert!(!user_text(&reqs[1]).iter().any(|t| t.contains("sub-agent's report")));

    // The report is in the next request: wrapped, and capped with a pointer to the full text.
    let report = user_text(&reqs[2])
        .into_iter()
        .find(|t| t.contains("sub-agent's report"))
        .expect("the report was injected");
    assert!(report.starts_with("The following is the sub-agent's report."), "{report}");
    assert!(report.contains(&format!("[run {child} · explore · completed · 1 turns")), "{report}");
    assert!(report.contains("summary cut at 64 bytes"), "{report}");
    let full = std::fs::read_to_string(rig.dir.path().join(format!("runs/{child}/result.md"))).unwrap();
    assert_eq!(full.len(), 200);
    assert!(report.contains(&full[..64]));
    assert!(!report.contains(&full[..65]));
}

#[tokio::test]
async fn a_foreground_delegate_blocks_the_parents_tool_call_until_the_child_ends() {
    let clock = FakeClock::new();
    let lead = script(
        delegate_call("explore", "find it", false)
            .into_iter()
            .chain([text("thanks")]),
    );
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
    assert!(result.contains(&format!("[run {child} · explore · completed")));
    assert_eq!(out.finished(rig.lead).turns, 2);
}

#[tokio::test]
async fn a_definition_with_background_true_overrides_a_caller_who_says_false() {
    let clock = FakeClock::new();
    let mut set = Setup::default();
    set.defs.get_mut(&"explore".parse::<AgentName>().unwrap()).unwrap().background = true;
    let lead = script(
        delegate_call("explore", "find it", false)
            .into_iter()
            .chain([text("carry on")]),
    );
    let mut rig = Rig::new(set, lead, |_| script([delay(60), text("late")]));
    let out = rig.run("go").await;

    let launched = rig.launched();
    assert!(launched[0].background, "the spec says background");
    let answer: Value = serde_json::from_str(&results(&rig.lead_requests()[1])[0]).unwrap();
    assert_eq!(answer, json!({ "run_id": launched[0].run.to_string() }));
    assert!(clock.elapsed() < Duration::from_secs(60), "the lead did not wait");
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);
}

#[tokio::test]
async fn ctrl_b_moves_a_foreground_wait_to_the_background() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("explore", "slow one", false)
            .into_iter()
            .chain([delay(10), text("moved on"), end(), text("final")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |_| script([delay(5), text("child report")]));
    rig.start_lead("go", None);
    let lead = rig.lead;
    let mut pressed = false;
    let out = rig
        .pump(|app, _, msg| {
            if let Msg::RunStarted { run, parent, .. } = msg {
                if *parent == Some(lead) && !pressed {
                    pressed = true;
                    app.step(Msg::Command(Command::Background { run: *run })).unwrap();
                }
            }
        })
        .await;

    assert!(pressed);
    let child = rig.launched()[0].run;
    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 3);
    let answer: Value = serde_json::from_str(&results(&reqs[1])[0]).unwrap();
    assert_eq!(answer, json!({ "run_id": child.to_string(), "background": true }));
    let report = user_text(&reqs[2]).join("\n");
    assert!(report.contains("child report"), "the result still came back: {report}");
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);
}

#[tokio::test]
async fn a_background_runs_permission_prompt_names_the_run_and_a_deny_lets_it_carry_on() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("general", "run it", true)
            .into_iter()
            .chain([delay(20), text("waiting"), end(), text("ok")]),
    );
    let set = Setup {
        tty: true,
        ..Setup::default()
    };
    let child_script = || {
        script(
            call("bash", json!({"command": "echo hi"}))
                .into_iter()
                .chain([text("done")]),
        )
    };
    let mut rig = Rig::new(set, lead, move |_| child_script());
    rig.start_lead("go", None);
    let mut asked = Vec::new();
    let out = rig
        .pump(|_, orch, msg| {
            if let Msg::PermissionNeeded { request } = msg {
                asked.push(request.clone());
                orch.mailbox(request.run)
                    .expect("the run is live")
                    .try_send(RunMail::Permission {
                        id: request.id,
                        decision: Decision::Deny,
                    })
                    .unwrap();
            }
        })
        .await;

    let child = rig.launched()[0].run;
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].agent.as_str(), "general");
    assert_eq!(asked[0].run, child);
    assert!(asked[0].summary.contains("echo hi"));

    // The call was refused, the run went on to its answer.
    let finished = out
        .msgs
        .iter()
        .find_map(|m| match m {
            Msg::ToolCallFinished { run, result, .. } if *run == child => Some(result.clone()),
            _ => None,
        })
        .expect("the bash call ended");
    assert!(finished.is_error);
    assert!(finished.content.starts_with("denied:"), "{}", finished.content);
    let r = out.finished(child);
    assert_eq!((r.status, r.summary.as_str()), (RunStatus::Completed, "done"));

    // A background run is not offered the interactive tools.
    assert!(rig.launched()[0].background);
    let names = tool_names(&rig.child_requests(child)[0]);
    for t in ["ask_user", "enter_plan_mode", "exit_plan_mode"] {
        assert!(!names.contains(&t.to_owned()), "{t} in {names:?}");
    }
    assert!(as_open(&rig.app).is_empty(), "the prompt was closed");
}

fn as_open(app: &A) -> Vec<aibus_graph::Value> {
    match app.graph.read_key("model:permissions:open") {
        Some(aibus_graph::Value::List(l)) => l.to_vec(),
        _ => vec![],
    }
}

// ---- limits ---------------------------------------------------------------------------------

#[tokio::test]
async fn past_the_concurrent_limit_the_tool_result_is_the_error() {
    let _clock = FakeClock::new();
    // Two delegate calls in one turn.
    let ask = |prompt: &str| Step::ToolCall {
        name: "delegate".into(),
        input: json!({"agent": "explore", "prompt": prompt}),
    };
    let lead = script([
        ask("a"),
        ask("b"),
        Step::Stop(StopReason::ToolUse),
        text("both asked"),
    ]);
    let set = Setup {
        limits: Limits {
            max_concurrent_runs: 2,
            ..Limits::default()
        },
        ..Setup::default()
    };
    let mut rig = Rig::new(set, lead, |_| script([delay(30), text("x")]));
    let out = rig.run("go").await;

    let reqs = rig.lead_requests();
    let got = results(&reqs[1]);
    assert_eq!(got.len(), 2);
    let first: Value = serde_json::from_str(&got[0]).unwrap();
    assert!(first["run_id"].is_string());
    assert_eq!(got[1], r#"{"error":"concurrent run limit reached (2)"}"#);
    assert_eq!(rig.launched().len(), 1, "only the first got a run");
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);
}

#[tokio::test]
async fn nesting_stops_at_depth_3_and_delegate_is_not_offered_there() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("general", "level 1", false)
            .into_iter()
            .chain([text("all done")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |spec| match spec.depth {
        1 => script(
            delegate_call("general", "level 2", false)
                .into_iter()
                .chain([text("one done")]),
        ),
        2 => script(
            delegate_call("general", "level 3", false)
                .into_iter()
                .chain([text("two done")]),
        ),
        _ => script([text("three done")]),
    });
    let out = rig.run("go").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    let launched = rig.launched();
    assert_eq!(launched.iter().map(|s| s.depth).collect::<Vec<_>>(), [1, 2, 3]);
    let offered = |i: usize| tool_names(&rig.child_requests(launched[i].run)[0]);
    assert!(offered(0).contains(&"delegate".to_owned()), "depth 1");
    assert!(offered(1).contains(&"delegate".to_owned()), "depth 2");
    assert!(!offered(2).contains(&"delegate".to_owned()), "depth 3: {:?}", offered(2));
    assert!(tool_names(&rig.lead_requests()[0]).contains(&"delegate".to_owned()));

    // Each level hears only from the one below it.
    let deepest = results(&rig.child_requests(launched[1].run)[1]).join("\n");
    assert!(deepest.contains("three done"));
    let lead_saw = results(&rig.lead_requests()[1]).join("\n");
    assert!(lead_saw.contains("one done"));
    assert!(!lead_saw.contains("three done"), "results do not bubble past one level");
}

#[tokio::test]
async fn a_run_that_cannot_be_built_ends_failed_and_the_waiting_parent_hears_it() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("plan", "think", false)
            .into_iter()
            .chain([text("ok then")]),
    );
    let set = Setup {
        fail_for: Some("plan"),
        ..Setup::default()
    };
    let mut rig = Rig::new(set, lead, |_| script([text("never")]));
    let out = rig.run("go").await;

    let seen = results(&rig.lead_requests()[1]).join("\n");
    assert!(seen.contains("· plan · failed"), "{seen}");
    assert!(seen.contains("no provider is reachable"), "{seen}");
    assert!(rig.launched().is_empty());
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);
    // The graph is told too, perhaps just after the lead's end.
    while let Ok(msg) = rig.rx.try_recv() {
        rig.app.step(msg).unwrap();
    }
    let order = rig.app.graph.read_key("model:runs:order").cloned().unwrap();
    let aibus_graph::Value::List(order) = order else {
        panic!("no run order")
    };
    let aibus_graph::Value::Str(child) = &order[1] else {
        panic!("no second run")
    };
    assert_eq!(rig.status(child.parse().unwrap()), "failed");
}

// ---- the executor -----------------------------------------------------------------------------

#[tokio::test]
async fn spawn_run_is_idempotent_and_moves_queued_to_starting() {
    let _clock = FakeClock::new();
    let mut rig = Rig::new(Setup::default(), Script::new(), |_| script([delay(100), text("x")]));
    rig.lead_started();
    rig.orch
        .adopt(rig.lead, mpsc::channel(8).0, CancellationToken::new());
    let child = RunId::new();
    rig.app
        .step(Msg::Command(Command::Delegate {
            agent: "explore".parse().unwrap(),
            prompt: "p".into(),
            background: true,
            isolation: Isolation::None,
            parent: Some(rig.lead),
            run: Some(child),
            model: None,
            account: None,
        }))
        .unwrap();
    assert_eq!(rig.status(child), "queued");
    assert_eq!(rig.launched().len(), 1, "started by the commit that listed it");

    // Reading the same listing again, or asking for the same id, starts nothing.
    rig.app.step(Msg::Tick).unwrap();
    rig.app.step(Msg::Tick).unwrap();
    let spec = rig.launched()[0].clone();
    rig.app.effects.spawn_run(spec);
    assert_eq!(rig.launched().len(), 1);

    // The run's own `RunStarted` is what moves it on.
    loop {
        let msg = rig.rx.recv().await.unwrap();
        let started = matches!(&msg, Msg::RunStarted { run, .. } if *run == child);
        rig.app.step(msg).unwrap();
        if started {
            break;
        }
    }
    assert_eq!(rig.status(child), "starting");
    assert_eq!(rig.launched().len(), 1);
}

#[tokio::test]
async fn a_run_whose_parent_is_not_live_is_cancelled_and_never_started() {
    let _clock = FakeClock::new();
    let mut rig = Rig::new(Setup::default(), Script::new(), |_| script([text("x")]));
    // The graph has a lead, but no live run behind it: it ended.
    rig.lead_started();
    let child = RunId::new();
    rig.app
        .step(Msg::Command(Command::Delegate {
            agent: "explore".parse().unwrap(),
            prompt: "p".into(),
            background: true,
            isolation: Isolation::None,
            parent: Some(rig.lead),
            run: Some(child),
            model: None,
            account: None,
        }))
        .unwrap();
    assert!(rig.launched().is_empty());
    let msg = rig.rx.recv().await.unwrap();
    assert!(matches!(&msg, Msg::Command(Command::Cancel { run }) if *run == child));
    rig.app.step(msg).unwrap();
    assert_eq!(rig.status(child), "cancelled");
    assert!(rig.launched().is_empty());
}

// ---- explicit ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_mention_opens_the_lead_with_a_delegate_and_the_lead_still_gets_the_result() {
    let _clock = FakeClock::new();
    let mut rig = Rig::new(Setup::default(), script([text("thanks")]), |_| {
        script([text("the report")])
    });
    rig.start_lead("@agent-explore map the repo", Some(("explore", "map the repo")));
    let out = rig.pump(|_, _, _| {}).await;

    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 1, "the delegate stood in for the first request");
    let m = &reqs[0].request.messages;
    assert_eq!(m.len(), 3, "the task, the delegate call, its result");
    assert!(m[0].role == MessageRole::User);
    assert!(m[1].role == MessageRole::Assistant);
    assert!(m[2].role == MessageRole::User);
    let ContentBlock::ToolUse { name, input, .. } = &m[1].blocks[0] else {
        panic!("{:?}", m[1])
    };
    assert_eq!(name.as_str(), "delegate");
    assert_eq!(input["agent"], "explore");
    assert_eq!(input["prompt"], "map the repo");
    assert_eq!(input["background"], false);
    let report = &results(&reqs[0])[0];
    assert!(report.contains("the report") && report.contains("· explore · completed"), "{report}");

    let child = rig.launched()[0].run;
    assert_eq!(rig.launched()[0].parent, Some(rig.lead));
    assert_eq!(out.finished(child).summary, "the report");
    let lead = out.finished(rig.lead);
    assert_eq!((lead.status, lead.turns, lead.summary.as_str()), (RunStatus::Completed, 1, "thanks"));
}
