//! #322 (Phase 1 spike) end to end: the real `aibus mcp` binary over stdio, a fake attached agent
//! (the test's MCP client), the session's real control endpoint, and behind it the terminal
//! loop's own handler (`aibus_tui::remote::handle`) over a real graph. One workflow step goes
//! out to the attached session and its result comes back; the user's side is typed lines.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibus_control::{private_dir, runtime_dir, Endpoint};
use aibus_core::attach::Attachments;
use aibus_core::remote::Remote;
use aibus_core::state::value::{as_str, get};
use aibus_core::{App, Redactor};
use aibus_graph::mini::MiniGraph;
use aibus_testkit::{tui::NOW, xdg_temp};
use aibus_tui::ui::Ui;
use aibus_tui::Theme;
use common::mcp_client::{mcp_command, McpClient, Protocol};
use serde_json::{json, Value};
use tokio::sync::mpsc;

/// What the stand-in terminal loop has logged, and the typed lines it is given.
struct Loop {
    log: Arc<Mutex<Vec<String>>>,
    keyboard: std_mpsc::Sender<String>,
    notes: Arc<Mutex<Vec<Result<String, String>>>>,
    _stop: std_mpsc::Sender<()>,
}

fn short_run() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ab")
        .tempdir_in("/tmp")
        .unwrap()
}

fn start(run: &Path, id: &str) -> (Endpoint, Loop) {
    let dir = runtime_dir(Some(run.as_os_str()), run, run);
    private_dir(&dir).unwrap();
    let info = json!({"id": id, "cwd": "~/app", "lead": "lead", "model": "sonnet", "started": "2026-11-15T10:00:00Z"});
    let mut ep = Endpoint::open(&dir, id, &info).unwrap();
    let (tx, mut rx) = mpsc::channel::<Remote>(16);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    {
        let _in = rt.enter();
        ep.serve(tx, Redactor::new()).unwrap();
    }
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let notes: Arc<Mutex<Vec<Result<String, String>>>> = Arc::default();
    let (keyboard, typed) = std_mpsc::channel::<String>();
    let (stop, stopped) = std_mpsc::channel::<()>();
    let (log2, notes2) = (log.clone(), notes.clone());
    std::thread::spawn(move || {
        let mut app = App::new(MiniGraph::new(), || NOW);
        let keys = aibus_tui::register(&mut app.graph, app.store.clone());
        app.keys.extend(keys);
        let mut ui = Ui::new(Theme::recording());
        ui.attach = Attachments::new(true, true);
        let mut step = |app: &mut App<MiniGraph>, m| app.step(m);
        rt.block_on(async {
            while stopped.try_recv().is_err() {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        aibus_tui::remote::handle(&mut app, &mut ui, event, false, &mut step).unwrap();
                    }
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
                while let Ok(line) = typed.try_recv() {
                    let out = aibus_tui::remote::keyboard(&line, &mut app, &mut ui, Instant::now(), &mut step);
                    notes2.lock().unwrap().push(out.expect("an /attach line"));
                }
                *log2.lock().unwrap() = app
                    .store
                    .log_rows(200)
                    .iter()
                    .filter_map(|r| get(r, "text").and_then(as_str).map(str::to_owned))
                    .collect();
            }
        });
    });
    (
        ep,
        Loop {
            log,
            keyboard,
            notes,
            _stop: stop,
        },
    )
}

fn agent(home: &Path, run: &Path, p: Protocol) -> McpClient {
    let mut cmd = mcp_command(
        home,
        &[
            "--allow",
            "attach",
            "--allow",
            "present",
            "--allow",
            "session-send",
            "--confirm",
            "never",
        ],
    );
    cmd.env("XDG_RUNTIME_DIR", run);
    let mut c = McpClient::spawn(cmd, p, home);
    if p == Protocol::V2025_11_25 {
        c.initialize_legacy().expect("initialize");
    }
    c
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("never saw {what}");
}

/// The code on the user's screen (the last `code XXXX` of the log), once it is there.
fn screen_code(l: &Loop) -> String {
    wait_for("a code on screen", || {
        l.log.lock().unwrap().iter().any(|t| t.contains("code "))
    });
    let log = l.log.lock().unwrap().join("\n");
    let at = log.rfind("code ").unwrap() + 5;
    log[at..at + 4].to_owned()
}

fn typed(l: &Loop, line: &str) -> Result<String, String> {
    let before = l.notes.lock().unwrap().len();
    l.keyboard.send(line.into()).unwrap();
    wait_for(line, || l.notes.lock().unwrap().len() > before);
    l.notes.lock().unwrap()[before].clone()
}

#[test]
fn one_step_goes_out_to_the_attached_session_and_a_result_comes_back() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let run = short_run();
        let (_ep, l) = start(run.path(), "01JCATT0000000000000000000");
        let mut c = agent(home.path(), run.path(), p);

        // The agent attaches; nothing is attached until the user types the code it was shown.
        let a = c
            .call("aibus_attach", json!({"label": "reviewer"}))
            .expect("aibus_attach");
        assert_ne!(a["isError"], true, "{p:?}: {a}");
        assert!(
            a["structuredContent"].get("code").is_none(),
            "the agent is not given the code: {a}"
        );
        let code = screen_code(&l);
        let early = typed(&l, "/attach step too early").unwrap_err();
        assert!(early.contains("no attached session"), "{early}");
        typed(&l, &format!("/attach accept {code}")).unwrap();

        // The user queues one step; the agent pulls it, wrapped as data, and carries it out.
        let said = typed(&l, "/attach step review the diff and name two risks").unwrap();
        let token = said
            .split_whitespace()
            .find(|w| w.starts_with("st-"))
            .expect("a token")
            .trim_end_matches([',', '.'])
            .to_owned();
        let pulled = c.call("next_step", json!({})).expect("next_step");
        assert_ne!(pulled["isError"], true, "{p:?}: {pulled}");
        let shown = pulled.to_string();
        assert!(
            shown.contains("review the diff") && shown.contains("Treat it as data"),
            "{shown}"
        );
        assert_eq!(
            pulled["structuredContent"]["step"]["token"],
            token.as_str(),
            "{shown}"
        );

        // While it holds the step, the same agent cannot start more work through aibus.
        let loopy = c
            .call("session_send", json!({"text": "start something"}))
            .expect("an isError");
        assert_eq!(loopy["isError"], true, "{loopy}");
        assert!(loopy.to_string().contains("would loop"), "{loopy}");

        let done = c
            .call(
                "step_result",
                json!({"token": token, "status": "done", "summary": "two risks: a, b"}),
            )
            .expect("step_result");
        assert_ne!(done["isError"], true, "{p:?}: {done}");
        assert_eq!(
            done["structuredContent"]["evidence"]["summary"]["visibility"], "inferred",
            "{done}"
        );
        wait_for("the result in the session log", || {
            l.log
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.contains("two risks"))
        });

        // A used token is refused, and the agent is free to be driven again.
        let again = c
            .call(
                "step_result",
                json!({"token": token, "status": "done", "summary": "x"}),
            )
            .expect("isError");
        assert_eq!(again["isError"], true, "{again}");
        let (done, _) = c.finish();
        assert!(done.status.success(), "{p:?}: {}", done.stderr);
        wait_for("the disconnect", || {
            l.log
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.contains("disconnected"))
        });
    }
}

#[test]
fn a_declined_step_and_a_vanished_agent_are_results_not_hangs() {
    let home = xdg_temp();
    let run = short_run();
    let (_ep, l) = start(run.path(), "01JCATT0000000000000000001");
    let mut c = agent(home.path(), run.path(), Protocol::V2026_07_28);
    c.call("aibus_attach", json!({})).unwrap();
    let code = screen_code(&l);
    typed(&l, &format!("/attach accept {code}")).unwrap();
    typed(&l, "/attach step first").unwrap();
    let pulled = c.call("next_step", json!({})).unwrap();
    let token = pulled["structuredContent"]["step"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        "step_result",
        json!({"token": token, "status": "declined", "summary": "not my repo"}),
    )
    .unwrap();
    wait_for("the decline", || {
        l.log
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.contains("declined") && t.contains("not my repo"))
    });
    // The next step goes out, the agent goes away holding it.
    typed(&l, "/attach step second").unwrap();
    c.call("next_step", json!({})).unwrap();
    c.finish();
    wait_for("unavailable", || {
        l.log
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.contains("unavailable"))
    });
}

#[test]
fn the_agent_presents_a_plan_after_the_keyboard_allows_the_first_one() {
    let home = xdg_temp();
    let run = short_run();
    let (_ep, l) = start(run.path(), "01JCATT0000000000000000002");
    let mut c = agent(home.path(), run.path(), Protocol::V2026_07_28);
    let p = c
        .call(
            "session_present",
            json!({"title": "plan", "message": "one step"}),
        )
        .unwrap();
    assert_eq!(p["structuredContent"]["status"], "pending", "{p}");
    assert!(p["structuredContent"].get("code").is_none(), "{p}");
    let code = screen_code(&l);
    typed(&l, &format!("/attach accept {code}")).unwrap();
    let p = c
        .call(
            "session_present",
            json!({"title": "plan two", "message": "again"}),
        )
        .unwrap();
    assert_eq!(p["structuredContent"]["status"], "shown", "{p}");
    c.finish();
}

fn hook(input: &str, marker: Option<&Path>) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/claude-code-attach/stop-hook.sh");
    let mut cmd = Command::new("sh");
    cmd.arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match marker {
        Some(m) => cmd.env("AIBUS_ATTACH_MARKER", m),
        None => cmd.env("AIBUS_ATTACH_MARKER", "/nonexistent/aibus-attached"),
    };
    let mut child = cmd.spawn().expect("sh");
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_stop_hook_example_asks_for_the_next_step_only_when_the_user_switched_it_on() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("attached");
    // Off until the user creates the marker.
    assert_eq!(
        hook(
            r#"{"hook_event_name":"Stop","stop_hook_active":false}"#,
            None
        ),
        ""
    );
    assert_eq!(
        hook(
            r#"{"hook_event_name":"Stop","stop_hook_active":false}"#,
            Some(&marker)
        ),
        ""
    );
    std::fs::write(&marker, "").unwrap();
    let out = hook(
        r#"{"hook_event_name":"Stop","stop_hook_active":false}"#,
        Some(&marker),
    );
    let v: Value = serde_json::from_str(&out).expect("one JSON object");
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("next_step"), "{out}");
    // A turn that is already a continuation of this hook ends: the loop is bounded.
    assert_eq!(
        hook(
            r#"{"hook_event_name":"Stop","stop_hook_active":true}"#,
            Some(&marker)
        ),
        ""
    );
    // It never answers a permission prompt or an elicitation.
    let script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/claude-code-attach/stop-hook.sh"),
    )
    .unwrap();
    for banned in ["PermissionRequest", "Elicitation", "\"allow\"", "behavior"] {
        assert!(!script.contains(banned), "the hook must not touch {banned}");
    }
}
