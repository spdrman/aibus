//! One lead run over temp XDG directories, for the cases that need no orchestrator: hooks,
//! permissions, retries, budgets, the advisor and the router. Like `Rig`, holding a `Solo`
//! checks at the end of the test that no log line and no file under its tree has a credential.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::config::Hooks;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script};
use aibus_provider::{ContentBlock, MessageRole};
use aibus_testkit::{capture_logs, fake_credential, xdg_temp, LogCapture, Xdg};
use tempfile::TempDir;
use tokio::sync::mpsc;

use crate::rig::path_env;

/// A built-in definition in `bypass`, so that a tool call is not parked on a prompt.
pub fn bypass(name: &str) -> AgentDef {
    let dir = TempDir::new().unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let mut d = found.agents[&name.parse::<AgentName>().unwrap()].clone();
    d.permission_mode = PermissionMode::Bypass;
    d
}

pub struct Solo {
    pub xdg: Xdg,
    pub dir: PathBuf,
    _logs: LogCapture,
}

pub struct Done {
    pub msgs: Vec<Msg>,
    pub fake: Arc<FakeProvider>,
}

impl Done {
    pub fn finished(&self) -> &RunResult {
        self.msgs
            .iter()
            .find_map(|m| match m {
                Msg::RunFinished { result, .. } => Some(result),
                _ => None,
            })
            .expect("a RunFinished")
    }

    pub fn count(&self, name: &str) -> usize {
        self.msgs.iter().filter(|m| m.name() == name).count()
    }

    /// The tool results the model was shown on request `n`.
    pub fn results(&self, n: usize) -> Vec<String> {
        self.fake.requests()[n]
            .request
            .messages
            .iter()
            .flat_map(|m| &m.blocks)
            .filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    /// The system notes (advice, router guesses) that request `n` carried.
    pub fn notes(&self, n: usize) -> Vec<String> {
        self.fake.requests()[n]
            .request
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .map(|m| m.plain_text())
            .collect()
    }
}

impl Solo {
    pub fn new() -> Self {
        let logs = capture_logs();
        let xdg = xdg_temp();
        let dir = xdg.root().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        Self { xdg, dir, _logs: logs }
    }

    /// The standard tools, behind `gate`.
    pub fn tools(&self, gate: Gate) -> Arc<ToolExecutor> {
        let bash = Arc::new(Bash::new(20, vec![], path_env(), self.dir.clone(), "s".into()));
        Arc::new(ToolExecutor::with_gate(ToolRegistry::standard(bash), gate).unwrap())
    }

    /// Hooks that run real `sh -c` commands in the project, so a test that fires one runs on
    /// the real clock.
    pub fn hooks(&self) -> Arc<HookRunner> {
        Arc::new(HookRunner::new(
            &Hooks::default(),
            path_env(),
            self.dir.clone(),
            "01SESSION".into(),
            Some(self.xdg.paths.state.join("logs/hooks")),
            None,
        ))
    }

    /// Plays `script` as the lead `def`. `tweak` may change the config and the dependencies
    /// before the run starts, `on` sees every message with the run's handle as it arrives.
    pub async fn go_with(
        &self,
        script: Script,
        def: AgentDef,
        tweak: impl FnOnce(&mut RunConfig, &mut RunDeps),
        mut on: impl FnMut(&Msg, &RunHandle),
    ) -> Done {
        let fake = Arc::new(FakeProvider::new(script));
        let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
        let mut cfg = RunConfig {
            run: RunId::new(),
            def,
            prompt: "do it".into(),
            parent: None,
            depth: 0,
            model: ModelId("fake-small".into()),
            account: AccountName("fake".into()),
            cwd: self.dir.clone(),
            runs_dir: self.xdg.paths.state.join("runs"),
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
        let mut deps = RunDeps {
            provider: fake.clone(),
            creds: Arc::new(fake_credential()),
            tools: self.tools(Gate::default()),
            tx,
            hooks: Arc::new(HookRunner::default()),
            compactor: None,
            assist: None,
        };
        tweak(&mut cfg, &mut deps);
        let handle = spawn(cfg, deps);
        let mut msgs = Vec::new();
        while let Some(m) = tokio::time::timeout(Duration::from_secs(3600), rx.recv())
            .await
            .expect("the run hung")
        {
            on(&m, &handle);
            let last = m.name() == "RunFinished";
            msgs.push(m);
            if last {
                break;
            }
        }
        handle.join.await.unwrap();
        Done { msgs, fake }
    }

    pub async fn go(
        &self,
        script: Script,
        def: AgentDef,
        tweak: impl FnOnce(&mut RunConfig, &mut RunDeps),
    ) -> Done {
        self.go_with(script, def, tweak, |_, _| {}).await
    }
}
