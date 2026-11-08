//! B1.2: the permission layer, the user-facing tools and how the run loop uses them. Process
//! and filesystem tests, so this runs in the container only.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aibus_core::agent::discover::discover;
use aibus_core::permission::{bypass_allowed, decide, persist_allow, Verdict};
use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::tools::{
    AskUser, Bash, Edit, EnterPlanMode, ExitPlanMode, Plan, ReportArtifact, Write,
};
use aibus_core::*;
use aibus_graph::Graph;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::StopReason;
use aibus_testkit::{fake_credential, FakeClock};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn pat(s: &str) -> ToolPattern {
    s.parse().unwrap()
}

fn cx(cwd: &Path) -> ToolCx {
    ToolCx {
        run: RunId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
    }
}

// ---- the mode table -------------------------------------------------------------------

#[test]
fn decide_reproduces_every_cell_of_the_09_table() {
    use PermissionMode::*;
    use Verdict::*;
    // The tools whose need depends on the mode come from the real implementations.
    let bash = Bash::new(1, vec![], vec![], ".".into(), "s".into());
    let read = aibus_core::tools::Read;
    let tools: [(&str, &dyn Tool); 7] = [
        ("read", &read),
        ("write", &Write),
        ("edit", &Edit),
        ("bash", &bash),
        ("plan", &aibus_core::tools::Plan),
        ("report_artifact", &ReportArtifact),
        ("ask_user", &AskUser),
    ];
    // (mode, read, write/edit, bash, plan, report_artifact, ask_user)
    let table = [
        (Default, Allow, Ask, Ask, Allow, Allow, Allow),
        (AcceptEdits, Allow, Allow, Ask, Allow, Allow, Allow),
        (Plan, Allow, Deny, Deny, Allow, Allow, Allow),
        (DontAsk, Allow, Deny, Deny, Allow, Allow, Allow),
        (Bypass, Allow, Allow, Allow, Allow, Allow, Allow),
    ];
    for (mode, rd, wr, sh, pl, ra, au) in table {
        let want = [rd, wr, wr, sh, pl, ra, au];
        for ((name, tool), want) in tools.iter().zip(want) {
            let need = tool.needs_permission(&json!({}), mode);
            assert_eq!(decide(mode, name, need, false), want, "{name} in {mode:?}");
        }
        // web_fetch and delegate-with-worktree are not implemented yet, but they sit in the bash
        // column of the table: they report `Ask` outside bypass.
        for name in ["web_fetch", "delegate"] {
            let need = if mode == Bypass { Need::Never } else { Need::Ask };
            assert_eq!(decide(mode, name, need, false), sh, "{name} in {mode:?}");
        }
    }
}

#[test]
fn pre_approval_skips_the_prompt_but_never_lifts_a_refusal() {
    use PermissionMode::*;
    use Verdict::*;
    // (mode, need, pre-approved, verdict)
    let table = [
        (Default, Need::Ask, true, Allow),
        (Default, Need::Ask, false, Ask),
        (Default, Need::Always, true, Ask),
        (AcceptEdits, Need::Ask, true, Allow),
        (DontAsk, Need::Ask, true, Allow),
        (DontAsk, Need::Ask, false, Deny),
        (DontAsk, Need::Always, true, Deny),
        (Plan, Need::Ask, true, Deny),
        (Plan, Need::Always, true, Deny),
        (Bypass, Need::Always, false, Allow),
        (Default, Need::Never, false, Allow),
    ];
    for (mode, need, pre, want) in table {
        assert_eq!(decide(mode, "bash", need, pre), want, "{mode:?} {need:?} pre={pre}");
    }
    assert_eq!(decide(Plan, "plan", Need::Ask, false), Allow, "the plan tool is allowed in plan mode");
}

#[test]
fn bypass_needs_the_environment_variable_and_marks_the_graph() {
    use PermissionMode::*;
    assert!(matches!(bypass_allowed(Bypass, None), Err(AibusError::Usage(_))));
    assert!(matches!(bypass_allowed(Bypass, Some("0")), Err(AibusError::Usage(_))));
    assert!(matches!(bypass_allowed(Bypass, Some("yes")), Err(AibusError::Usage(_))));
    assert_eq!(bypass_allowed(Bypass, None).unwrap_err().exit_code(), 2);
    assert!(bypass_allowed(Bypass, Some("1")).unwrap());
    for m in [Default, AcceptEdits, DontAsk, Plan] {
        assert!(!bypass_allowed(m, None).unwrap());
        assert!(!bypass_allowed(m, Some("1")).unwrap());
    }

    let mut a = common::app();
    let key = "model:permissions:bypass";
    assert_eq!(a.graph.read_key(key), Some(&aibus_graph::Value::Bool(false)));
    a.enable_bypass().unwrap();
    assert_eq!(a.graph.read_key(key), Some(&aibus_graph::Value::Bool(true)));
}

#[test]
fn patterns_round_trip_and_persist_without_losing_other_keys() {
    for s in ["bash(cargo *)", "read", "write(.aibus/settings.toml)"] {
        assert_eq!(pat(s).to_string(), s);
    }
    assert!("bash(cargo *".parse::<ToolPattern>().is_err());
    assert!("(x)".parse::<ToolPattern>().is_err());
    assert_eq!(pat("Bash(git *)").tool, "bash");

    let dir = TempDir::new().unwrap();
    let f = dir.path().join(".aibus/settings.toml");
    persist_allow(&f, &[pat("bash(cargo *)")]).unwrap();
    assert!(std::fs::read_to_string(&f).unwrap().contains("bash(cargo *)"));
    std::fs::write(&f, "[defaults]\nmodel = \"fast\"\n\n[tools]\nweb = true\nallow = [\"read\"]\n").unwrap();
    persist_allow(&f, &[pat("bash(cargo *)"), pat("read"), pat("bash(cargo *)")]).unwrap();
    let doc: toml::Table = std::fs::read_to_string(&f).unwrap().parse().unwrap();
    assert_eq!(doc["defaults"]["model"].as_str(), Some("fast"));
    assert_eq!(doc["tools"]["web"].as_bool(), Some(true));
    let allow: Vec<_> = doc["tools"]["allow"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(allow, ["read", "bash(cargo *)"], "no duplicates, order kept");
    assert!(!f.with_extension("toml.tmp").exists());
}

// ---- the executor's gate --------------------------------------------------------------

fn executor(gate: Gate, dir: &Path) -> (Arc<ToolExecutor>, Arc<Bash>) {
    let env = vec![("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())];
    let bash = Arc::new(Bash::new(20, vec![], env, dir.to_path_buf(), "s".into()));
    let ex = ToolExecutor::with_gate(ToolRegistry::standard(bash.clone()), gate).unwrap();
    (Arc::new(ex), bash)
}

fn all() -> EffectiveTools {
    EffectiveTools {
        allow: ["read", "ls", "glob", "grep", "write", "edit", "bash", "report_artifact"].iter().map(|s| pat(s)).collect(),
        deny: vec![],
    }
}

async fn gated(
    ex: &ToolExecutor,
    dir: &Path,
    mode: PermissionMode,
    tool: &str,
    input: Value,
    answer: Option<Decision>,
) -> (Result<ToolOutput, ToolError>, Vec<String>) {
    let asked = std::sync::Mutex::new(Vec::new());
    let r = ex
        .call_gated(&all(), &cx(dir), mode, tool, input, |summary| {
            asked.lock().unwrap().push(summary);
            async move { answer.unwrap_or(Decision::Deny) }
        })
        .await;
    (r, asked.into_inner().unwrap())
}

fn denied(r: &Result<ToolOutput, ToolError>) -> Option<&str> {
    match r {
        Err(ToolError::Denied { reason }) => Some(reason),
        _ => None,
    }
}

#[tokio::test]
async fn the_executor_asks_denies_and_remembers_per_the_mode() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let echo = json!({"command": "echo hi"});
    let write = json!({"path": "a.txt", "content": "x"});

    // default: bash and write ask; a yes runs the call and a no is a denial
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Default, "bash", echo.clone(), Some(Decision::Allow)).await;
    assert_eq!(r.unwrap().content.trim_end(), "hi");
    assert_eq!(asked, ["bash: echo hi"]);
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Default, "bash", echo.clone(), Some(Decision::Deny)).await;
    assert_eq!(denied(&r), Some("permission denied"));
    assert_eq!(asked.len(), 1);
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Default, "write", write.clone(), Some(Decision::Allow)).await;
    assert!(r.is_ok());
    assert_eq!(asked.len(), 1);
    assert!(asked[0].starts_with("write: "));

    // accept_edits: write is silent, bash still asks
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::AcceptEdits, "write", write.clone(), None).await;
    assert!(r.is_ok() && asked.is_empty());
    let (_, asked) = gated(&ex, dir.path(), PermissionMode::AcceptEdits, "bash", echo.clone(), Some(Decision::Allow)).await;
    assert_eq!(asked.len(), 1);

    // dont_ask: what would ask is refused without asking
    for (tool, input) in [("bash", echo.clone()), ("write", write.clone())] {
        let (r, asked) = gated(&ex, dir.path(), PermissionMode::DontAsk, tool, input, Some(Decision::Allow)).await;
        assert_eq!(denied(&r), Some(&*format!("dont_ask mode does not allow {tool}")));
        assert!(asked.is_empty());
    }

    // plan: reads pass, anything that writes or runs is refused
    std::fs::write(dir.path().join("r.txt"), "readable\n").unwrap();
    let (r, _) = gated(&ex, dir.path(), PermissionMode::Plan, "read", json!({"path": "r.txt"}), None).await;
    assert!(r.unwrap().content.contains("readable"));
    for (tool, input) in [("bash", echo.clone()), ("write", write.clone())] {
        let (r, asked) = gated(&ex, dir.path(), PermissionMode::Plan, tool, input, Some(Decision::Allow)).await;
        assert_eq!(denied(&r), Some(&*format!("plan mode does not allow {tool}")));
        assert!(asked.is_empty());
    }

    // bypass: nothing asks
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Bypass, "bash", echo.clone(), None).await;
    assert!(r.is_ok() && asked.is_empty());
}

#[tokio::test]
async fn always_for_this_pattern_is_session_scoped_and_covers_every_part() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let mode = PermissionMode::Default;
    let echo = |c: &str| json!({ "command": c });

    let (r, asked) = gated(&ex, dir.path(), mode, "bash", echo("echo one"), Some(Decision::AllowSession)).await;
    assert!(r.is_ok());
    assert_eq!(asked.len(), 1);
    assert_eq!(ex.session_allow(), [pat("bash(echo *)")]);

    // The same pattern no longer asks, even as one part of a compound command...
    let (r, asked) = gated(&ex, dir.path(), mode, "bash", echo("echo two; echo three | cat"), Some(Decision::Deny)).await;
    assert_eq!(asked.len(), 1, "`cat` is not covered, so it asks: {r:?}");
    let (r, asked) = gated(&ex, dir.path(), mode, "bash", echo("echo two && echo $(echo three)"), None).await;
    assert!(r.is_ok() && asked.is_empty(), "{r:?}");
    // ...but a part it does not cover asks again.
    let (r, asked) = gated(&ex, dir.path(), mode, "bash", echo("echo ok; touch PWN"), Some(Decision::Deny)).await;
    assert_eq!(denied(&r), Some("permission denied"));
    assert_eq!(asked.len(), 1);
    assert!(!dir.path().join("PWN").exists());

    // It is held in memory only: a new executor starts empty and nothing was written.
    let (fresh, _) = executor(Gate::default(), dir.path());
    assert!(fresh.session_allow().is_empty());
    assert!(!dir.path().join(".aibus").exists());
    persist_allow(&dir.path().join(".aibus/settings.toml"), &ex.session_allow()).unwrap();
    assert!(std::fs::read_to_string(dir.path().join(".aibus/settings.toml")).unwrap().contains("bash(echo *)"));
}

#[tokio::test]
async fn pre_approved_patterns_from_config_and_the_flag_skip_the_prompt() {
    let dir = TempDir::new().unwrap();
    let gate = Gate {
        allow: vec![pat("bash(echo *)"), pat("write")],
        ..Gate::default()
    };
    let (ex, _) = executor(gate, dir.path());
    let echo = |c: &str| json!({ "command": c });
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Default, "bash", echo("echo a && echo b"), None).await;
    assert!(r.is_ok() && asked.is_empty(), "{r:?}");
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Default, "bash", echo("echo a; touch PWN"), Some(Decision::Deny)).await;
    assert!(r.is_err() && asked.len() == 1);
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::Default, "write", json!({"path": "p.txt", "content": "x"}), None).await;
    assert!(r.is_ok() && asked.is_empty());
    // dont_ask treats a pre-approved call as one that would not ask
    let (r, asked) = gated(&ex, dir.path(), PermissionMode::DontAsk, "bash", echo("echo a"), None).await;
    assert!(r.is_ok() && asked.is_empty());
    // a pre-approval does not reach into plan mode
    let (r, _) = gated(&ex, dir.path(), PermissionMode::Plan, "bash", echo("echo a"), None).await;
    assert!(r.is_err());
}

#[tokio::test]
async fn the_prompt_summary_is_redacted_and_short() {
    let dir = TempDir::new().unwrap();
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), "sk-live-123456789");
    let gate = Gate {
        redactor: Some(redactor),
        ..Gate::default()
    };
    let (ex, _) = executor(gate, dir.path());
    let (_, asked) = gated(&ex, dir.path(), PermissionMode::Default, "bash", json!({"command": "curl -H 'x: sk-live-123456789' host"}), None).await;
    assert!(!asked[0].contains("sk-live-123456789"), "{}", asked[0]);
    let long = format!("echo {}", "x".repeat(500));
    let (_, asked) = gated(&ex, dir.path(), PermissionMode::Default, "bash", json!({ "command": long }), None).await;
    assert!(asked[0].chars().count() <= 203, "{}", asked[0].len());
    assert!(asked[0].ends_with("..."));
}

#[tokio::test]
async fn cancelling_while_a_prompt_is_open_ends_the_call() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let c = cx(dir.path());
    let cancel = c.cancel.clone();
    let tools = all();
    let call = ex.call_gated(&tools, &c, PermissionMode::Default, "bash", json!({"command": "echo hi"}), |_| async {
        std::future::pending::<Decision>().await
    });
    tokio::pin!(call);
    tokio::select! {
        _ = &mut call => panic!("waiting for an answer"),
        () = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
    cancel.cancel();
    let r = tokio::time::timeout(Duration::from_millis(500), call).await.unwrap();
    assert!(matches!(r, Err(ToolError::Cancelled)), "{r:?}");
}

// ---- the user-facing tools ------------------------------------------------------------

#[tokio::test]
async fn report_artifact_records_the_file_and_the_fraction() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("out.md"), "report").unwrap();
    let c = cx(dir.path());
    let out = ReportArtifact.call(&c, json!({"path": "out.md", "kind": "report", "progress": 0.25})).await.unwrap();
    assert_eq!(out.artifacts.len(), 1);
    assert_eq!(out.artifacts[0].name, "out.md");
    assert_eq!(out.artifacts[0].kind, "report");
    assert_eq!(out.artifacts[0].path, dir.path().canonicalize().unwrap().join("out.md"));
    assert_eq!(out.progress, Some(Progress::Fraction(0.25)));

    let out = ReportArtifact.call(&c, json!({"path": "out.md", "kind": "report"})).await.unwrap();
    assert_eq!(out.progress, None);

    for bad in [
        json!({"path": "out.md", "kind": "report", "progress": 1.5}),
        json!({"path": "out.md", "kind": "report", "progress": -0.1}),
        json!({"path": "out.md", "kind": "  "}),
        json!({"path": "out.md"}),
        json!({"path": "missing.md", "kind": "report"}),
        json!({"path": "/etc/hostname", "kind": "report"}),
        json!({"path": "../x", "kind": "report"}),
    ] {
        assert!(ReportArtifact.call(&c, bad.clone()).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn ask_user_and_the_plan_tools_return_signals_for_the_run_loop() {
    let dir = TempDir::new().unwrap();
    let c = cx(dir.path());
    let out = AskUser.call(&c, json!({"question": "which?", "choices": ["a", "b"]})).await.unwrap();
    assert_eq!(out.signal, Some(Signal::AskUser { question: "which?".into(), choices: vec!["a".into(), "b".into()] }));
    assert!(AskUser.call(&c, json!({"question": ""})).await.is_err());
    assert_eq!(EnterPlanMode.call(&c, json!({})).await.unwrap().signal, Some(Signal::EnterPlan));
    assert_eq!(ExitPlanMode.call(&c, json!({})).await.unwrap().signal, Some(Signal::ExitPlan));
    assert!(EnterPlanMode.call(&c, json!({"x": 1})).await.is_err());
    let plan = Plan.call(&c, json!({"plan": "1. read\n2. write"})).await.unwrap();
    assert!(plan.content.contains("1. read") && plan.signal.is_none());
}

#[test]
fn interactive_and_plan_tools_are_lead_only_and_foreground_only() {
    let dir = TempDir::new().unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let get = |n: &str| found.agents[&n.parse::<AgentName>().unwrap()].clone();
    let names = |d: &AgentDef, role, bg| -> Vec<String> {
        effective_tools(d, role, 0, bg, 3).allow.iter().map(|p| p.tool.to_string()).collect()
    };
    let lead = names(&get("lead"), Role::Lead, false);
    for t in ["ask_user", "plan", "enter_plan_mode", "exit_plan_mode", "bash", "write", "edit", "report_artifact"] {
        assert!(lead.iter().any(|n| n == t), "lead lacks {t}: {lead:?}");
    }
    let worker = names(&get("general"), Role::Worker, false);
    for t in ["ask_user", "plan", "enter_plan_mode", "exit_plan_mode"] {
        assert!(!worker.iter().any(|n| n == t), "worker has {t}");
    }
    assert!(worker.iter().any(|n| n == "bash"));
    let background = names(&get("lead"), Role::Lead, true);
    for t in ["ask_user", "enter_plan_mode", "exit_plan_mode"] {
        assert!(!background.iter().any(|n| n == t), "background lead has {t}");
    }
    assert!(background.iter().any(|n| n == "plan"));
}

// ---- schemas --------------------------------------------------------------------------

#[test]
fn every_tool_schema_matches_its_file_under_docs_schemas_tools() {
    let bash = Arc::new(Bash::new(1, vec![], vec![], ".".into(), "s".into()));
    let reg = ToolRegistry::standard(bash);
    let everything = EffectiveTools {
        allow: [
            "read", "write", "edit", "glob", "grep", "ls", "bash", "report_artifact", "ask_user",
            "plan", "enter_plan_mode", "exit_plan_mode",
        ]
        .iter()
        .map(|s| pat(s))
        .collect(),
        deny: vec![],
    };
    let specs = reg.specs(&everything);
    assert_eq!(specs.len(), 12);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/tools");
    for spec in specs {
        let text = std::fs::read_to_string(dir.join(format!("{}.json", spec.name)))
            .unwrap_or_else(|e| panic!("schema file for {}: {e}", spec.name));
        let mut file: Value = serde_json::from_str(&text).unwrap();
        let obj = file.as_object_mut().unwrap();
        assert_eq!(obj.remove("title").as_ref().and_then(Value::as_str), Some(spec.name.as_str()));
        assert!(obj.remove("$schema").is_some());
        assert_eq!(file, spec.schema, "{} drifted from its schema file", spec.name);
    }
    // And nothing in the directory is orphaned.
    let files = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(files, 12);
}

#[tokio::test]
async fn sample_inputs_validate_against_the_schemas_the_tools_publish() {
    // (tool, good inputs, bad inputs): the bad ones must be rejected by the tool itself.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f"), "x").unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let table: [(&str, Vec<Value>, Vec<Value>); 6] = [
        ("write", vec![json!({"path": "w1", "content": ""})], vec![json!({"path": "w2"}), json!({"content": "x"}), json!({"path": "", "content": "x"}), json!({"path": "w3", "content": "x", "extra": 1})]),
        ("edit", vec![json!({"path": "f", "old": "x", "new": "y"})], vec![json!({"path": "f", "old": "y"}), json!({"path": "f", "old": "", "new": "z"}), json!({"path": "f", "old": "y", "new": "z", "all": "yes"})]),
        ("bash", vec![json!({"command": "true"}), json!({"command": "true", "timeout": 5, "background": false})], vec![json!({}), json!({"command": "true", "timeout": "5"}), json!({"command": "true", "timeout": 0})]),
        ("report_artifact", vec![json!({"path": "f", "kind": "data", "progress": 1})], vec![json!({"path": "f"}), json!({"path": "f", "kind": "data", "progress": 2})]),
        ("read", vec![json!({"path": "f"})], vec![json!({}), json!({"path": 3})]),
        ("ls", vec![json!({"path": "."})], vec![json!({"path": []})]),
    ];
    for (tool, good, bad) in table {
        for input in good {
            let (r, _) = gated(&ex, dir.path(), PermissionMode::Bypass, tool, input.clone(), None).await;
            assert!(r.is_ok(), "{tool} {input}: {r:?}");
        }
        for input in bad {
            let (r, _) = gated(&ex, dir.path(), PermissionMode::Bypass, tool, input.clone(), None).await;
            assert!(r.is_err(), "{tool} {input} should be rejected");
        }
    }
}

// ---- the run loop ---------------------------------------------------------------------

fn lead_or(name: &str) -> AgentDef {
    let dir = TempDir::new().unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    found.agents[&name.parse::<AgentName>().unwrap()].clone()
}

fn call_step(name: &str, input: Value) -> [Step; 2] {
    [
        Step::ToolCall { name: name.into(), input },
        Step::Stop(StopReason::ToolUse),
    ]
}

fn script(calls: &[(&str, Value)]) -> Script {
    let mut s = Script::new();
    for (name, input) in calls {
        for step in call_step(name, input.clone()) {
            s = s.then(step);
        }
    }
    s.then(Step::Text("all done".into()))
}

/// Runs `def` against `script` and hands every message to `on_msg` as it arrives, together with
/// the run's mailbox so the test can answer a prompt. The graph sees every message too.
async fn drive(
    script: Script,
    def: AgentDef,
    cwd: &Path,
    ex: Arc<ToolExecutor>,
    mut on_msg: impl FnMut(&Msg, &RunHandle, &mut common::A),
) -> Vec<Msg> {
    let fake = Arc::new(FakeProvider::new(script));
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let runs = TempDir::new().unwrap();
    let handle = spawn(
        RunConfig {
            run: RunId::new(),
            def,
            prompt: "do it".into(),
            parent: None,
            depth: 0,
            model: ModelId("fake-small".into()),
            account: AccountName("fake".into()),
            cwd: cwd.to_path_buf(),
            runs_dir: runs.path().to_path_buf(),
            limits: ToolLimits::default(),
            date: "2026-11-08".into(),
            data_home: None,
            budgets: Budgets::default(),
            context: Vec::new(),
            background: false,
            max_spawn_depth: 3,
            forced: None,
        },
        RunDeps {
            provider: fake,
            creds: Arc::new(fake_credential()),
            tools: ex,
            tx,
            hooks: Arc::new(HookRunner::default()),
            compactor: None,
            assist: None,
        },
    );
    let mut app = common::app();
    let mut msgs = Vec::new();
    let deadline = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(m) = rx.recv().await {
            app.step(m.clone()).unwrap();
            on_msg(&m, &handle, &mut app);
            let last = m.name() == "RunFinished";
            msgs.push(m);
            if last {
                break;
            }
        }
    });
    deadline.await.expect("the run finished in time");
    msgs
}

fn finished(msgs: &[Msg]) -> Vec<&ToolResult> {
    msgs.iter()
        .filter_map(|m| match m {
            Msg::ToolCallFinished { result, .. } => Some(result),
            _ => None,
        })
        .collect()
}

fn needed(msgs: &[Msg]) -> Vec<&PermissionRequest> {
    msgs.iter()
        .filter_map(|m| match m {
            Msg::PermissionNeeded { request } => Some(request),
            _ => None,
        })
        .collect()
}

fn status_of(a: &common::A, run: RunId) -> String {
    let v = a.graph.read_key(&format!("model:run:{run}:status"));
    v.and_then(as_str).expect("a status").to_owned()
}

fn open_len(a: &common::A) -> usize {
    as_list(a.graph.read_key("model:permissions:open").unwrap()).len()
}

#[tokio::test]
async fn a_prompt_parks_the_run_until_the_answer_arrives() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate { tty: true, ..Gate::default() }, dir.path());
    let mut saw_open = false;
    let msgs = drive(
        script(&[("bash", json!({"command": "echo ran"}))]),
        lead_or("general"),
        dir.path(),
        ex,
        |m, h, app| match m {
            Msg::PermissionNeeded { request } => {
                assert_eq!(request.run, h.id);
                assert_eq!(request.agent.as_str(), "general");
                assert_eq!(request.tool, "bash");
                assert!(request.summary.contains("echo ran"));
                // The graph holds the open request and the run waits on it.
                assert_eq!(open_len(app), 1);
                assert_eq!(status_of(app, h.id), format!("waiting_permission:{}", request.id));
                let entry = &as_list(app.graph.read_key("model:permissions:open").unwrap())[0];
                assert_eq!(get(entry, "agent").and_then(as_str), Some("general"));
                saw_open = true;
                h.mailbox.try_send(RunMail::Permission { id: request.id, decision: Decision::Allow }).unwrap();
            }
            Msg::PermissionDecided { .. } => assert_eq!(open_len(app), 0),
            _ => {}
        },
    )
    .await;
    assert!(saw_open);
    let names: Vec<_> = msgs.iter().map(Msg::name).filter(|n| !matches!(*n, "Stream" | "UsageUpdate")).collect();
    assert_eq!(
        names,
        ["RunStarted", "ToolCallRequested", "PermissionNeeded", "PermissionDecided", "ToolCallFinished", "RunFinished"]
    );
    let r = finished(&msgs);
    assert!(!r[0].is_error && r[0].content.trim_end() == "ran", "{:?}", r[0]);
    assert!(matches!(&msgs[msgs.len() - 1], Msg::RunFinished { result, .. } if result.status == RunStatus::Completed));
    assert!(msgs.iter().any(|m| matches!(m, Msg::PermissionDecided { decision: Decision::Allow, reason: None, .. })));
}

#[tokio::test]
async fn a_denied_call_is_an_error_result_and_the_run_carries_on() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate { tty: true, ..Gate::default() }, dir.path());
    let msgs = drive(
        script(&[("bash", json!({"command": "touch PWN"})), ("ls", json!({"path": "."}))]),
        lead_or("general"),
        dir.path(),
        ex,
        |m, h, _| {
            if let Msg::PermissionNeeded { request } = m {
                h.mailbox.try_send(RunMail::Permission { id: request.id, decision: Decision::Deny }).unwrap();
            }
        },
    )
    .await;
    let r = finished(&msgs);
    assert_eq!(r.len(), 2, "the second call still ran");
    assert!(r[0].is_error && r[0].content.contains("permission denied"), "{:?}", r[0]);
    assert!(!r[1].is_error);
    assert!(!dir.path().join("PWN").exists());
    assert!(matches!(&msgs[msgs.len() - 1], Msg::RunFinished { result, .. } if result.status == RunStatus::Completed));
    assert!(msgs.iter().any(|m| matches!(m, Msg::PermissionDecided { decision: Decision::Deny, reason: None, .. })));
}

#[tokio::test]
async fn headless_default_emits_needed_then_denied_with_no_tty() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let msgs = drive(
        script(&[("bash", json!({"command": "touch PWN"}))]),
        lead_or("general"),
        dir.path(),
        ex,
        |_, _, _| {},
    )
    .await;
    let names: Vec<_> = msgs.iter().map(Msg::name).filter(|n| !matches!(*n, "Stream" | "UsageUpdate")).collect();
    assert_eq!(
        names,
        ["RunStarted", "ToolCallRequested", "PermissionNeeded", "PermissionDecided", "ToolCallFinished", "RunFinished"]
    );
    let decided = msgs.iter().find_map(|m| match m {
        Msg::PermissionDecided { id, decision, reason } => Some((*id, *decision, reason.clone())),
        _ => None,
    });
    let (id, decision, reason) = decided.unwrap();
    assert_eq!(id, needed(&msgs)[0].id);
    assert_eq!(decision, Decision::Deny);
    assert_eq!(reason.as_deref(), Some("no tty"));
    assert!(finished(&msgs)[0].is_error);
    assert!(!dir.path().join("PWN").exists());
}

#[tokio::test]
async fn always_applies_to_later_calls_in_the_same_session() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate { tty: true, ..Gate::default() }, dir.path());
    let msgs = drive(
        script(&[("bash", json!({"command": "echo one"})), ("bash", json!({"command": "echo two"}))]),
        lead_or("general"),
        dir.path(),
        ex.clone(),
        |m, h, _| {
            if let Msg::PermissionNeeded { request } = m {
                h.mailbox.try_send(RunMail::Permission { id: request.id, decision: Decision::AllowSession }).unwrap();
            }
        },
    )
    .await;
    assert_eq!(needed(&msgs).len(), 1, "the second call did not ask");
    assert_eq!(finished(&msgs).len(), 2);
    assert!(finished(&msgs).iter().all(|r| !r.is_error));
    assert_eq!(ex.session_allow(), [pat("bash(echo *)")]);
}

#[tokio::test]
async fn dont_ask_and_accept_edits_in_the_loop() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let mut def = lead_or("general");
    def.permission_mode = PermissionMode::DontAsk;
    let msgs = drive(
        script(&[("bash", json!({"command": "touch PWN"}))]),
        def,
        dir.path(),
        ex.clone(),
        |_, _, _| {},
    )
    .await;
    assert!(needed(&msgs).is_empty(), "dont_ask never prompts");
    assert!(finished(&msgs)[0].content.contains("dont_ask mode does not allow bash"));

    let mut def = lead_or("general");
    def.permission_mode = PermissionMode::AcceptEdits;
    let msgs = drive(
        script(&[("write", json!({"path": "w.txt", "content": "hi"})), ("bash", json!({"command": "touch PWN"}))]),
        def,
        dir.path(),
        ex,
        |_, _, _| {},
    )
    .await;
    assert!(dir.path().join("w.txt").exists());
    assert_eq!(needed(&msgs).len(), 1, "only the bash call asked");
    assert_eq!(needed(&msgs)[0].tool, "bash");
    assert!(!dir.path().join("PWN").exists());
}

#[tokio::test]
async fn enter_plan_mode_switches_the_effective_mode_until_exit() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let mut def = lead_or("lead");
    def.permission_mode = PermissionMode::AcceptEdits;
    let msgs = drive(
        script(&[
            ("write", json!({"path": "before.txt", "content": "1"})),
            ("enter_plan_mode", json!({})),
            ("write", json!({"path": "during.txt", "content": "2"})),
            ("plan", json!({"plan": "do x then y"})),
            ("read", json!({"path": "before.txt"})),
            ("exit_plan_mode", json!({})),
            ("write", json!({"path": "after.txt", "content": "3"})),
        ]),
        def,
        dir.path(),
        ex,
        |_, _, _| {},
    )
    .await;
    let r = finished(&msgs);
    assert_eq!(r.len(), 7);
    assert!(!r[0].is_error, "{:?}", r[0]);
    assert!(!r[1].is_error);
    assert!(r[2].is_error && r[2].content.contains("plan mode does not allow write"), "{:?}", r[2]);
    assert!(!r[3].is_error, "the plan tool is allowed in plan mode: {:?}", r[3]);
    assert!(!r[4].is_error);
    assert!(!r[5].is_error);
    assert!(!r[6].is_error, "back to accept_edits: {:?}", r[6]);
    assert!(dir.path().join("before.txt").exists());
    assert!(!dir.path().join("during.txt").exists());
    assert!(dir.path().join("after.txt").exists());
}

#[tokio::test]
async fn ask_user_commits_waiting_user_and_the_next_message_is_the_answer() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let mut waiting = false;
    let msgs = drive(
        script(&[("ask_user", json!({"question": "which colour?", "choices": ["red", "blue"]}))]),
        lead_or("lead"),
        dir.path(),
        ex,
        |m, h, app| match m {
            Msg::ToolCallRequested { call, .. } => {
                assert_eq!(call.name, "ask_user");
                assert_eq!(status_of(app, h.id), "waiting_user");
                waiting = true;
                h.mailbox.try_send(RunMail::UserMessage("blue".into())).unwrap();
            }
            Msg::ToolCallFinished { .. } => assert_eq!(status_of(app, h.id), "running"),
            _ => {}
        },
    )
    .await;
    assert!(waiting);
    assert_eq!(finished(&msgs)[0].content, "blue");
}

#[tokio::test]
async fn report_artifact_reaches_the_graph_as_an_artifact_and_a_percentage() {
    let _clock = FakeClock::new();
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("report.md"), "# r").unwrap();
    let (ex, _) = executor(Gate::default(), dir.path());
    let mut meta_artifacts = 0;
    let mut progress = None;
    let msgs = drive(
        script(&[("report_artifact", json!({"path": "report.md", "kind": "report", "progress": 0.5}))]),
        lead_or("general"),
        dir.path(),
        ex,
        |m, h, app| {
            if matches!(m, Msg::ToolCallFinished { .. }) {
                let meta = app.graph.read_key(&format!("model:run:{}:meta", h.id)).unwrap();
                meta_artifacts = get(meta, "artifacts").map_or(0, |a| as_list(a).len());
                if let Some(aibus_graph::Value::F64(p)) = app.graph.read_key(&format!("model:run:{}:progress", h.id)) {
                    progress = Some(*p);
                }
            }
        },
    )
    .await;
    assert_eq!(finished(&msgs)[0].progress, Some(Progress::Fraction(0.5)));
    assert_eq!(finished(&msgs)[0].artifacts[0].kind, "report");
    assert_eq!(meta_artifacts, 1);
    assert_eq!(progress, Some(0.5));
}
