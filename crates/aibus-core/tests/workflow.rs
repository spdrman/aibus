//! C1.5: workflows and trigger hints end to end on the fake provider. A real lead run opens with
//! a `workflow` call (what `/fix <text>` and `aibus run --workflow fix` do), the orchestrator
//! starts every step as a foreground run under the lead, and the gates are real `sh -c` commands
//! in a temp project.

mod common;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::config::Hooks;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, RecordedRequest, Script, Step};
use aibus_provider::{ContentBlock, MessageRole, StopReason};
use aibus_testkit::fake_credential;
use aibus_testkit::{xdg_temp, Xdg};
use common::{app, A};
use serde_json::{json, Value};
use tokio::sync::mpsc;

fn path_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

fn script(texts: &[&str]) -> Script {
    texts
        .iter()
        .flat_map(|t| [Step::Text((*t).into()), Step::Stop(StopReason::EndTurn)])
        .fold(Script::new(), Script::then)
}

fn defs() -> BTreeMap<AgentName, AgentDef> {
    let dir = xdg_temp();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    found
        .agents
        .iter()
        .map(|(k, v)| {
            let mut d = v.clone();
            d.permission_mode = PermissionMode::Bypass;
            (k.clone(), d)
        })
        .collect()
}

/// Hands every child the next canned answer for its agent.
struct ChainLauncher {
    answers: Mutex<HashMap<String, VecDeque<String>>>,
    defs: BTreeMap<AgentName, AgentDef>,
    tools: Arc<ToolExecutor>,
    hooks: Arc<HookRunner>,
    dir: PathBuf,
    launched: Mutex<Vec<(AgentName, RunId)>>,
    fakes: Mutex<HashMap<RunId, Arc<FakeProvider>>>,
}

impl Launcher for ChainLauncher {
    fn launch(
        &self,
        spec: &SpawnSpec,
        prompt: String,
        tx: mpsc::Sender<Msg>,
    ) -> Result<(RunConfig, RunDeps), String> {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .get_mut(spec.agent.as_str())
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| "(nothing scripted)".into());
        let fake = Arc::new(FakeProvider::new(script(&[&answer])));
        self.launched
            .lock()
            .unwrap()
            .push((spec.agent.clone(), spec.run));
        self.fakes.lock().unwrap().insert(spec.run, fake.clone());
        Ok((
            RunConfig {
                run: spec.run,
                def: self.defs[&spec.agent].clone(),
                prompt,
                parent: spec.parent,
                depth: spec.depth,
                model: ModelId("fake-small".into()),
                account: AccountName("fake".into()),
                cwd: self.dir.clone(),
                runs_dir: self.dir.join("runs"),
                limits: ToolLimits::default(),
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
                hooks: self.hooks.clone(),
                compactor: None,
                assist: None,
                fallback: None,
            },
        ))
    }
}

struct Rig {
    app: A,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    orch: Arc<Orchestrator>,
    launcher: Arc<ChainLauncher>,
    dir: Xdg,
    lead: RunId,
    lead_fake: Arc<FakeProvider>,
}

impl Rig {
    /// `children` is what each agent answers, in the order it is asked; `lead` what the lead
    /// says once it has the workflow's report; `workflows` are extra files for `.aibus/workflows`.
    fn new(
        children: &[(&str, &[&str])],
        lead: &[&str],
        workflows: &[(&str, &str)],
        triggers: Option<&str>,
    ) -> Self {
        let dir = xdg_temp();
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        let wf_dir = dir.path().join(".aibus/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        for (name, text) in aibus_core::workflow::EXAMPLES {
            std::fs::write(wf_dir.join(format!("{name}.toml")), text).unwrap();
        }
        for (name, text) in workflows {
            std::fs::write(wf_dir.join(format!("{name}.toml")), text).unwrap();
        }
        let (tx, rx) = mpsc::channel(MSG_CAPACITY);
        let limits = Limits::default();
        let orch = Orchestrator::new(limits.clone(), tx.clone());
        let mut hooks = HookRunner::new(
            &Hooks::default(),
            path_env(),
            dir.path().to_path_buf(),
            "s".into(),
            None,
            None,
        );
        if let Some(t) = triggers {
            hooks = hooks.with_triggers(Triggers::parse(t).unwrap());
        }
        let hooks = Arc::new(hooks);
        let bash = Arc::new(Bash::new(
            20,
            vec![],
            path_env(),
            dir.path().to_path_buf(),
            "s".into(),
        ));
        let mut registry = ToolRegistry::standard(bash);
        registry.register(Arc::new(Delegate::new(orch.clone())));
        registry.register(Arc::new(RunWorkflow::new(
            orch.clone(),
            hooks.clone(),
            aibus_core::workflow::dirs(dir.path(), None),
        )));
        let tools = Arc::new(ToolExecutor::with_gate(registry, Gate::default()).unwrap());
        let defs = defs();
        let launcher = Arc::new(ChainLauncher {
            answers: Mutex::new(
                children
                    .iter()
                    .map(|(a, v)| ((*a).to_owned(), v.iter().map(|s| (*s).to_owned()).collect()))
                    .collect(),
            ),
            defs: defs.clone(),
            tools,
            hooks,
            dir: dir.path().to_path_buf(),
            launched: Mutex::default(),
            fakes: Mutex::default(),
        });
        let mut app = app();
        app.effects =
            Effects::orchestrated(orch.clone(), launcher.clone(), app.store.clone(), |_| {});
        app.set_limits(&limits).unwrap();
        let all: Vec<AgentDef> = defs.values().cloned().collect();
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
            lead_fake: Arc::new(FakeProvider::new(script(lead))),
        }
    }

    fn start_lead(&mut self, prompt: &str, forced: Option<(&str, Value)>) {
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
                limits: ToolLimits::default(),
                budgets: Budgets::default(),
                date: "2026-11-08".into(),
                data_home: None,
                context: Vec::new(),
                background: false,
                max_spawn_depth: 3,
                forced: forced.map(|(t, i)| (t.into(), i)),
                start: Start::New,
                worktree: None,
            },
            RunDeps {
                provider: self.lead_fake.clone(),
                creds: Arc::new(fake_credential()),
                tools: self.launcher.tools.clone(),
                tx: self.tx.clone(),
                hooks: self.launcher.hooks.clone(),
                compactor: None,
                assist: None,
                fallback: None,
            },
        );
        self.orch
            .adopt(self.lead, handle.mailbox.clone(), handle.cancel.clone());
    }

    /// Runs the lead to its end and returns its final status.
    async fn finish(&mut self) -> RunStatus {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(60), self.rx.recv())
                .await
                .expect("the session hung")
                .expect("the channel closed");
            self.app.step(msg.clone()).unwrap();
            if let Msg::RunFinished { run, result } = &msg {
                if *run == self.lead {
                    return result.status;
                }
            }
        }
    }

    fn agents(&self) -> Vec<String> {
        self.launcher
            .launched
            .lock()
            .unwrap()
            .iter()
            .map(|(a, _)| a.to_string())
            .collect()
    }

    fn lead_requests(&self) -> Vec<RecordedRequest> {
        self.lead_fake.requests()
    }

    /// What the `n`th child run was asked.
    fn child_prompt(&self, n: usize) -> String {
        let run = self.launcher.launched.lock().unwrap()[n].1;
        let reqs = self.launcher.fakes.lock().unwrap()[&run].requests();
        user_text(&reqs[0]).join("\n")
    }
}

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

fn fix(input: &str) -> Option<(&'static str, Value)> {
    Some(("workflow", json!({"name": "fix", "input": input})))
}

#[tokio::test]
async fn fix_runs_the_chain_with_each_step_seeing_the_ones_before_it() {
    let mut rig = Rig::new(
        &[
            ("explore", &["cause is in a.txt line 1"]),
            ("plan", &["small change.\nRISK: LOW"]),
            ("general", &["patched a.txt", "tests pass"]),
        ],
        &["the chain is done"],
        &[],
        None,
    );
    rig.start_lead("/fix login fails", fix("login fails"));
    assert_eq!(rig.finish().await, RunStatus::Completed);

    assert_eq!(rig.agents(), ["explore", "plan", "general", "general"]);
    assert_eq!(
        rig.child_prompt(0),
        "Diagnose this problem and say where the cause is (file paths and line numbers): login fails"
    );
    assert!(rig
        .child_prompt(1)
        .contains("Risks of fixing it this way: cause is in a.txt line 1"));
    let make = rig.child_prompt(2);
    assert!(
        make.contains("Diagnosis: cause is in a.txt line 1"),
        "{make}"
    );
    assert!(
        make.contains("Risks to keep in mind: small change.\nRISK: LOW"),
        "{make}"
    );
    assert!(rig.child_prompt(3).contains("Problem: login fails"));

    // The lead made no model request until the chain was over, and then it saw every report.
    let reqs = rig.lead_requests();
    assert_eq!(reqs.len(), 1);
    let report = &results(&reqs[0])[0];
    assert!(
        report.starts_with("workflow fix: completed, 4 steps"),
        "{report}"
    );
    for want in [
        "cause is in a.txt line 1",
        "RISK: LOW",
        "patched a.txt",
        "tests pass",
    ] {
        assert!(report.contains(want), "{want} missing from {report}");
    }
    assert_eq!(
        report
            .matches("The following is the sub-agent's report.")
            .count(),
        4
    );
}

#[tokio::test]
async fn a_gate_that_exits_2_stops_the_chain_and_the_lead_is_told_why() {
    let mut rig = Rig::new(
        &[("explore", &["cause"]), ("plan", &["too big.\nRISK: HIGH"])],
        &["I will not go on"],
        &[],
        None,
    );
    rig.start_lead("/fix x", fix("x"));
    assert_eq!(rig.finish().await, RunStatus::Completed);

    // Only the first two steps ran: the gate on the second one blocked the rest.
    assert_eq!(rig.agents(), ["explore", "plan"]);
    let report = results(&rig.lead_requests()[0]).remove(0);
    assert!(
        report.starts_with(
            "workflow fix: stopped at step 2 of 4 (plan): gate blocked: the risk assessment ended on RISK: HIGH. Not run: general, general."
        ),
        "{report}"
    );
    assert!(report.contains("failed: gate blocked"), "{report}");
}

const RETRY: &str = r#"
description = "retry demo"

[[step]]
agent = "explore"
prompt = "look: {{input}}"

[[step]]
agent = "general"
prompt = "do: {{prev.summary}}"
# Blocks the first time it runs and passes the second.
gate = 'n=$(cat count 2>/dev/null || echo 0); echo $((n + 1)) > count; [ "$n" -ge 1 ] || { echo "not yet" >&2; exit 2; }'
on_fail = "retry:2"
"#;

#[tokio::test]
async fn on_fail_runs_the_failing_pair_again_with_the_reason() {
    let mut rig = Rig::new(
        &[
            ("explore", &["first look", "second look"]),
            ("general", &["first try", "second try"]),
        ],
        &["fixed on the second go"],
        &[("retry", RETRY)],
        None,
    );
    rig.start_lead(
        "/retry thing",
        Some(("workflow", json!({"name": "retry", "input": "thing"}))),
    );
    assert_eq!(rig.finish().await, RunStatus::Completed);

    assert_eq!(rig.agents(), ["explore", "general", "explore", "general"]);
    assert!(!rig.child_prompt(0).contains("did not pass"));
    // Both of the pair were told why the last attempt failed.
    assert!(rig
        .child_prompt(2)
        .contains("The previous attempt did not pass: gate blocked: not yet"));
    assert!(rig
        .child_prompt(3)
        .contains("The previous attempt did not pass: gate blocked: not yet"));
    // The second general step saw the second look, not the first.
    assert!(rig.child_prompt(3).contains("do: second look"));
    let report = results(&rig.lead_requests()[0]).remove(0);
    assert!(
        report.starts_with("workflow retry: completed, 2 steps"),
        "{report}"
    );
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("count"))
            .unwrap()
            .trim(),
        "2"
    );
}

#[tokio::test]
async fn a_gate_that_never_passes_is_run_at_most_one_plus_the_retries_times() {
    let always = RETRY.replace("[ \"$n\" -ge 1 ] || ", "false || ");
    let mut rig = Rig::new(&[], &["giving up"], &[("retry", &always)], None);
    rig.start_lead(
        "go",
        Some(("workflow", json!({"name": "retry", "input": "t"}))),
    );
    assert_eq!(rig.finish().await, RunStatus::Completed);

    // The pair ran three times (the first go and two retries), and the gate three times too.
    assert_eq!(rig.agents().len(), 6);
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("count"))
            .unwrap()
            .trim(),
        "3"
    );
    let report = results(&rig.lead_requests()[0]).remove(0);
    assert!(
        report.contains("stopped at step 2 of 2 (general): gate blocked: not yet"),
        "{report}"
    );
}

#[tokio::test]
async fn an_unknown_workflow_or_agent_is_an_error_the_lead_reads() {
    let mut rig = Rig::new(
        &[],
        &["ok"],
        &[("ghost", "[[step]]\nagent = \"not-here\"\nprompt = \"hi\"\n")],
        None,
    );
    rig.start_lead(
        "go",
        Some(("workflow", json!({"name": "nope", "input": "t"}))),
    );
    assert_eq!(rig.finish().await, RunStatus::Completed);
    assert!(results(&rig.lead_requests()[0])[0].contains("no workflow named nope"));
    assert!(rig.agents().is_empty());

    let mut rig = Rig::new(
        &[],
        &["ok"],
        &[("ghost", "[[step]]\nagent = \"not-here\"\nprompt = \"hi\"\n")],
        None,
    );
    rig.start_lead(
        "go",
        Some(("workflow", json!({"name": "ghost", "input": "t"}))),
    );
    assert_eq!(rig.finish().await, RunStatus::Completed);
    assert!(results(&rig.lead_requests()[0])[0]
        .contains("names the agent not-here, which is not defined"));
    assert!(rig.agents().is_empty());
}

const TRIGGERS: &str = r#"
[[trigger]]
agents = ["explore", "plan"]
keywords = ["crash"]
"#;

#[tokio::test]
async fn a_trigger_adds_a_hint_to_the_leads_turn_and_nothing_is_delegated() {
    let mut rig = Rig::new(&[], &["I looked at it myself"], &[], Some(TRIGGERS));
    rig.start_lead("the app crashes on start", None);
    assert_eq!(rig.finish().await, RunStatus::Completed);

    let reqs = rig.lead_requests();
    let seen = user_text(&reqs[0]);
    assert!(
        seen.iter()
            .any(|t| t == "[hint] matching agents: explore, plan"),
        "{seen:?}"
    );
    // The hint is in front of the prompt, which is still the last block.
    assert_eq!(seen.last().unwrap(), "the app crashes on start");
    // The model did not ask for a delegate, so none happened, whatever the hint said.
    assert!(rig.agents().is_empty());
    assert_eq!(reqs.len(), 1);
}

#[tokio::test]
async fn a_prompt_that_matches_nothing_gets_no_hint() {
    let mut rig = Rig::new(&[], &["fine"], &[], Some(TRIGGERS));
    rig.start_lead("rename a variable", None);
    assert_eq!(rig.finish().await, RunStatus::Completed);
    assert!(!user_text(&rig.lead_requests()[0])
        .iter()
        .any(|t| t.starts_with("[hint]")));
}

#[tokio::test]
async fn the_lead_can_be_handed_a_workflow_while_it_runs() {
    let mut rig = Rig::new(
        &[
            ("explore", &["cause"]),
            ("plan", &["RISK: LOW"]),
            ("general", &["a", "b"]),
        ],
        &["after"],
        &[],
        None,
    );
    rig.start_lead("hello", None);
    let mailbox = rig.orch.mailbox(rig.lead).unwrap();
    mailbox
        .send(RunMail::Call {
            tool: "workflow".into(),
            input: json!({"name": "fix", "input": "from the mailbox"}),
        })
        .await
        .unwrap();
    assert_eq!(rig.finish().await, RunStatus::Completed);
    // The mail was queued before the loop's first turn boundary, so the call came before the
    // lead's first request.
    assert_eq!(rig.agents(), ["explore", "plan", "general", "general"]);
    let report = results(&rig.lead_requests()[0]).remove(0);
    assert!(report.starts_with("workflow fix: completed"), "{report}");
}
