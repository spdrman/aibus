//! B2.2: fork, resume, `send_message` and compaction end to end on the fake provider, with the
//! same rig as the delegation tests: a real lead run, the orchestrator, the effects executor and
//! a launcher that hands every child its own scripted fake.

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
use tempfile::TempDir;
use tokio::sync::mpsc;

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
                provider: fake.clone(),
                creds: Arc::new(fake_credential()),
                tools: self.tools.clone(),
                tx,
                hooks: Arc::new(HookRunner::default()),
                compactor: Some(Arc::new(ModelCompactor {
                    provider: fake.clone(),
                    creds: Arc::new(fake_credential()),
                    model: ModelId("fake-small".into()),
                })),
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
    auto_compact_at: Option<u32>,
}

struct Out {
    msgs: Vec<Msg>,
}

impl Out {
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
        registry.register(Arc::new(Fork::new(orch.clone())));
        registry.register(Arc::new(ResumeRun::new(orch.clone())));
        registry.register(Arc::new(SendMessage::new(orch.clone())));
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
            auto_compact_at: None,
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
                def: AgentDef {
                    auto_compact_at: self.auto_compact_at,
                    ..defs()[&"lead".parse::<AgentName>().unwrap()].clone()
                },
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
                forced: forced.map(|(a, r)| (a.parse().unwrap(), r.into())),
                start: Start::New,
                worktree: None,
            },
            RunDeps {
                provider: self.lead_fake.clone(),
                creds: Arc::new(fake_credential()),
                tools: self.launcher.tools.clone(),
                tx: self.tx.clone(),
                hooks: Arc::new(HookRunner::default()),
                compactor: Some(Arc::new(ModelCompactor {
                    provider: self.lead_fake.clone(),
                    creds: Arc::new(fake_credential()),
                    model: ModelId("fake-small".into()),
                })),
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

fn conversation(rig: &Rig, run: RunId) -> String {
    std::fs::read_to_string(rig.dir.path().join(format!("runs/{run}/conversation.jsonl"))).unwrap()
}

// ---- fork -----------------------------------------------------------------------------------

#[tokio::test]
async fn a_fork_starts_from_the_parents_conversation_and_cannot_fork() {
    let _clock = FakeClock::new();
    // The fork starts from the lead's conversation at its next turn boundary, so the lead takes
    // one more turn (with a call) before it settles down to wait.
    let lead = script(
        call("fork", json!({"prompt": "check the tests"}))
            .into_iter()
            .chain([delay(1)])
            .chain(call("ls", json!({"path": "."})))
            .chain([delay(10), text("working"), end(), text("noted")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |_| script([delay(5), text("tests pass")]));
    let out = rig.run("start here").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    let launched = rig.launched();
    assert_eq!(launched.len(), 1);
    assert!(launched[0].fork);
    let fork = launched[0].run;
    let lead_reqs = rig.lead_requests();
    let fork_reqs = rig.child_requests(fork);

    // Same system prompt, and the parent's messages first, word for word.
    assert_eq!(fork_reqs[0].request.system, lead_reqs[0].request.system);
    let sent = &lead_reqs[0].request.messages;
    assert_eq!(&fork_reqs[0].request.messages[..sent.len()], &sent[..]);
    let last = fork_reqs[0].request.messages.last().unwrap();
    assert!(last.plain_text().contains("check the tests"));
    // Its tools are the lead's without `fork`.
    assert!(tool_names(&lead_reqs[0]).contains(&"fork".to_string()));
    assert!(!tool_names(&fork_reqs[0]).contains(&"fork".to_string()));
    // Its own work stays out of the parent: the lead sees the wrapped result and nothing else.
    let report = user_text(lead_reqs.last().unwrap())
        .into_iter()
        .find(|t| t.contains("sub-agent's report"))
        .expect("the result came back");
    assert!(report.contains("tests pass"), "{report}");
    // The fork's own request and answer are nowhere in the lead's conversation.
    assert!(!lead_reqs.iter().any(|r| r.request.messages.iter().any(|m| m.plain_text() == "tests pass")));

    // The card and the saved conversation mark it.
    let meta = rig.app.graph.read_key(&format!("model:run:{fork}:meta")).unwrap();
    assert_eq!(aibus_core::state::value::get(meta, "fork"), Some(&aibus_graph::Value::Bool(true)));
    assert!(conversation(&rig, fork).contains("\"kind\":\"fork\""));
}

#[tokio::test]
async fn a_fork_is_not_offered_the_fork_tool_and_cannot_call_it() {
    let _clock = FakeClock::new();
    let lead = script(
        call("fork", json!({"prompt": "go"}))
            .into_iter()
            .chain([delay(1)])
            .chain(call("ls", json!({"path": "."})))
            .chain([delay(10), text("x"), end(), text("y")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |_| {
        script(call("fork", json!({"prompt": "again"})).into_iter().chain([text("could not")]))
    });
    rig.run("start").await;
    let fork = rig.launched()[0].run;
    let r = results(&rig.child_requests(fork)[1]);
    assert!(r.iter().any(|r| r.contains("not in allow-list")), "{r:?}");
    assert_eq!(rig.launched().len(), 1, "no second fork");
}

// ---- resume and send_message ----------------------------------------------------------------

#[tokio::test]
async fn resume_run_continues_the_same_run_with_its_history_by_name() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("explore", "look around", false)
            .into_iter()
            .chain(call("resume_run", json!({"run": "explore", "message": "now the tests", "background": false})))
            .chain([text("both done")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |s| {
        script([text(if s.resume { "second round" } else { "first round" })])
    });
    let out = rig.run("go").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    let launched = rig.launched();
    assert_eq!(launched.len(), 2);
    assert_eq!(launched[0].run, launched[1].run, "the same run id");
    assert!(launched[1].resume);
    let msgs: Vec<_> = rig.child_requests(launched[1].run)[0]
        .request
        .messages
        .iter()
        .map(|m| (m.role, m.plain_text()))
        .collect();
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    assert!(msgs[0].1.contains("look around"));
    assert_eq!(msgs[1], (MessageRole::Assistant, "first round".to_string()));
    assert_eq!(msgs[2], (MessageRole::User, "now the tests".to_string()));
    let lead_results = results(&rig.lead_requests()[2]);
    assert!(lead_results[1].contains("second round"), "{lead_results:?}");
}

#[tokio::test]
async fn a_partial_run_resumes_with_its_tool_results_intact() {
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
    let child = rig.launched()[0].run;
    let m = &rig.child_requests(child)[0].request.messages;
    // The resumed run saw: task, its `ls` call, the result, and the new message in the same turn.
    assert_eq!(m.len(), 3, "{:?}", m.iter().map(|m| m.plain_text()).collect::<Vec<_>>());
    assert!(matches!(m[1].blocks[0], ContentBlock::ToolUse { .. }));
    assert!(m[2].blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { content, .. } if content.contains("a.txt"))));
    assert!(m[2].plain_text().contains("carry on"));
}

#[tokio::test]
async fn send_message_to_a_finished_run_points_at_resume_run() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("explore", "look", false)
            .into_iter()
            .chain(call("send_message", json!({"run": "explore", "text": "psst"})))
            .chain([text("ok")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |_| script([text("done")]));
    rig.run("go").await;
    let r = results(&rig.lead_requests()[2]);
    assert!(r[1].contains("resume_run"), "{r:?}");
}

#[tokio::test]
async fn send_message_reaches_a_running_run_at_its_next_turn() {
    let _clock = FakeClock::new();
    let lead = script(
        delegate_call("explore", "look", true)
            .into_iter()
            .chain([delay(1)])
            .chain(call("send_message", json!({"run": "explore", "text": "also check b"})))
            .chain([delay(30), text("ok"), end(), text("bye")]),
    );
    let mut rig = Rig::new(Setup::default(), lead, |_| {
        script([delay(3)].into_iter().chain(call("ls", json!({"path": "."}))).chain([delay(1), text("done")]))
    });
    rig.run("go").await;
    let child = rig.launched()[0].run;
    let reqs = rig.child_requests(child);
    assert!(
        reqs.iter().any(|r| user_text(r).iter().any(|t| t == "also check b")),
        "the message was delivered"
    );
}

// ---- compaction -----------------------------------------------------------------------------

#[tokio::test]
async fn compaction_at_the_threshold_summarises_cuts_the_context_and_keeps_the_history() {
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
    let out = rig.run("a long task").await;
    assert_eq!(out.finished(rig.lead).status, RunStatus::Completed);

    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 3, "turn, summary, turn");
    let before = reqs[0].request.messages.len() + 2;
    let after = &reqs[2].request.messages;
    assert!(after.len() < before, "{} vs {before}", after.len());
    assert_eq!(after.len(), 1);
    assert!(after[0].plain_text().contains("SUMMARY: listed the directory"));
    // The summary request went to the same model and carried the history.
    assert_eq!(reqs[1].request.model, reqs[0].request.model);
    assert!(reqs[1].request.messages[0].plain_text().contains("a long task"));
    assert_eq!(out.msgs.iter().filter(|m| matches!(m, Msg::Compacted { pre_tokens: 500, .. })).count(), 1);

    // Nothing is lost on disk: the first messages, one boundary, and what came after.
    let saved = conversation(&rig, rig.lead);
    assert!(saved.contains("a long task") && saved.contains("done after compacting"));
    assert_eq!(saved.matches("compact_boundary").count(), 1);
    let loaded = aibus_core::run::transcript::load(&rig.dir.path().join(format!("runs/{}/conversation.jsonl", rig.lead))).unwrap();
    assert_eq!(loaded.conv.messages.len(), 2, "summary and the last answer");
}

#[tokio::test]
async fn a_slash_compact_in_the_prompt_asks_the_lead_to_compact() {
    let _clock = FakeClock::new();
    let mut rig = Rig::new(Setup::default(), script([text("first"), end()]), |_| script([text("unused")]));
    rig.lead_started();
    rig.app.step(Msg::SubmitPrompt { text: "/compact".into() }).unwrap();
    assert_eq!(rig.status(rig.lead), "compacting");
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
