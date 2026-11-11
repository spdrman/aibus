use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aibus_core::*;
use aibus_provider::ToolSpec;
use serde_json::{json, Value};
use aibus_testkit::{xdg_temp, Xdg};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The A2.2 tests exercise the allow-list boundary only, so they run in `bypass`, where the
/// permission layer never asks.
trait Ungated {
    async fn call(
        &self,
        eff: &EffectiveTools,
        cx: &ToolCx,
        name: &str,
        input: Value,
    ) -> Result<ToolOutput, ToolError>;
}

impl Ungated for ToolExecutor {
    async fn call(
        &self,
        eff: &EffectiveTools,
        cx: &ToolCx,
        name: &str,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        self.call_gated(eff, cx, PermissionMode::Bypass, name, input, |_| async {
            Decision::Deny
        })
        .await
    }
}

fn def(allow: Option<&[&str]>, deny: &[&str]) -> AgentDef {
    AgentDef {
        name: "t".parse().unwrap(),
        description: String::new(),
        role: Role::Worker,
        tools: ToolPolicy {
            allow: allow.map(|l| l.iter().map(|s| pat(s)).collect()),
            deny: deny.iter().map(|s| pat(s)).collect(),
        },
        model: ModelRef::Inherit,
        account: None,
        permission_mode: PermissionMode::Default,
        max_turns: None,
        max_tokens_total: None,
        max_cost_usd: None,
        effort: None,
        skills: vec![],
        hooks: Default::default(),
        memory: None,
        background: false,
        isolation: Isolation::None,
        omit_project_rules: false,
        color: None,
        checkpoints: vec![],
        router: None,
        initial_prompt: None,
        auto_compact_at: None,
        fallback: None,
        system_prompt: String::new(),
        source: DefSource {
            path: None,
            scope: Scope::Builtin,
        },
    }
}

fn pat(s: &str) -> ToolPattern {
    match s.split_once('(') {
        Some((t, rest)) => ToolPattern {
            tool: t.into(),
            spec: Some(rest.trim_end_matches(')').into()),
        },
        None => ToolPattern {
            tool: s.into(),
            spec: None,
        },
    }
}

fn names(e: &EffectiveTools) -> Vec<String> {
    e.allow.iter().map(|p| p.tool.to_string()).collect()
}

fn eff(allow: &[&str]) -> EffectiveTools {
    EffectiveTools {
        allow: allow.iter().map(|s| pat(s)).collect(),
        deny: vec![],
    }
}

fn cx(cwd: &Path) -> ToolCx {
    ToolCx {
        run: RunId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
        memory: None,
    }
}

/// Counts calls so a test can prove dispatch did not happen.
struct Spy {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    reply: String,
    hang: bool,
}

#[async_trait::async_trait]
impl Tool for Spy {
    fn name(&self) -> &'static str {
        self.name
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: String::new(),
            schema: json!({}),
        }
    }
    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }
    async fn call(&self, _: &ToolCx, _: Value) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            std::future::pending::<()>().await;
        }
        Ok(ToolOutput {
            content: self.reply.clone(),
            ..Default::default()
        })
    }
}

fn spy_executor(name: &'static str, reply: &str, hang: bool) -> (ToolExecutor, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut r = ToolRegistry::default();
    r.register(Arc::new(Spy {
        name,
        calls: calls.clone(),
        reply: reply.into(),
        hang,
    }));
    (ToolExecutor::new(r), calls)
}

fn is_denied(r: &Result<ToolOutput, ToolError>) -> bool {
    matches!(r, Err(ToolError::Denied { reason }) if reason == "not in allow-list")
}

#[test]
fn deny_beats_allow_and_roles_shape_the_list() {
    let e = effective_tools(
        &def(Some(&["read", "bash(git *)", "ls"]), &["bash"]),
        Role::Worker,
        0,
        false,
        3,
    );
    assert_eq!(names(&e), ["read", "ls"], "a bare deny removes the allow");

    let e = effective_tools(
        &def(Some(&["bash(git *)"]), &["bash(git push *)"]),
        Role::Worker,
        0,
        false,
        3,
    );
    assert_eq!(e.allow, vec![pat("bash(git *)")]);
    assert_eq!(e.deny, vec![pat("bash(git push *)")], "kept for the runtime check");

    let lead = effective_tools(&def(None, &[]), Role::Lead, 0, false, 3);
    let worker = effective_tools(&def(None, &[]), Role::Worker, 0, false, 3);
    assert!(names(&lead).contains(&"fork".into()));
    assert!(!names(&worker).contains(&"fork".into()));
    assert!(names(&worker).contains(&"delegate".into()));

    let deep = effective_tools(&def(None, &[]), Role::Worker, 3, false, 3);
    assert!(!names(&deep).contains(&"delegate".into()), "no delegate at the depth limit");
    let shallow = effective_tools(&def(None, &[]), Role::Worker, 1, false, 1);
    assert!(!names(&shallow).contains(&"delegate".into()), "the limit is a setting");

    // An allow-list cannot grant what the role lacks.
    let w = effective_tools(&def(Some(&["fork", "read"]), &[]), Role::Worker, 0, false, 3);
    assert_eq!(names(&w), ["read"]);
}

#[test]
fn background_runs_lose_the_interactive_tools() {
    let fg = effective_tools(&def(None, &[]), Role::Lead, 0, false, 3);
    let bg = effective_tools(&def(None, &[]), Role::Lead, 0, true, 3);
    for t in ["ask_user", "enter_plan_mode", "exit_plan_mode"] {
        assert!(names(&fg).contains(&t.into()), "{t} in foreground");
        assert!(!names(&bg).contains(&t.into()), "{t} stripped in background");
    }
    assert!(names(&bg).contains(&"plan".into()));
}

#[tokio::test]
async fn unknown_names_are_denied_before_dispatch() {
    let (ex, calls) = spy_executor("bash", "ran", false);
    let tmp = xdg_temp();
    let r = ex
        .call(&eff(&["read"]), &cx(tmp.path()), "bash", json!({"command": "ls"}))
        .await;
    assert!(is_denied(&r), "{r:?}");
    let r = ex.call(&eff(&["bash"]), &cx(tmp.path()), "nope", json!({})).await;
    assert!(is_denied(&r));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let r = ex
        .call(&eff(&["bash"]), &cx(tmp.path()), "bash", json!({"command": "ls"}))
        .await;
    assert_eq!(r.unwrap().content, "ran");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bash_specifiers_follow_the_09_examples() {
    // (command, admitted by `bash(git *)`)
    let table = [
        ("git status", true),
        ("git   status", true),
        ("  git\tstatus", true),
        ("GIT_PAGER=cat git log", true),
        ("A=1 B='x y' git log", true),
        (r#"A="x y" git log"#, true),
        // 09: the specifier does not stop options that change what git does.
        ("git -c core.sshCommand=evil push", true),
        ("rm -rf .", false),
        ("A=1 rm -rf .", false),
        ("sudo git status", false),
        ("git", false),
        ("gitk", false),
    ];
    let tmp = xdg_temp();
    for (cmd, ok) in table {
        let (ex, calls) = spy_executor("bash", "", false);
        let r = ex
            .call(&eff(&["bash(git *)"]), &cx(tmp.path()), "bash", json!({ "command": cmd }))
            .await;
        assert_eq!(!is_denied(&r), ok, "{cmd:?}: {r:?}");
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(ok), "{cmd:?}");
    }

    // A deny specifier still bites at call time, and a bare allow admits everything.
    let (ex, _) = spy_executor("bash", "", false);
    let both = EffectiveTools {
        allow: vec![pat("bash")],
        deny: vec![pat("bash(git push *)")],
    };
    assert!(ex.call(&both, &cx(tmp.path()), "bash", json!({"command": "git  push -f"})).await.is_err());
    assert!(ex.call(&both, &cx(tmp.path()), "bash", json!({"command": "git pull"})).await.is_ok());
}

fn project() -> Xdg {
    let tmp = xdg_temp();
    let p = tmp.path();
    std::fs::create_dir_all(p.join("src/deep")).unwrap();
    std::fs::write(p.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
    std::fs::write(p.join("src/deep/b.rs"), "// beta lives here\n").unwrap();
    std::fs::write(p.join("notes.txt"), "beta notes\n").unwrap();
    let long: String = (1..=2500).map(|i| format!("l{i}\n")).collect();
    std::fs::write(p.join("long.txt"), long).unwrap();
    tmp
}

fn ro() -> ToolExecutor {
    ToolExecutor::new(ToolRegistry::read_only())
}

fn all_ro() -> EffectiveTools {
    eff(&["read", "glob", "ls", "grep"])
}

async fn run(tool: &str, cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    ro().call(&all_ro(), &cx(cwd), tool, input).await
}

#[tokio::test]
async fn read_pages_by_lines() {
    let tmp = project();
    let out = run("read", tmp.path(), json!({"path": "long.txt"})).await.unwrap();
    assert_eq!(out.content.lines().count(), 2000);
    assert!(out.truncated);
    assert!(out.content.starts_with("     1\tl1\n"));

    let out = run("read", tmp.path(), json!({"path": "long.txt", "offset": 2499}))
        .await
        .unwrap();
    assert_eq!(out.content, "  2499\tl2499\n  2500\tl2500\n");
    assert!(!out.truncated);

    let out = run("read", tmp.path(), json!({"path": "long.txt", "offset": 10, "limit": 2}))
        .await
        .unwrap();
    assert_eq!(out.content, "    10\tl10\n    11\tl11\n");
    assert!(out.truncated);
}

#[tokio::test]
async fn glob_ls_and_grep_stay_in_the_project() {
    let tmp = project();
    let g = run("glob", tmp.path(), json!({"pattern": "src/**/*.rs"})).await.unwrap();
    assert_eq!(g.content, "src/a.rs\nsrc/deep/b.rs");
    let g = run("glob", tmp.path(), json!({"pattern": "*.txt"})).await.unwrap();
    assert_eq!(g.content, "long.txt\nnotes.txt");
    assert!(matches!(
        run("glob", tmp.path(), json!({"pattern": "../*"})).await,
        Err(ToolError::Denied { .. })
    ));

    let l = run("ls", tmp.path(), json!({"path": "."})).await.unwrap();
    // xdg_temp puts its `home/` next to the files.
    assert_eq!(l.content, "home/\nlong.txt\nnotes.txt\nsrc/");

    let r = run("grep", tmp.path(), json!({"pattern": "beta"})).await.unwrap();
    for hit in ["notes.txt:1:beta notes", "src/a.rs:2:fn beta() {}", "src/deep/b.rs:1:// beta lives here"] {
        assert!(r.content.contains(hit), "{hit}");
    }
    assert_eq!(r.content.lines().count(), 3);
    let r = run("grep", tmp.path(), json!({"pattern": "beta", "glob": "*.rs"})).await.unwrap();
    assert_eq!(r.content.lines().count(), 2);
    assert!(r.content.contains("src/a.rs:2:fn beta() {}"));
    let r = run("grep", tmp.path(), json!({"pattern": "beta", "path": "src"})).await.unwrap();
    assert_eq!(r.content.lines().count(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_leave_the_project() {
    let tmp = project();
    let outside = xdg_temp();
    std::fs::write(outside.path().join("secret.txt"), "beta secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), tmp.path().join("link.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("linkdir")).unwrap();

    for (tool, input) in [
        ("read", json!({"path": "link.txt"})),
        ("read", json!({"path": "linkdir/secret.txt"})),
        ("ls", json!({"path": "linkdir"})),
        ("grep", json!({"pattern": "beta", "path": "linkdir"})),
        ("read", json!({"path": "../../../../etc/hostname"})),
    ] {
        let r = run(tool, tmp.path(), input.clone()).await;
        assert!(is_denied(&r) || matches!(r, Err(ToolError::Io(_))), "{tool} {input}: {r:?}");
    }
    // A project-wide grep and glob never follow the links.
    let r = run("grep", tmp.path(), json!({"pattern": "secret"})).await.unwrap();
    assert_eq!(r.content, "");
    let g = run("glob", tmp.path(), json!({"pattern": "**/*.txt"})).await.unwrap();
    assert!(!g.content.contains("link"));
}

#[tokio::test]
async fn an_abs_specifier_admits_an_outside_path() {
    let tmp = project();
    let outside = xdg_temp();
    let outside_dir = outside.path().canonicalize().unwrap();
    std::fs::write(outside_dir.join("x.txt"), "hello\n").unwrap();
    let abs = outside_dir.join("x.txt");
    let input = json!({"path": abs});

    let r = ro().call(&eff(&["read"]), &cx(tmp.path()), "read", input.clone()).await;
    assert!(is_denied(&r), "bare read is project-only: {r:?}");

    let spec = format!("read({}/**)", outside_dir.display());
    let r = ro().call(&eff(&[&spec]), &cx(tmp.path()), "read", input.clone()).await;
    assert_eq!(r.unwrap().content, "     1\thello\n");

    let other = xdg_temp();
    std::fs::write(other.path().join("y.txt"), "no\n").unwrap();
    let r = ro()
        .call(&eff(&[&spec]), &cx(tmp.path()), "read", json!({"path": other.path().join("y.txt")}))
        .await;
    assert!(is_denied(&r), "{r:?}");
}

#[tokio::test]
async fn output_keeps_the_tail_behind_a_head_marker() {
    let (ex, _) = spy_executor("bash", &format!("{}END", "x".repeat(1000)), false);
    let tmp = xdg_temp();
    let mut c = cx(tmp.path());
    c.limits.bash_max_output_bytes = 100;
    let out = ex.call(&eff(&["bash"]), &c, "bash", json!({"command": "echo"})).await.unwrap();
    assert!(out.truncated);
    assert!(out.content.starts_with("[... 903 bytes truncated ...]\n"), "{}", out.content);
    assert!(out.content.ends_with("xxxEND"));
    assert_eq!(out.content.lines().nth(1).unwrap().len(), 100);
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_hanging_call_returns_within_100ms() {
    let (ex, calls) = spy_executor("bash", "", true);
    let tmp = xdg_temp();
    let c = cx(tmp.path());
    let cancel = c.cancel.clone();
    let e = eff(&["bash"]);
    let call = ex.call(&e, &c, "bash", json!({"command": "sleep 999"}));
    tokio::pin!(call);
    tokio::select! {
        _ = &mut call => panic!("should hang"),
        () = tokio::time::sleep(Duration::from_secs(5)) => {}
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cancel.cancel();
    let r = tokio::time::timeout(Duration::from_millis(100), call).await.expect("returned in time");
    assert!(matches!(r, Err(ToolError::Cancelled)));
}
