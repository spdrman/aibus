//! 09 section 1: hooks and `bash` start from the same scrubbed environment, and
//! `[hooks] pass_env` is the one way back in.

use std::collections::BTreeMap;

use aibus_core::config::{Hook, Hooks};
use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_testkit::xdg_temp;
use serde_json::{json, Map};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What the model-facing process would inherit: ordinary variables, every secret shape the spec
/// lists, and one of each that `pass_env` names.
fn host() -> Vec<(String, String)> {
    let mut env = vec![("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())];
    for name in [
        "EDITOR",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GH_TOKEN",
        "DB_SECRET",
        "DB_PASSWORD",
        "AIBUS_ACCOUNT_WORK",
        "aibus_account_home",
        "PASSED_TOKEN",
    ] {
        env.push((name.to_owned(), "value-7f3a".to_owned()));
    }
    env
}

const SCRUBBED: [&str; 7] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GH_TOKEN",
    "DB_SECRET",
    "DB_PASSWORD",
    "AIBUS_ACCOUNT_WORK",
    "aibus_account_home",
];

fn has(env: &str, name: &str) -> bool {
    env.lines().any(|l| l.starts_with(&format!("{name}=")))
}

fn assert_scrubbed(env: &str, who: &str) {
    for name in SCRUBBED {
        assert!(!has(env, name), "{who} still has {name}:\n{env}");
    }
    assert!(has(env, "EDITOR"), "{who} lost an ordinary variable:\n{env}");
    assert!(has(env, "PASSED_TOKEN"), "{who} lost a pass_env entry:\n{env}");
}

#[tokio::test]
async fn a_hook_and_a_bash_call_both_start_from_the_scrubbed_environment() {
    let dir = xdg_temp();
    let pass = vec!["PASSED_TOKEN".to_owned()];

    let hooks = Hooks {
        pass_env: pass.clone(),
        events: BTreeMap::from([(
            "pre_tool_use".to_owned(),
            vec![Hook { command: "env > hook-env.txt".into(), ..Hook::default() }],
        )]),
    };
    let runner = HookRunner::new(&hooks, host(), dir.path().to_path_buf(), "01SESSION".into(), None, None);
    let input = HookInput {
        event: HookEvent::PreToolUse,
        matcher: Some("bash"),
        run: Some(RunId::new()),
        agent: "lead",
        role: Role::Lead,
        cwd: dir.path(),
        transcript_path: None,
        permission_mode: PermissionMode::Default,
        extra: Map::new(),
    };
    runner.fire(&input, &HookSet::new(), &CancellationToken::new()).await;
    assert_scrubbed(&std::fs::read_to_string(dir.path().join("hook-env.txt")).unwrap(), "the hook");

    let bash = Bash::new(20, pass, host(), dir.path().to_path_buf(), "01SESSION".into());
    let cx = ToolCx {
        run: RunId::new(),
        cwd: dir.path().to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
        memory: None,
    };
    let out = bash.call(&cx, json!({"command": "env"})).await.unwrap();
    assert_scrubbed(&out.content, "bash");
}

#[tokio::test]
async fn without_pass_env_the_listed_name_is_scrubbed_too() {
    let dir = xdg_temp();
    let bash = Bash::new(20, vec![], host(), dir.path().to_path_buf(), "s".into());
    let cx = ToolCx {
        run: RunId::new(),
        cwd: dir.path().to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
        memory: None,
    };
    let out = bash.call(&cx, json!({"command": "env"})).await.unwrap();
    assert!(!has(&out.content, "PASSED_TOKEN"), "{}", out.content);
    assert!(has(&out.content, "EDITOR"));
}
