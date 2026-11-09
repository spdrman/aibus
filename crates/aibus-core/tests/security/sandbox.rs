//! 09 section 2, `[tools] sandbox`: `bash` inside `bwrap` (Linux) or `sandbox-exec` (macOS) sees
//! the project and `$TMPDIR` and not the user's home. These need the program to be installed and
//! allowed to make a namespace, which a plain container does not allow; the test says so and
//! passes when it cannot run, unless `AIBUS_REQUIRE_SANDBOX=1` (the container the gates run in
//! sets it).

use std::process::{Command, Stdio};

use aibus_core::config::SandboxKind;
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_testkit::{xdg_temp, Xdg};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The sandbox of this platform, if it works here: the program runs a trivial command.
fn usable() -> Option<SandboxKind> {
    let (kind, probe): (SandboxKind, &[&str]) = if cfg!(target_os = "macos") {
        (SandboxKind::SandboxExec, &["sandbox-exec", "-p", "(version 1)(allow default)", "true"])
    } else {
        (SandboxKind::Bwrap, &["bwrap", "--ro-bind", "/", "/", "true"])
    };
    let ok = Command::new(probe[0])
        .args(&probe[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok && std::env::var("AIBUS_REQUIRE_SANDBOX").as_deref() == Ok("1") {
        panic!("AIBUS_REQUIRE_SANDBOX=1 but {} does not work here", probe[0]);
    }
    if !ok {
        eprintln!("skipping: {} does not work here", probe[0]);
    }
    ok.then_some(kind)
}

struct World {
    base: Xdg,
}

impl World {
    fn new() -> Self {
        let base = xdg_temp();
        for d in ["home/.config/aibus", "proj", "tmp"] {
            std::fs::create_dir_all(base.path().join(d)).unwrap();
        }
        std::fs::write(base.path().join("home/.config/aibus/config.toml"), "token_hint = \"HOME-SECRET-7731\"\n").unwrap();
        Self { base }
    }

    fn bash(&self, sandbox: Option<Sandbox>) -> Bash {
        let p = self.base.path();
        let env = vec![
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), p.join("home").to_string_lossy().into_owned()),
            ("TMPDIR".to_owned(), p.join("tmp").to_string_lossy().into_owned()),
        ];
        let bash = Bash::new(20, vec![], env, p.join("proj"), "s".into());
        match sandbox {
            Some(s) => bash.with_sandbox(s),
            None => bash,
        }
    }

    async fn run(&self, bash: &Bash, command: &str) -> String {
        let cx = ToolCx {
            run: RunId::new(),
            cwd: self.base.path().join("proj"),
            cancel: CancellationToken::new(),
            limits: ToolLimits::default(),
            tx: mpsc::channel(1).0,
            memory: None,
        };
        bash.call(&cx, json!({"command": command})).await.unwrap().content
    }
}

#[tokio::test]
async fn the_sandbox_hides_the_user_config_and_keeps_the_project_and_tmp() {
    let Some(kind) = usable() else { return };
    let w = World::new();
    let read = "cat \"$HOME/.config/aibus/config.toml\"";

    // Control: without the sandbox the same command reads it, so the test is not vacuous.
    let plain = w.run(&w.bash(None), read).await;
    assert!(plain.contains("HOME-SECRET-7731"), "{plain}");

    let bash = w.bash(Some(Sandbox { kind, network: true }));
    let out = w.run(&bash, read).await;
    assert!(!out.contains("HOME-SECRET-7731"), "the sandbox let the config through: {out}");
    assert!(out.contains("exit status"), "{out}");

    let out = w.run(&bash, "echo mine > note.txt && echo scratch > \"$TMPDIR/x\" && cat note.txt").await;
    assert!(out.contains("mine"), "{out}");
    assert_eq!(std::fs::read_to_string(w.base.path().join("proj/note.txt")).unwrap(), "mine\n");
    assert_eq!(std::fs::read_to_string(w.base.path().join("tmp/x")).unwrap(), "scratch\n");

    let out = w.run(&bash, "echo pwned > \"$HOME/pwned\"").await;
    assert!(out.contains("exit status"), "writing into the home directory worked: {out}");
    assert!(!w.base.path().join("home/pwned").exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandbox_network_false_leaves_only_the_loopback_interface() {
    let Some(kind) = usable() else { return };
    let w = World::new();
    let list = "tail -n +3 /proc/net/dev | cut -d: -f1 | tr -d ' '";
    let cut = w.run(&w.bash(Some(Sandbox { kind, network: false })), list).await;
    assert_eq!(cut.trim(), "lo", "{cut}");
}

#[tokio::test]
async fn a_configured_sandbox_that_is_missing_refuses_the_call_and_never_runs_outside_it() {
    let w = World::new();
    // No PATH at all, so neither program can be found whatever this machine has installed.
    let env = vec![("HOME".to_owned(), "/nonexistent".to_owned())];
    let bash = Bash::new(20, vec![], env, w.base.path().join("proj"), "s".into())
        .with_sandbox(Sandbox { kind: SandboxKind::Bwrap, network: true });
    let cx = ToolCx {
        run: RunId::new(),
        cwd: w.base.path().join("proj"),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
        memory: None,
    };
    let r = bash.call(&cx, json!({"command": "echo ran > outside.txt"})).await;
    assert!(matches!(&r, Err(ToolError::Denied { reason }) if reason.contains("bwrap")), "{r:?}");
    assert!(!w.base.path().join("proj/outside.txt").exists());
}
