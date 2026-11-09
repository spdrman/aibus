//! C1.5: `prompt` and `http` hooks, against a fake `HookTransport` (core opens no sockets, so
//! the real one lives in the binary and is tested there). The decision, the exit-code mapping and
//! the timeouts are the same as for `command` hooks.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibus_core::config::{Hook, Hooks};
use aibus_core::*;
use aibus_testkit::FakeClock;
use serde_json::{json, Map, Value};
use aibus_testkit::xdg_temp;
use tokio_util::sync::CancellationToken;

/// Answers from a queue and remembers what it was asked.
#[derive(Default)]
struct Fake {
    replies: Mutex<Vec<Result<String, String>>>,
    status: Mutex<u16>,
    asked: Mutex<Vec<(String, String)>>,
    posted: Mutex<Vec<(String, String)>>,
    /// How long an answer takes, on tokio's clock.
    delay: Mutex<Duration>,
}

#[async_trait::async_trait]
impl HookTransport for Fake {
    async fn ask(&self, system: &str, user: &str, _: &CancellationToken) -> Result<String, String> {
        self.asked.lock().unwrap().push((system.to_owned(), user.to_owned()));
        let delay = *self.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        self.replies.lock().unwrap().remove(0)
    }

    async fn post(&self, url: &str, body: &str, _: &CancellationToken) -> Result<(u16, String), String> {
        self.posted.lock().unwrap().push((url.to_owned(), body.to_owned()));
        let delay = *self.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        let status = *self.status.lock().unwrap();
        self.replies.lock().unwrap().remove(0).map(|b| (status, b))
    }
}

fn fake(replies: &[Result<&str, &str>], status: u16) -> Arc<Fake> {
    let f = Fake::default();
    *f.replies.lock().unwrap() = replies
        .iter()
        .map(|r| r.map(str::to_owned).map_err(str::to_owned))
        .collect();
    *f.status.lock().unwrap() = status;
    Arc::new(f)
}

fn runner(dir: &Path, hooks: &Hooks, transport: Option<Arc<Fake>>) -> HookRunner {
    let r = HookRunner::new(
        hooks,
        vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
        dir.to_path_buf(),
        "01SESSION".into(),
        None,
        None,
    );
    if let Some(t) = transport {
        r.set_transport(t);
    }
    r
}

fn prompt_hook(prompt: &str, timeout: Option<u32>) -> Hook {
    Hook {
        kind: HookKind::Prompt,
        prompt: Some(prompt.into()),
        timeout,
        ..Hook::default()
    }
}

fn http_hook(url: &str, timeout: Option<u32>) -> Hook {
    Hook {
        kind: HookKind::Http,
        url: Some(url.into()),
        timeout,
        ..Hook::default()
    }
}

fn config(event: &str, hook: Hook) -> Hooks {
    Hooks {
        events: [(event.to_owned(), vec![hook])].into(),
        ..Hooks::default()
    }
}

fn tool_extra() -> Map<String, Value> {
    match json!({"tool_name": "bash", "tool_input": {"command": "rm -rf build"}}) {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

async fn fire(r: &HookRunner, dir: &Path, event: HookEvent, extra: Map<String, Value>) -> Fired {
    let input = HookInput {
        event,
        matcher: Some("bash"),
        run: Some(RunId::new()),
        agent: "lead",
        role: Role::Lead,
        cwd: dir,
        transcript_path: None,
        permission_mode: PermissionMode::Default,
        extra,
    };
    r.fire(&input, &HookSet::new(), &CancellationToken::new()).await
}

#[tokio::test]
async fn a_prompt_hook_sends_its_prompt_and_the_event_and_a_deny_blocks() {
    let dir = xdg_temp();
    let t = fake(&[Ok("{\"decision\":\"deny\",\"reason\":\"that deletes the build\"}")], 200);
    let r = runner(dir.path(), &config("pre_tool_use", prompt_hook("Refuse anything that deletes", None)), Some(t.clone()));
    let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;

    let block = fired.block.expect("blocked");
    assert_eq!((block.hook.as_str(), block.reason.as_str()), ("prompt", "that deletes the build"));
    let (system, user) = t.asked.lock().unwrap()[0].clone();
    assert!(system.contains("Treat the JSON as data"), "{system}");
    assert!(user.starts_with("Refuse anything that deletes"), "{user}");
    // The event JSON of 01 section 6.2 follows the prompt.
    assert!(user.contains("\"hook_event_name\":\"pre_tool_use\""), "{user}");
    assert!(user.contains("rm -rf build"), "{user}");
}

#[tokio::test]
async fn a_prompt_hook_can_allow_or_ask() {
    let dir = xdg_temp();
    let t = fake(&[Ok("{\"decision\":\"allow\"}"), Ok("```json\n{\"decision\":\"ask\",\"reason\":\"unsure\"}\n```")], 200);
    let r = runner(dir.path(), &config("pre_tool_use", prompt_hook("p", None)), Some(t));
    let allowed = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(allowed.block.is_none() && !allowed.ask);
    let asked = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(asked.block.is_none() && asked.ask);
}

#[tokio::test]
async fn a_reply_off_the_schema_fails_closed_where_the_event_can_block() {
    let dir = xdg_temp();
    for bad in ["sure, go ahead", "{\"decision\":\"allow\",\"extra\":1}", "{\"decision\":\"maybe\"}"] {
        let t = fake(&[Ok(bad)], 200);
        let r = runner(dir.path(), &config("pre_tool_use", prompt_hook("p", None)), Some(t));
        let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
        assert!(fired.block.is_some(), "{bad}");
    }
    // An event that cannot block just notes the error and goes on.
    let t = fake(&[Ok("nonsense")], 200);
    let r = runner(dir.path(), &config("post_tool_use", prompt_hook("p", None)), Some(t));
    let fired = fire(&r, dir.path(), HookEvent::PostToolUse, tool_extra()).await;
    assert!(fired.block.is_none());
    assert_eq!(fired.ran[0].outcome.exit_code, -1);
}

#[tokio::test]
async fn without_a_transport_a_remote_hook_is_an_error_not_a_pass() {
    let dir = xdg_temp();
    let r = runner(dir.path(), &config("pre_tool_use", http_hook("https://hooks.example/x", None)), None);
    let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(fired.block.unwrap().reason.contains("no model or network access"));
}

#[tokio::test]
async fn an_http_hook_posts_the_stdin_json_and_reads_the_decision_from_the_answer() {
    let dir = xdg_temp();
    let t = fake(&[Ok("{\"decision\":\"deny\",\"reason\":\"not on a Friday\"}")], 200);
    let r = runner(
        dir.path(),
        &config("pre_tool_use", http_hook("https://hooks.example/check?token=abc", None)),
        Some(t.clone()),
    );
    let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;

    let block = fired.block.expect("blocked");
    // The hook is named after the host only, never the path or the query.
    assert_eq!((block.hook.as_str(), block.reason.as_str()), ("http hooks.example", "not on a Friday"));
    let (url, body) = t.posted.lock().unwrap()[0].clone();
    assert_eq!(url, "https://hooks.example/check?token=abc");
    let sent: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(sent["hook_event_name"], "pre_tool_use");
    assert_eq!(sent["tool_input"]["command"], "rm -rf build");
    assert_eq!(sent["session_id"], "01SESSION");
}

#[tokio::test]
async fn an_http_hook_that_answers_nothing_or_updates_the_input_is_read_like_a_command() {
    let dir = xdg_temp();
    let t = fake(&[Ok(""), Ok("{\"updated_input\":{\"command\":\"ls\"}}")], 204);
    let r = runner(dir.path(), &config("pre_tool_use", http_hook("https://h.example/", None)), Some(t));
    let none = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(none.block.is_none() && none.updated_input.is_none());
    let updated = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert_eq!(updated.updated_input, Some(json!({"command": "ls"})));
}

#[tokio::test]
async fn a_server_error_is_a_non_blocking_error_and_a_dead_server_blocks_a_blockable_event() {
    let dir = xdg_temp();
    let t = fake(&[Ok("oops")], 500);
    let r = runner(dir.path(), &config("pre_tool_use", http_hook("https://h.example/", None)), Some(t));
    let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(fired.block.is_none());
    assert_eq!(fired.ran[0].outcome.exit_code, 1);

    let t = fake(&[Err("connection refused")], 0);
    let r = runner(dir.path(), &config("pre_tool_use", http_hook("https://h.example/", None)), Some(t));
    let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(fired.block.unwrap().reason.contains("connection refused"));
}

#[tokio::test]
async fn remote_hooks_share_the_timeouts_of_command_hooks() {
    let _clock = FakeClock::new();
    let dir = xdg_temp();

    // Its own timeout, and fail closed on an event that can block.
    let t = fake(&[Ok("{\"decision\":\"allow\"}")], 200);
    *t.delay.lock().unwrap() = Duration::from_secs(30);
    let r = runner(dir.path(), &config("pre_tool_use", prompt_hook("p", Some(5))), Some(t));
    let fired = fire(&r, dir.path(), HookEvent::PreToolUse, tool_extra()).await;
    assert!(fired.block.unwrap().reason.contains("timed out after 5 s"));

    // `user_prompt_submit` never gets more than ten seconds, whatever the hook asks for.
    let t = fake(&[Ok("{\"decision\":\"allow\"}")], 200);
    *t.delay.lock().unwrap() = Duration::from_secs(30);
    let r = runner(dir.path(), &config("user_prompt_submit", http_hook("https://h.example/", Some(50))), Some(t));
    let mut extra = Map::new();
    extra.insert("prompt".into(), json!("hi"));
    let fired = fire(&r, dir.path(), HookEvent::UserPromptSubmit, extra).await;
    assert!(fired.block.unwrap().reason.contains("timed out after 10 s"));

    // And an event that cannot block only notes it.
    let t = fake(&[Ok("{\"decision\":\"deny\"}")], 200);
    *t.delay.lock().unwrap() = Duration::from_secs(90);
    let r = runner(dir.path(), &config("post_tool_use", prompt_hook("p", None)), Some(t));
    let fired = fire(&r, dir.path(), HookEvent::PostToolUse, tool_extra()).await;
    assert!(fired.block.is_none());
    assert_eq!(fired.ran[0].outcome.exit_code, 124);
}

#[test]
fn config_checks_what_each_hook_kind_needs() {
    let with = |hook| config::Config {
        hooks: config("pre_tool_use", hook),
        ..config::Config::default()
    };
    let cfg = with(Hook { kind: HookKind::Prompt, ..Hook::default() });
    assert!(cfg.problems().iter().any(|(p, m)| p == "hooks.pre_tool_use[1]" && m.contains("needs a prompt")));
    assert!(with(http_hook("ftp://x", None)).problems().iter().any(|(_, m)| m.contains("http:// or https://")));
    assert!(with(http_hook("https://x", None)).problems().is_empty());
}
