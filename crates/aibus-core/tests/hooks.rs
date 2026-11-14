//! B1.3: `HookRunner::fire` on its own. The hooks are real `sh -c` commands in a temp project;
//! the tests that need a timeout run on a paused clock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aibus_core::config::{Hook, Hooks};
use aibus_core::*;
use aibus_testkit::FakeClock;
use serde_json::{json, Map, Value};
use aibus_testkit::{xdg_temp, Xdg};
use tokio_util::sync::CancellationToken;

fn path_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

fn hook(matcher: Option<&str>, command: &str, timeout: Option<u32>) -> Hook {
    Hook {
        matcher: matcher.map(Into::into),
        command: command.into(),
        timeout,
        ..Hook::default()
    }
}

fn config(events: &[(&str, Vec<Hook>)]) -> Hooks {
    Hooks {
        pass_env: Vec::new(),
        events: events
            .iter()
            .map(|(e, h)| ((*e).to_owned(), h.clone()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn runner(dir: &Xdg, hooks: &Hooks) -> HookRunner {
    HookRunner::new(
        hooks,
        path_env(),
        dir.path().to_path_buf(),
        "01SESSION".into(),
        Some(dir.path().join("logs/hooks/01SESSION")),
        None,
    )
}

fn input<'a>(
    event: HookEvent,
    matcher: Option<&'a str>,
    cwd: &'a Path,
    role: Role,
    extra: Map<String, Value>,
) -> HookInput<'a> {
    HookInput {
        event,
        matcher,
        run: Some(RunId::new()),
        agent: "code-reviewer",
        role,
        cwd,
        transcript_path: Some(PathBuf::from("/s/runs/r/transcript.jsonl")),
        permission_mode: PermissionMode::Default,
        extra,
    }
}

fn tool_extra(command: &str) -> Map<String, Value> {
    match json!({
        "tool_name": "bash",
        "tool_input": {"command": command},
        "tool_use_id": "toolu_01ABC",
    }) {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

async fn fire(r: &HookRunner, i: &HookInput<'_>) -> Fired {
    r.fire(i, &HookSet::new(), &CancellationToken::new()).await
}

fn spec(matcher: Option<&str>, command: &str) -> HookSpec {
    HookSpec {
        matcher: matcher.map(Into::into),
        command: command.into(),
        timeout_secs: None,
        kind: HookKind::Command,
    }
}

#[tokio::test]
async fn stdin_carries_every_common_field_and_the_event_extras() {
    let dir = xdg_temp();
    let r = runner(&dir, &config(&[("pre_tool_use", vec![hook(None, "cat > stdin.json", None)])]));
    let i = input(
        HookEvent::PreToolUse,
        Some("bash"),
        dir.path(),
        Role::Worker,
        tool_extra("psql -c 'SELECT 1'"),
    );
    let fired = fire(&r, &i).await;
    assert_eq!(fired.ran.len(), 1);
    assert_eq!(fired.ran[0].outcome.exit_code, 0);

    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("stdin.json")).unwrap())
            .unwrap();
    assert_eq!(v["session_id"], "01SESSION");
    assert_eq!(v["run_id"], i.run.unwrap().to_string());
    assert_eq!(v["agent_type"], "code-reviewer");
    assert_eq!(v["role"], "worker");
    assert_eq!(v["cwd"], dir.path().to_str().unwrap());
    assert_eq!(v["transcript_path"], "/s/runs/r/transcript.jsonl");
    assert_eq!(v["permission_mode"], "default");
    assert_eq!(v["hook_event_name"], "pre_tool_use");
    assert_eq!(v["tool_name"], "bash");
    assert_eq!(v["tool_input"]["command"], "psql -c 'SELECT 1'");
    assert_eq!(v["tool_use_id"], "toolu_01ABC");
}

#[tokio::test]
async fn all_eleven_events_fire_with_their_own_name() {
    let dir = xdg_temp();
    let cmd = "cat >> seen.jsonl; echo >> seen.jsonl";
    let events: Vec<(&str, Vec<Hook>)> = HookEvent::ALL
        .iter()
        .map(|e| (e.as_str(), vec![hook(None, cmd, None)]))
        .collect();
    let r = runner(&dir, &config(&events));
    assert_eq!(HookEvent::ALL.len(), 11);
    for event in HookEvent::ALL {
        let matcher = match event {
            HookEvent::UserPromptSubmit | HookEvent::Stop => None,
            _ => Some("x"),
        };
        let fired = fire(&r, &input(event, matcher, dir.path(), Role::Lead, Map::new())).await;
        assert_eq!(fired.ran.len(), 1, "{}", event.as_str());
    }
    let seen = std::fs::read_to_string(dir.path().join("seen.jsonl")).unwrap();
    let names: Vec<String> = seen
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["hook_event_name"].as_str().unwrap().to_owned())
        .collect();
    let want: Vec<&str> = HookEvent::ALL.iter().map(|e| e.as_str()).collect();
    assert_eq!(names, want);
}

#[tokio::test]
async fn only_blocking_events_can_block() {
    let can: Vec<&str> = HookEvent::ALL
        .iter()
        .filter(|e| e.can_block())
        .map(|e| e.as_str())
        .collect();
    assert_eq!(
        can,
        ["user_prompt_submit", "pre_tool_use", "subagent_stop", "stop", "checkpoint"]
    );
}

#[tokio::test]
async fn a_matcher_picks_the_tools_it_names() {
    let dir = xdg_temp();
    let r = runner(
        &dir,
        &config(&[(
            "pre_tool_use",
            vec![
                hook(Some("write|edit"), "echo w >> hit.txt", None),
                hook(Some("bash"), "echo b >> hit.txt", None),
                hook(Some("*"), "echo any >> hit.txt", None),
            ],
        )]),
    );
    for tool in ["bash", "write", "read"] {
        fire(&r, &input(HookEvent::PreToolUse, Some(tool), dir.path(), Role::Lead, Map::new()))
            .await;
    }
    let hits = std::fs::read_to_string(dir.path().join("hit.txt")).unwrap();
    assert_eq!(hits.lines().collect::<Vec<_>>(), ["b", "any", "w", "any", "any"]);
}

#[tokio::test]
async fn exit_zero_json_gives_the_verdict_and_the_updated_input() {
    let dir = xdg_temp();
    let cases: [(&str, bool, bool, Option<&str>); 4] = [
        (r#"echo '{"decision":"deny","reason":"no psql"}'"#, true, false, None),
        (r#"echo '{"decision":"ask","reason":"hm"}'"#, false, true, None),
        (r#"echo '{"decision":"allow"}'"#, false, false, None),
        (
            r#"echo '{"decision":"allow","updated_input":{"command":"ls"}}'"#,
            false,
            false,
            Some("ls"),
        ),
    ];
    for (cmd, blocks, asks, updated) in cases {
        let r = runner(&dir, &config(&[("pre_tool_use", vec![hook(None, cmd, None)])]));
        let i = input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, tool_extra("x"));
        let f = fire(&r, &i).await;
        assert_eq!(f.block.is_some(), blocks, "{cmd}");
        assert_eq!(f.ask, asks, "{cmd}");
        assert_eq!(
            f.updated_input.as_ref().and_then(|v| v["command"].as_str()),
            updated,
            "{cmd}"
        );
        if blocks {
            assert_eq!(f.block.unwrap().reason, "no psql");
        }
    }
    // A verdict on an event that cannot block is not one.
    let r = runner(
        &dir,
        &config(&[("post_tool_use", vec![hook(None, r#"echo '{"decision":"deny"}'"#, None)])]),
    );
    let f = fire(&r, &input(HookEvent::PostToolUse, Some("bash"), dir.path(), Role::Lead, Map::new())).await;
    assert!(f.block.is_none());
}

#[tokio::test]
async fn updated_input_reaches_the_next_hook_of_the_event() {
    let dir = xdg_temp();
    let r = runner(
        &dir,
        &config(&[(
            "pre_tool_use",
            vec![
                hook(None, r#"echo '{"updated_input":{"command":"second"}}'"#, None),
                hook(None, "cat > seen.json", None),
            ],
        )]),
    );
    let i = input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, tool_extra("first"));
    let f = fire(&r, &i).await;
    assert_eq!(f.updated_input.unwrap()["command"], "second");
    let seen: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("seen.json")).unwrap())
            .unwrap();
    assert_eq!(seen["tool_input"]["command"], "second");
}

#[tokio::test]
async fn plain_stdout_is_context_only_on_user_prompt_submit_and_session_start() {
    let dir = xdg_temp();
    for (event, kept) in [
        (HookEvent::UserPromptSubmit, true),
        (HookEvent::SessionStart, true),
        (HookEvent::PreToolUse, false),
        (HookEvent::SubagentStart, false),
    ] {
        let r = runner(&dir, &config(&[(event.as_str(), vec![hook(None, "echo today is tuesday", None)])]));
        let f = fire(&r, &input(event, Some("x"), dir.path(), Role::Lead, Map::new())).await;
        assert_eq!(f.context, if kept { vec!["[hook echo]\ntoday is tuesday".to_owned()] } else { vec![] }, "{}", event.as_str());
        assert!(f.block.is_none());
    }
}

#[tokio::test]
async fn exit_two_blocks_with_stderr_where_it_can_and_is_a_note_where_it_cannot() {
    let dir = xdg_temp();
    let r = runner(
        &dir,
        &config(&[
            ("pre_tool_use", vec![hook(None, "echo no psql here >&2; exit 2", None), hook(None, "echo never > second.txt", None)]),
            ("post_tool_use", vec![hook(None, "echo lint failed >&2; exit 2", None)]),
        ]),
    );
    let f = fire(&r, &input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, Map::new())).await;
    let b = f.block.unwrap();
    assert_eq!(b.reason, "no psql here");
    assert_eq!(b.hook, "echo");
    assert_eq!(f.ran.len(), 1, "a block ends the chain");
    assert!(!dir.path().join("second.txt").exists());

    let f = fire(&r, &input(HookEvent::PostToolUse, Some("bash"), dir.path(), Role::Lead, Map::new())).await;
    assert!(f.block.is_none());
    assert_eq!(f.notes, ["[hook echo] lint failed"]);
}

#[tokio::test]
async fn any_other_exit_code_is_a_hook_error_and_the_action_goes_on() {
    let dir = xdg_temp();
    let r = runner(&dir, &config(&[("pre_tool_use", vec![hook(None, "false", None)])]));
    let f = fire(&r, &input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, Map::new())).await;
    assert!(f.block.is_none());
    assert_eq!(f.ran[0].outcome.exit_code, 1);
    assert_eq!(f.ran[0].outcome.message.as_deref(), Some("hook error: false"));
}

#[tokio::test]
async fn a_hook_that_cannot_start_blocks_a_blocking_event_and_only_warns_otherwise() {
    let gone = HookRunner::new(
        &config(&[
            ("pre_tool_use", vec![hook(None, "true", None)]),
            ("post_tool_use", vec![hook(None, "true", None)]),
        ]),
        path_env(),
        PathBuf::from("/no/such/project/dir"),
        "01SESSION".into(),
        None,
        None,
    );
    let cwd = Path::new("/");
    let f = fire(&gone, &input(HookEvent::PreToolUse, Some("bash"), cwd, Role::Lead, Map::new())).await;
    assert!(f.block.unwrap().reason.contains("could not start"));
    let f = fire(&gone, &input(HookEvent::PostToolUse, Some("bash"), cwd, Role::Lead, Map::new())).await;
    assert!(f.block.is_none());
    assert_eq!(f.ran[0].outcome.message.as_deref(), Some("hook error: true"));
}

// ---- timeouts, on a paused clock --------------------------------------------------------

/// Fires with the clock paused, moves it to one second short of `secs` and checks the hook is
/// still running, then moves it the last second and returns what fired. (tokio does not
/// auto-advance while a blocking task is in flight, which a running hook is, so the test has to
/// be the one that moves time.)
async fn given_up_at(clock: &FakeClock, r: &HookRunner, i: &HookInput<'_>, secs: u64) -> Fired {
    let fut = fire(r, i);
    tokio::pin!(fut);
    // The first poll starts the hook and its timer.
    tokio::select! {
        biased;
        _ = &mut fut => panic!("the hook ended at once"),
        () = tokio::task::yield_now() => {}
    }
    clock.advance(Duration::from_secs(secs - 1)).await;
    // The kill happens on a thread of its own, in real time; give it ample room to show up.
    std::thread::sleep(Duration::from_millis(400));
    tokio::select! {
        biased;
        _ = &mut fut => panic!("given up before {secs} s"),
        () = tokio::task::yield_now() => {}
    }
    // A few milliseconds over: tokio rounds a deadline up to its next whole millisecond.
    clock.advance(Duration::from_millis(1_005)).await;
    fut.await
}

#[tokio::test]
async fn a_timeout_blocks_a_blocking_event_and_is_an_error_for_the_rest() {
    let clock = FakeClock::new();
    let dir = xdg_temp();
    let r = runner(
        &dir,
        &config(&[
            ("pre_tool_use", vec![hook(None, "sleep 100", Some(5))]),
            ("post_tool_use", vec![hook(None, "sleep 100", Some(2))]),
        ]),
    );
    let i = input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, Map::new());
    let f = given_up_at(&clock, &r, &i, 5).await;
    assert_eq!(f.block.unwrap().reason, "hook sleep timed out after 5 s");
    assert_eq!(f.ran[0].outcome.exit_code, 124);

    let i = input(HookEvent::PostToolUse, Some("bash"), dir.path(), Role::Lead, Map::new());
    let f = given_up_at(&clock, &r, &i, 2).await;
    assert!(f.block.is_none());
    assert!(f.ran[0].outcome.message.as_deref().unwrap().starts_with("hook error: sleep"));
}

#[tokio::test]
async fn the_default_is_sixty_seconds_and_user_prompt_submit_is_capped_at_ten() {
    let clock = FakeClock::new();
    let dir = xdg_temp();
    let r = runner(
        &dir,
        &config(&[
            ("pre_tool_use", vec![hook(None, "sleep 1000", None)]),
            ("user_prompt_submit", vec![hook(None, "sleep 1000", Some(60))]),
        ]),
    );
    let i = input(HookEvent::UserPromptSubmit, None, dir.path(), Role::Lead, Map::new());
    assert!(given_up_at(&clock, &r, &i, 10).await.block.is_some());
    let i = input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, Map::new());
    assert!(given_up_at(&clock, &r, &i, 60).await.block.is_some());
}

// ---- environment, cwd, capture and logs -------------------------------------------------

#[tokio::test]
async fn hooks_run_in_the_project_root_with_a_scrubbed_environment() {
    let dir = xdg_temp();
    let mut env = path_env();
    env.extend([
        ("OPENAI_API_KEY".into(), "sk-live".into()),
        ("AIBUS_ACCOUNT_WORK".into(), "k".into()),
        ("GH_TOKEN".into(), "t".into()),
        ("EDITOR".into(), "vi".into()),
    ]);
    let hooks = Hooks {
        pass_env: vec!["GH_TOKEN".into()],
        events: BTreeMap::from([(
            "pre_tool_use".to_owned(),
            vec![hook(None, "pwd > pwd.txt; env > env.txt", None)],
        )]),
    };
    let r = HookRunner::new(&hooks, env, dir.path().to_path_buf(), "01SESSION".into(), None, None);
    // The run's own directory (a worktree, say) is not where hooks start.
    let elsewhere = xdg_temp();
    let i = input(HookEvent::PreToolUse, Some("bash"), elsewhere.path(), Role::Lead, Map::new());
    fire(&r, &i).await;

    let pwd = std::fs::read_to_string(dir.path().join("pwd.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
    let env = std::fs::read_to_string(dir.path().join("env.txt")).unwrap();
    let has = |k: &str| env.lines().any(|l| l.starts_with(&format!("{k}=")));
    assert!(!has("OPENAI_API_KEY") && !has("AIBUS_ACCOUNT_WORK"));
    assert!(has("GH_TOKEN") && has("EDITOR"));
    assert!(env.contains(&format!("AIBUS_PROJECT_DIR={}", dir.path().display())));
    assert!(env.contains("AIBUS_SESSION_ID=01SESSION"));
    assert!(env.contains(&format!("AIBUS_RUN_ID={}", i.run.unwrap())));
}

#[tokio::test]
async fn output_is_capped_at_one_mebibyte_and_logged_under_logs_hooks() {
    let dir = xdg_temp();
    let r = runner(
        &dir,
        &config(&[("post_tool_use", vec![hook(None, "head -c 3145728 /dev/zero | tr '\\0' a", None)])]),
    );
    let f = fire(&r, &input(HookEvent::PostToolUse, Some("bash"), dir.path(), Role::Lead, Map::new())).await;
    assert_eq!(f.ran[0].outcome.exit_code, 0);

    let logs: Vec<_> = std::fs::read_dir(dir.path().join("logs/hooks/01SESSION"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].file_name().unwrap().to_str().unwrap().starts_with("post_tool_use-"));
    let text = std::fs::read_to_string(&logs[0]).unwrap();
    assert!(text.starts_with("$ head -c 3145728"));
    assert!(text.contains("exit 0"));
    assert!(text.contains("bytes truncated"));
    assert!(text.len() > 1_000_000 && text.len() < 1_100_000, "{}", text.len());
}

#[tokio::test]
async fn secrets_are_masked_in_the_log_and_in_what_goes_back_to_the_model() {
    let dir = xdg_temp();
    let redactor = Redactor::new();
    let secret = "sk-ant-0123456789abcdefSECRET";
    redactor.register(&AccountName("work".into()), secret);
    let r = HookRunner::new(
        &config(&[("pre_tool_use", vec![hook(None, &format!("echo {secret} >&2; exit 2"), None)])]),
        path_env(),
        dir.path().to_path_buf(),
        "01SESSION".into(),
        Some(dir.path().join("logs")),
        Some(redactor),
    );
    let mut extra = tool_extra("curl");
    extra.insert("tool_input".into(), json!({"command": format!("curl -H 'x: {secret}'")}));
    let f = fire(&r, &input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, extra)).await;
    assert!(!f.block.unwrap().reason.contains(secret));
    for entry in std::fs::read_dir(dir.path().join("logs")).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(secret));
        assert!(text.contains("[redacted:work]"));
    }
}

// ---- scoping ---------------------------------------------------------------------------

#[tokio::test]
async fn a_definitions_hooks_run_only_when_the_call_brings_them() {
    let dir = xdg_temp();
    let r = runner(&dir, &Hooks::default());
    let mut scoped = HookSet::new();
    scoped.insert("pre_tool_use".into(), vec![spec(Some("bash"), "echo mine > scoped.txt")]);
    let i = input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Worker, Map::new());

    let none = r.fire(&i, &HookSet::new(), &CancellationToken::new()).await;
    assert!(none.ran.is_empty() && !dir.path().join("scoped.txt").exists());
    let some = r.fire(&i, &scoped, &CancellationToken::new()).await;
    assert_eq!(some.ran.len(), 1);
    assert!(dir.path().join("scoped.txt").exists());
}

#[tokio::test]
async fn a_workers_stop_is_delivered_as_subagent_stop_and_a_leads_is_not() {
    let dir = xdg_temp();
    let r = runner(&dir, &Hooks::default());
    let mut scoped = HookSet::new();
    scoped.insert("stop".into(), vec![spec(None, "echo halt >&2; exit 2")]);
    let token = CancellationToken::new();

    let worker = input(HookEvent::SubagentStop, Some("code-reviewer"), dir.path(), Role::Worker, Map::new());
    assert_eq!(r.fire(&worker, &scoped, &token).await.block.unwrap().reason, "halt");
    let lead = input(HookEvent::SubagentStop, Some("code-reviewer"), dir.path(), Role::Lead, Map::new());
    assert!(r.fire(&lead, &scoped, &token).await.ran.is_empty());
    let lead_stop = input(HookEvent::Stop, None, dir.path(), Role::Lead, Map::new());
    assert!(r.fire(&lead_stop, &scoped, &token).await.block.is_some());
}

#[tokio::test]
async fn the_default_runner_runs_nothing_which_is_what_no_hooks_gives() {
    let dir = xdg_temp();
    let r = HookRunner::default();
    let mut scoped = HookSet::new();
    scoped.insert("pre_tool_use".into(), vec![spec(None, "echo x > ran.txt")]);
    let i = input(HookEvent::PreToolUse, Some("bash"), dir.path(), Role::Lead, Map::new());
    let f = r.fire(&i, &scoped, &CancellationToken::new()).await;
    assert!(f.ran.is_empty());
    assert!(!dir.path().join("ran.txt").exists());
}

#[test]
fn config_names_only_real_events_and_reads_pass_env() {
    let dir = xdg_temp();
    let file = dir.path().join("config.toml");
    std::fs::write(
        &file,
        "[hooks]\npass_env = [\"GH_TOKEN\"]\nstop = [{ command = \"a.sh\", timeout = 5 }]\n",
    )
    .unwrap();
    let c = aibus_core::config::load(&file, None).unwrap().config;
    assert_eq!(c.hooks.pass_env, ["GH_TOKEN"]);
    assert_eq!(c.hooks.events["stop"][0].timeout, Some(5));

    std::fs::write(&file, "[hooks]\nbefore_tool = [{ command = \"a.sh\" }]\n").unwrap();
    let e = aibus_core::config::load(&file, None).unwrap_err();
    assert!(e.to_string().contains("hooks.before_tool"), "{e}");
    assert_eq!(e.line, Some(2));
}
