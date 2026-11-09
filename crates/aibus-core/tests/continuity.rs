//! C1.4: the continuity tools (ledger, handoff, recall, memory) called the way a run calls them,
//! and the handoff samples in `fixtures/handoffs/`.

use std::fs;
use std::path::{Path, PathBuf};

use aibus_core::config::Config;
use aibus_core::continuity::handoff::Handoff;
use aibus_core::continuity::{ledger, Layout};
use aibus_core::*;
use serde_json::{json, Value};
use aibus_testkit::{xdg_temp, Xdg};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Rig {
    dir: Xdg,
    layout: Layout,
    redactor: Redactor,
    rx: mpsc::Receiver<Msg>,
    cx: ToolCx,
}

fn rig() -> Rig {
    let dir = xdg_temp();
    let project = dir.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    let layout = Layout::new(&project, &Config::default(), &dir.path().join("data"), Some("auth")).unwrap();
    let (tx, rx) = mpsc::channel(32);
    let cx = ToolCx {
        run: RunId::new(),
        cwd: project,
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx,
        memory: None,
    };
    Rig {
        dir,
        layout,
        redactor: Redactor::new(),
        rx,
        cx,
    }
}

impl Rig {
    fn msgs(&mut self) -> Vec<Msg> {
        std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
    }

    fn memory(&mut self, agent: &str) -> PathBuf {
        let dir = self.layout.project.join(".aibus/agent-memory-local").join(agent);
        self.cx.memory = Some(dir.clone());
        dir
    }
}

fn out(r: Result<ToolOutput, ToolError>) -> Value {
    serde_json::from_str(&r.expect("the call works").content).expect("json output")
}

// ---- fixtures -------------------------------------------------------------------------------

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/handoffs");
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    found.sort();
    found
}

#[test]
fn the_continuous_claude_samples_parse_unchanged_and_round_trip() {
    let samples = fixtures();
    assert!(samples.len() >= 4, "{samples:?}");
    for path in samples {
        let text = fs::read_to_string(&path).unwrap();
        let h = Handoff::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!h.goal().is_empty() && !h.now().is_empty(), "{}", path.display());
        let again = Handoff::parse(&h.to_yaml()).unwrap();
        assert_eq!(again, h, "{}", path.display());
        assert_eq!(again.to_json(), h.to_json(), "{}", path.display());
    }
}

#[test]
fn the_spec_example_keeps_every_field() {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/handoffs/auth-refactor.yaml")).unwrap();
    let j = Handoff::parse(&text).unwrap().to_json();
    assert_eq!(j["session"], "auth-refactor");
    assert_eq!(j["date"], "2026-11-07");
    assert_eq!(j["status"], "partial");
    assert_eq!(j["outcome"], "PARTIAL_PLUS");
    assert_eq!(j["test"], "cargo test -p server auth::");
    assert_eq!(j["done_this_session"][0]["files"], json!(["src/auth/layer.rs", "src/main.rs"]));
    assert_eq!(j["questions"], json!(["Should refresh tokens rotate on every use?"]));
    assert_eq!(j["next"], json!(["Run migration 0042", "Add expired-token test"]));
    assert_eq!(j["files"]["created"], json!(["src/auth/layer.rs"]));
}

// ---- who gets which tool --------------------------------------------------------------------

fn names(def: &AgentDef, role: Role) -> Vec<String> {
    effective_tools(def, role, 0, false, 3).allow.into_iter().map(|p| p.tool.to_string()).collect()
}

#[test]
fn the_continuity_tools_are_the_leads_and_memory_follows_the_definition() {
    let def = default_def();
    let lead = names(&def, Role::Lead);
    for t in ["ledger_update", "create_handoff", "recall"] {
        assert!(lead.iter().any(|n| n == t), "lead lacks {t}");
        assert!(!names(&def, Role::Worker).iter().any(|n| n == t), "a worker has {t}");
    }
    assert!(!lead.iter().any(|n| n.starts_with("memory_")));
    let with_memory = AgentDef {
        memory: Some(MemoryScope::Project),
        ..def
    };
    for role in [Role::Lead, Role::Worker] {
        let n = names(&with_memory, role);
        assert!(["memory_read", "memory_write", "memory_append"].iter().all(|t| n.iter().any(|x| x == t)), "{role:?}");
    }
    assert!(!names(&with_memory, Role::Advisor).iter().any(|n| n.starts_with("memory_")));
}

fn default_def() -> AgentDef {
    let found = aibus_core::agent::discover::discover(None, Path::new("/nonexistent"), None, &[], &[]).unwrap();
    found.agents.values().find(|d| d.role == Role::Lead).expect("a built-in lead").clone()
}

// ---- ledger_update --------------------------------------------------------------------------

#[tokio::test]
async fn ledger_update_changes_the_file_and_tells_the_status_bar() {
    let mut r = rig();
    let tool = LedgerUpdate::new(r.layout.clone(), r.redactor.clone());
    let got = out(tool.call(&r.cx, json!({"section": "Goal", "text": "Move auth to middleware"})).await);
    assert_eq!(got["ok"], true);
    out(tool.call(&r.cx, json!({"section": "In Progress", "text": "write the test\nrun the migration"})).await);
    out(tool.call(&r.cx, json!({"section": "In Progress", "text": "ship", "append": true})).await);

    let l = ledger::read(&r.layout).unwrap().unwrap();
    assert_eq!(l.goal(), "Move auth to middleware");
    assert_eq!(l.items("In Progress"), ["write the test", "run the migration", "ship"]);
    let file = fs::read_to_string(r.layout.ledger()).unwrap();
    assert!(file.contains("## In Progress\n- write the test\n"), "{file}");

    // The last two messages carry the goal and the first open item, which `model:ledger` holds.
    let sent: Vec<(String, String)> = r
        .msgs()
        .into_iter()
        .filter_map(|m| match m {
            Msg::Command(Command::LedgerUpdate { section, text }) => Some((section, text)),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 6);
    assert_eq!(sent[4], ("goal".into(), "Move auth to middleware".into()));
    assert_eq!(sent[5], ("in_progress".into(), "write the test".into()));
}

#[tokio::test]
async fn ledger_update_refuses_an_unknown_section_and_scrubs_secrets() {
    let r = rig();
    r.redactor.register(&AccountName("work".into()), "sk-very-secret-key");
    let tool = LedgerUpdate::new(r.layout.clone(), r.redactor.clone());
    let err = tool.call(&r.cx, json!({"section": "Notes", "text": "x"})).await.unwrap_err();
    assert!(matches!(err, ToolError::Invalid(ref m) if m.contains("Goal, Completed")), "{err}");
    assert!(matches!(tool.call(&r.cx, json!({"text": "x"})).await, Err(ToolError::Invalid(_))));
    out(tool.call(&r.cx, json!({"section": "Decisions", "text": "use sk-very-secret-key for the test"})).await);
    let file = fs::read_to_string(r.layout.ledger()).unwrap();
    assert!(!file.contains("sk-very-secret-key") && file.contains("[redacted:work]"), "{file}");
}

// ---- create_handoff -------------------------------------------------------------------------

#[tokio::test]
async fn create_handoff_writes_the_schema_and_announces_the_file() {
    let mut r = rig();
    let tool = CreateHandoff::new(r.layout.clone(), r.redactor.clone(), Some("PARTIAL_PLUS".into()));
    let got = out(
        tool.call(
            &r.cx,
            json!({"goal": "Move auth", "now": "run the migration", "next": ["a", "b"],
                   "done_this_session": [{"task": "layer", "files": ["src/a.rs"]}], "status": "partial"}),
        )
        .await,
    );
    let path = PathBuf::from(got["path"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), r.layout.handoff_dir());
    let h = Handoff::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(h.text("outcome").as_deref(), Some("PARTIAL_PLUS"));
    assert_eq!(h.text("session").as_deref(), Some("auth"));
    assert_eq!(h.to_json()["done_this_session"][0]["files"], json!(["src/a.rs"]));
    assert!(r.msgs().iter().any(|m| matches!(m, Msg::HandoffWritten { path: p } if *p == path)));

    // The model's own outcome beats the session's.
    let got = out(tool.call(&r.cx, json!({"goal": "g", "now": "n", "outcome": "FAILED", "status": "blocked"})).await);
    let h = Handoff::parse(&fs::read_to_string(got["path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(h.text("outcome").as_deref(), Some("FAILED"));
}

#[tokio::test]
async fn create_handoff_needs_goal_and_now() {
    let r = rig();
    let tool = CreateHandoff::new(r.layout.clone(), r.redactor.clone(), None);
    for bad in [
        json!({"goal": "g"}),
        json!({"now": "n"}),
        json!({"goal": "", "now": "n"}),
        json!({"goal": "g", "now": "n", "status": "great"}),
        json!({"goal": "g", "now": "n", "outcome": "GREAT"}),
        json!({"goal": "g", "now": "n", "session": "x"}),
        json!({"goal": "g", "now": "n", "status": 4}),
    ] {
        let err = tool.call(&r.cx, bad.clone()).await.expect_err(&bad.to_string());
        assert!(matches!(err, ToolError::Invalid(_)), "{bad}: {err}");
    }
    assert!(!r.layout.handoffs().exists(), "nothing was written");
}

// ---- recall ---------------------------------------------------------------------------------

#[tokio::test]
async fn recall_finds_memory_ledgers_and_handoffs_with_snippets() {
    let r = rig();
    let user = r.layout.data.join("agent-memory/lead");
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("LEARNINGS.md"), "- [fact, high] the migration needs the staging flag\n").unwrap();
    ledger::update(&r.layout, "goal", "finish the migration of auth", false, "2026-11-08").unwrap();
    let tool = Recall::new(r.layout.clone(), 5);
    let hits = out(tool.call(&r.cx, json!({"query": "migration"})).await);
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    for h in hits {
        assert!(h["score"].as_f64().unwrap() > 0.0);
        assert!(h["snippet"].as_str().unwrap().contains("[migration]"), "{h}");
    }
    assert!(hits.iter().any(|h| h["path"].as_str().unwrap().ends_with("LEARNINGS.md")));
    // `k` limits, and a file written after the last sync is found too.
    assert_eq!(out(tool.call(&r.cx, json!({"query": "migration", "k": 1})).await).as_array().unwrap().len(), 1);
    fs::write(user.join("new.md"), "a brand new zebra note").unwrap();
    assert_eq!(out(tool.call(&r.cx, json!({"query": "zebra"})).await).as_array().unwrap().len(), 1);
    assert!(out(tool.call(&r.cx, json!({"query": "!!!"})).await).as_array().unwrap().is_empty());
}

// ---- memory_* -------------------------------------------------------------------------------

#[tokio::test]
async fn memory_tools_read_write_and_append_inside_the_directory() {
    let mut r = rig();
    let dir = r.memory("scribe");
    let (write, append) = (MemoryWrite::new(r.redactor.clone()), MemoryAppend::new(r.redactor.clone()));
    write.call(&r.cx, json!({"path": "MEMORY.md", "text": "# index\n"})).await.unwrap();
    write.call(&r.cx, json!({"path": "notes/api.md", "text": "first"})).await.unwrap();
    append.call(&r.cx, json!({"path": "notes/api.md", "text": "second"})).await.unwrap();
    append.call(&r.cx, json!({"path": "new.md", "text": "start"})).await.unwrap();
    assert_eq!(fs::read_to_string(dir.join("notes/api.md")).unwrap(), "first\nsecond\n");
    assert_eq!(fs::read_to_string(dir.join("new.md")).unwrap(), "start\n");
    let read = MemoryRead.call(&r.cx, json!({"path": "notes/api.md"})).await.unwrap();
    assert_eq!(read.content, "first\nsecond\n");
    assert!(matches!(MemoryRead.call(&r.cx, json!({"path": "missing.md"})).await, Err(ToolError::Invalid(_))));
    // The local scope is kept out of version control the first time it is written.
    assert_eq!(fs::read_to_string(r.layout.project.join(".gitignore")).unwrap(), ".aibus/agent-memory-local/\n");
}

#[tokio::test]
async fn memory_paths_cannot_leave_the_directory() {
    let mut r = rig();
    let dir = r.memory("scribe");
    let write = MemoryWrite::new(r.redactor.clone());
    fs::create_dir_all(&dir).unwrap();
    fs::write(r.dir.path().join("secret.txt"), "s").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(r.dir.path(), dir.join("out")).unwrap();
    let mut bad = vec!["../x.md", "a/../../x.md", "/etc/passwd", "", "."];
    if cfg!(unix) {
        bad.extend(["out/secret.txt", "out/new.md"]);
    }
    for path in bad {
        let w = write.call(&r.cx, json!({"path": path, "text": "x"})).await;
        let rd = MemoryRead.call(&r.cx, json!({"path": path})).await;
        assert!(matches!(w, Err(ToolError::Invalid(_))), "write {path:?}: {w:?}");
        assert!(matches!(rd, Err(ToolError::Invalid(_))), "read {path:?}: {rd:?}");
    }
    assert!(!r.dir.path().join("new.md").exists() && !r.dir.path().join("x.md").exists());
}

#[tokio::test]
async fn memory_tools_need_a_memory_directory_and_scrub_what_they_write() {
    let mut r = rig();
    assert!(matches!(MemoryRead.call(&r.cx, json!({"path": "a.md"})).await, Err(ToolError::Denied { .. })));
    let dir = r.memory("scribe");
    r.redactor.register(&AccountName("work".into()), "sk-very-secret-key");
    MemoryWrite::new(r.redactor.clone())
        .call(&r.cx, json!({"path": "a.md", "text": "key sk-very-secret-key"}))
        .await
        .unwrap();
    assert!(!fs::read_to_string(dir.join("a.md")).unwrap().contains("sk-very-secret-key"));
    let big = "x".repeat((1 << 20) + 1);
    assert!(matches!(
        MemoryWrite::new(r.redactor.clone()).call(&r.cx, json!({"path": "b.md", "text": big})).await,
        Err(ToolError::Invalid(_))
    ));
}

#[tokio::test]
async fn the_executor_keeps_memory_tools_behind_the_allow_list() {
    let mut r = rig();
    let dir = r.memory("scribe");
    let mut registry = ToolRegistry::default();
    registry.register(std::sync::Arc::new(MemoryWrite::new(r.redactor.clone())));
    let exec = ToolExecutor::new(registry);
    let none = EffectiveTools::default();
    let denied = exec
        .call_gated(&none, &r.cx, PermissionMode::DontAsk, "memory_write", json!({"path": "a.md", "text": "x"}), |_| async { Decision::Deny })
        .await;
    assert!(matches!(denied, Err(ToolError::Denied { ref reason }) if reason == "not in allow-list"), "{denied:?}");
    assert!(!dir.join("a.md").exists());
    let eff = EffectiveTools {
        allow: vec!["memory_write".parse().unwrap()],
        deny: vec![],
    };
    exec.call_gated(&eff, &r.cx, PermissionMode::DontAsk, "memory_write", json!({"path": "a.md", "text": "x"}), |_| async { Decision::Deny })
        .await
        .unwrap();
    assert!(dir.join("a.md").exists());
}
