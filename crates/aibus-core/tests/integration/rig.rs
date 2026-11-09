//! The rig behind the engine integration suite (10 section 4): a real lead run, the
//! orchestrator, the effects executor and a launcher that hands every child its own scripted
//! fake, over temp XDG directories. Holding a `Rig` means two checks at the end of the test:
//! no log line and no file under the temp tree carries a credential.
#![allow(dead_code, unused_imports)]

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
use aibus_testkit::{capture_logs, fake_credential, xdg_temp, LogCapture, Xdg};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::mpsc;

use crate::common::{app, A};

pub fn path_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

pub fn text(t: &str) -> Step {
    Step::Text(t.into())
}

pub fn delay(secs: u64) -> Step {
    Step::Delay(Duration::from_secs(secs))
}

pub fn end() -> Step {
    Step::Stop(StopReason::EndTurn)
}

pub fn call(name: &str, input: Value) -> [Step; 2] {
    [
        Step::ToolCall {
            name: name.into(),
            input,
        },
        Step::Stop(StopReason::ToolUse),
    ]
}

pub fn delegate_call(agent: &str, prompt: &str, background: bool) -> [Step; 2] {
    call(
        "delegate",
        json!({"agent": agent, "prompt": prompt, "background": background}),
    )
}

pub fn script(steps: impl IntoIterator<Item = Step>) -> Script {
    steps.into_iter().fold(Script::new(), Script::then)
}

pub fn defs() -> BTreeMap<AgentName, AgentDef> {
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
pub struct TestLauncher {
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
                    handoff: None,
                })),
                assist: None,
            },
        ))
    }
}

pub struct Setup {
    pub limits: Limits,
    pub tool_limits: ToolLimits,
    pub tty: bool,
    pub defs: BTreeMap<AgentName, AgentDef>,
    pub fail_for: Option<&'static str>,
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

pub struct Rig {
    pub app: A,
    pub tx: mpsc::Sender<Msg>,
    pub rx: mpsc::Receiver<Msg>,
    pub orch: Arc<Orchestrator>,
    pub launcher: Arc<TestLauncher>,
    pub xdg: Xdg,
    pub dir: PathBuf,
    _logs: LogCapture,
    pub lead: RunId,
    pub lead_fake: Arc<FakeProvider>,
    pub auto_compact_at: Option<u32>,
    /// Where the lead's compactor writes its auto-handoffs, when a test wants them.
    pub layout: Option<aibus_core::continuity::Layout>,
}

pub struct Out {
    pub msgs: Vec<Msg>,
}

impl Out {
    pub fn position(&self, f: impl Fn(&Msg) -> bool) -> usize {
        self.msgs.iter().position(f).expect("the message was sent")
    }

    pub fn finished(&self, run: RunId) -> &RunResult {
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
    pub fn new(
        set: Setup,
        lead: Script,
        children: impl Fn(&SpawnSpec) -> Script + Send + Sync + 'static,
    ) -> Self {
        let logs = capture_logs();
        let xdg = xdg_temp();
        let dir = xdg.root().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        let (tx, rx) = mpsc::channel(MSG_CAPACITY);
        let orch = Orchestrator::new(set.limits.clone(), tx.clone());
        let bash = Arc::new(Bash::new(20, vec![], path_env(), dir.clone(), "s".into()));
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
            dir: dir.clone(),
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
            xdg,
            dir,
            _logs: logs,
            lead: RunId::new(),
            lead_fake: Arc::new(FakeProvider::new(lead)),
            auto_compact_at: None,
            layout: None,
        }
    }

    /// The graph hears about the lead first, so the orchestrator's counts include it by the time
    /// the lead asks for a child.
    pub fn lead_started(&mut self) {
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

    pub fn start_lead(&mut self, prompt: &str, forced: Option<(&str, &str)>) {
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
                cwd: self.dir.clone(),
                runs_dir: self.xdg.paths.state.join("runs"),
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
                    handoff: self.layout.clone().map(|layout| AutoHandoff {
                        layout,
                        redactor: Redactor::new(),
                    }),
                })),
                assist: None,
            },
        );
        self.orch
            .adopt(self.lead, handle.mailbox.clone(), handle.cancel.clone());
    }

    /// Steps every message into the app until the lead ends. `on` sees each one after the app
    /// has, and may step more.
    pub async fn pump(&mut self, mut on: impl FnMut(&mut A, &Orchestrator, &Msg)) -> Out {
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

    pub async fn run(&mut self, prompt: &str) -> Out {
        self.start_lead(prompt, None);
        self.pump(|_, _, _| {}).await
    }

    pub fn lead_requests(&self) -> Vec<RecordedRequest> {
        self.lead_fake.requests()
    }

    pub fn launched(&self) -> Vec<SpawnSpec> {
        self.launcher.launched.lock().unwrap().clone()
    }

    pub fn child_requests(&self, run: RunId) -> Vec<RecordedRequest> {
        self.launcher.fakes.lock().unwrap()[&run].requests()
    }

    pub fn status(&self, run: RunId) -> String {
        match self.app.graph.read_key(&format!("model:run:{run}:status")) {
            Some(aibus_graph::Value::Str(s)) => s.to_string(),
            other => panic!("no status for {run}: {other:?}"),
        }
    }
}

pub fn conversation(rig: &Rig, run: RunId) -> String {
    std::fs::read_to_string(rig.xdg.paths.state.join(format!("runs/{run}/conversation.jsonl"))).unwrap()
}

// ---- fork -----------------------------------------------------------------------------------

pub fn results(r: &RecordedRequest) -> Vec<String> {
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
pub fn user_text(r: &RecordedRequest) -> Vec<String> {
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

pub fn tool_names(r: &RecordedRequest) -> Vec<String> {
    r.request.tools.iter().map(|t| t.name.to_string()).collect()
}

// ---- background -----------------------------------------------------------------------------
