//! B1.2: `write` and `edit`, their atomic replace and the path deny-list. Filesystem tests, so
//! they run in the container only.

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use aibus_core::tools::{Edit, Write};
use aibus_core::*;
use serde_json::{json, Value};
use aibus_testkit::xdg_temp;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn pat(s: &str) -> ToolPattern {
    s.parse().unwrap()
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

fn executor(home: Option<&Path>) -> ToolExecutor {
    let mut reg = ToolRegistry::default();
    reg.register(std::sync::Arc::new(Write));
    reg.register(std::sync::Arc::new(Edit));
    ToolExecutor::with_gate(
        reg,
        Gate {
            home: home.map(Path::to_path_buf),
            ..Gate::default()
        },
    )
    .unwrap()
}

async fn call(
    ex: &ToolExecutor,
    mode: PermissionMode,
    e: &EffectiveTools,
    cwd: &Path,
    tool: &str,
    input: Value,
) -> Result<ToolOutput, ToolError> {
    ex.call_gated(e, &cx(cwd), mode, tool, input, |_| async { Decision::Allow })
        .await
}

fn is_denied(r: &Result<ToolOutput, ToolError>, needle: &str) -> bool {
    matches!(r, Err(ToolError::Denied { reason }) if reason.contains(needle))
}

#[test]
fn write_and_edit_ask_by_default_and_never_in_accept_edits() {
    use PermissionMode::*;
    let table = [
        (Default, Need::Ask),
        (AcceptEdits, Need::Never),
        (DontAsk, Need::Ask),
        (Plan, Need::Ask),
        (Bypass, Need::Never),
    ];
    for tool in [&Write as &dyn Tool, &Edit] {
        for (mode, want) in table {
            assert_eq!(tool.needs_permission(&json!({}), mode), want, "{} in {mode:?}", tool.name());
        }
    }
}

#[tokio::test]
async fn write_creates_files_and_directories_and_leaves_no_temp_file() {
    let dir = xdg_temp();
    let ex = executor(None);
    let e = eff(&["write"]);
    let r = call(&ex, PermissionMode::AcceptEdits, &e, dir.path(), "write", json!({"path": "a/b/c.txt", "content": "hello"})).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(), "hello");

    let r = call(&ex, PermissionMode::AcceptEdits, &e, dir.path(), "write", json!({"path": "a/b/c.txt", "content": "second"})).await;
    assert!(r.is_ok());
    assert_eq!(std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(), "second");
    let names: Vec<_> = std::fs::read_dir(dir.path().join("a/b"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["c.txt"], "a temp file was left behind");
}

#[tokio::test]
async fn replacing_a_file_keeps_its_permissions() {
    let dir = xdg_temp();
    let f = dir.path().join("run.sh");
    std::fs::write(&f, "old").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ex = executor(None);
    call(&ex, PermissionMode::Bypass, &eff(&["write"]), dir.path(), "write", json!({"path": "run.sh", "content": "new"}))
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
}

#[tokio::test]
async fn a_failed_write_leaves_the_original_and_no_temp_file() {
    let dir = xdg_temp();
    std::fs::write(dir.path().join("keep.txt"), "original").unwrap();
    // The target is a directory, so the final rename fails after the temp file was written.
    std::fs::create_dir(dir.path().join("d")).unwrap();
    let ex = executor(None);
    let r = call(&ex, PermissionMode::Bypass, &eff(&["write"]), dir.path(), "write", json!({"path": "d", "content": "x"})).await;
    assert!(r.is_err(), "{r:?}");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| n == "keep.txt" || n == "d"), "{names:?}");
    assert_eq!(std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(), "original");
}

#[tokio::test]
async fn edit_needs_an_exact_unique_match_unless_all_is_set() {
    let dir = xdg_temp();
    let f = dir.path().join("f.txt");
    let ex = executor(None);
    let e = eff(&["edit"]);
    let edit = |input: Value| {
        let (ex, e, dir) = (&ex, &e, dir.path());
        async move { call(ex, PermissionMode::AcceptEdits, e, dir, "edit", input).await }
    };

    std::fs::write(&f, "one two one three one").unwrap();
    let r = edit(json!({"path": "f.txt", "old": "one", "new": "1"})).await;
    assert!(matches!(&r, Err(ToolError::Invalid(m)) if m.contains("3 times")), "{r:?}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one two one three one", "untouched on error");

    let r = edit(json!({"path": "f.txt", "old": "One", "new": "1"})).await;
    assert!(matches!(&r, Err(ToolError::Invalid(m)) if m.contains("not found")), "case matters: {r:?}");

    edit(json!({"path": "f.txt", "old": "two", "new": "2"})).await.unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one 2 one three one");

    let r = edit(json!({"path": "f.txt", "old": "one", "new": "1", "all": true})).await.unwrap();
    assert!(r.content.contains("3 occurrence"), "{}", r.content);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "1 2 1 three 1");

    // Whitespace is part of the match.
    std::fs::write(&f, "a\n  b\n").unwrap();
    assert!(edit(json!({"path": "f.txt", "old": "a\nb", "new": "x"})).await.is_err());
    edit(json!({"path": "f.txt", "old": "a\n  b", "new": "x"})).await.unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "x\n");

    for bad in [
        json!({"path": "f.txt", "old": "", "new": "x"}),
        json!({"path": "f.txt", "new": "x"}),
        json!({"path": "missing.txt", "old": "a", "new": "x"}),
    ] {
        assert!(edit(bad.clone()).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn the_deny_list_holds_even_in_bypass() {
    let dir = xdg_temp();
    let home = xdg_temp();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".git/hooks")).unwrap();
    std::fs::create_dir(p.join("sub")).unwrap();
    std::fs::write(p.join(".git/config"), "[core]\n").unwrap();
    std::fs::create_dir_all(p.join(".aibus")).unwrap();
    std::fs::write(p.join(".aibus/settings.toml"), "x = 1\n").unwrap();
    std::fs::create_dir_all(p.join("vendor/lib/.git")).unwrap();
    std::fs::create_dir_all(home.path().join(".config/aibus")).unwrap();
    symlink(p.join(".git"), p.join("gitlink")).unwrap();
    let ex = executor(Some(home.path()));
    let e = eff(&["write", "edit", &format!("write({}/**)", home.path().display())]);
    let e_plain = eff(&["write", "edit"]);

    let blocked = [
        ".git/config",
        ".git/hooks/pre-commit",
        ".git/new-file",
        "./.git/config",
        "sub/../.git/config",
        "vendor/lib/.git/config",
        ".GIT/config",
        "gitlink/config",
        ".aibus/settings.toml",
        ".AIBUS/Settings.toml",
    ];
    for path in blocked {
        for mode in [PermissionMode::Default, PermissionMode::AcceptEdits, PermissionMode::Bypass] {
            let r = call(&ex, mode, &e_plain, p, "write", json!({"path": path, "content": "pwned"})).await;
            assert!(is_denied(&r, "protected"), "{path} in {mode:?}: {r:?}");
        }
    }
    let r = call(&ex, PermissionMode::Bypass, &e_plain, p, "edit", json!({"path": ".git/config", "old": "core", "new": "x"})).await;
    assert!(is_denied(&r, ".git/**"), "{r:?}");
    assert_eq!(std::fs::read_to_string(p.join(".git/config")).unwrap(), "[core]\n");
    assert_eq!(std::fs::read_to_string(p.join(".aibus/settings.toml")).unwrap(), "x = 1\n");

    // ~/.config/aibus is outside the project, so it needs its own specifier to be reachable
    // at all, and the deny-list still holds against that specifier unless it names the path.
    let target = home.path().join(".config/aibus/config.toml");
    let r = call(&ex, PermissionMode::Bypass, &e, p, "write", json!({"path": target, "content": "x"})).await;
    assert!(r.is_ok(), "a specifier that names the path lifts the deny-list: {r:?}");
    let r = call(&ex, PermissionMode::Bypass, &e_plain, p, "write", json!({"path": target, "content": "y"})).await;
    assert!(is_denied(&r, "protected"), "{r:?}");

    // Things that only look similar are fine.
    for ok in [".gitignore", ".github/ci.yml", ".aibus/other.toml", "gitfile", "a.git", "notes/.gitkeep"] {
        let r = call(&ex, PermissionMode::AcceptEdits, &e_plain, p, "write", json!({"path": ok, "content": "fine"})).await;
        assert!(r.is_ok(), "{ok}: {r:?}");
    }
}

#[tokio::test]
async fn a_specifier_naming_the_path_lifts_the_deny_list() {
    let dir = xdg_temp();
    std::fs::create_dir_all(dir.path().join(".aibus")).unwrap();
    let ex = executor(None);
    let e = eff(&["write(.aibus/settings.toml)"]);
    let r = call(&ex, PermissionMode::AcceptEdits, &e, dir.path(), "write", json!({"path": ".aibus/settings.toml", "content": "x = 2\n"})).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(std::fs::read_to_string(dir.path().join(".aibus/settings.toml")).unwrap(), "x = 2\n");
    // The same specifier says nothing about .git.
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let r = call(&ex, PermissionMode::AcceptEdits, &e, dir.path(), "write", json!({"path": ".git/config", "content": "x"})).await;
    assert!(is_denied(&r, "not in allow-list") || is_denied(&r, "protected"), "{r:?}");
}

#[tokio::test]
async fn paths_are_confined_to_the_project_unless_a_specifier_reaches_out() {
    let dir = xdg_temp();
    let outside = xdg_temp();
    let ex = executor(None);
    let bare = eff(&["write"]);
    for path in [
        outside.path().join("x.txt").to_string_lossy().into_owned(),
        "../escape.txt".to_owned(),
        "sub/../../escape.txt".to_owned(),
        "/etc/aibus-test.txt".to_owned(),
    ] {
        let r = call(&ex, PermissionMode::Bypass, &bare, dir.path(), "write", json!({"path": path, "content": "x"})).await;
        assert!(r.is_err(), "{path}: {r:?}");
    }
    assert!(!outside.path().join("x.txt").exists());

    // A link that points out of the project is judged by where it lands.
    symlink(outside.path(), dir.path().join("out")).unwrap();
    let r = call(&ex, PermissionMode::Bypass, &bare, dir.path(), "write", json!({"path": "out/y.txt", "content": "x"})).await;
    assert!(is_denied(&r, "not in allow-list"), "{r:?}");
    assert!(!outside.path().join("y.txt").exists());

    let spec = eff(&[&format!("write({}/**)", outside.path().canonicalize().unwrap().display())]);
    let r = call(&ex, PermissionMode::Bypass, &spec, dir.path(), "write", json!({"path": outside.path().join("new/z.txt"), "content": "z"})).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(std::fs::read_to_string(outside.path().join("new/z.txt")).unwrap(), "z");
}

#[tokio::test]
async fn a_cancelled_write_does_nothing() {
    let dir = xdg_temp();
    let ex = executor(None);
    let c = cx(dir.path());
    c.cancel.cancel();
    let r = ex
        .call_gated(&eff(&["write"]), &c, PermissionMode::Bypass, "write", json!({"path": "a.txt", "content": "x"}), |_| async { Decision::Allow })
        .await;
    assert!(matches!(r, Err(ToolError::Cancelled)), "{r:?}");
    assert!(!dir.path().join("a.txt").exists());
}
