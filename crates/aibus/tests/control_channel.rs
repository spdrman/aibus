//! The stdin control channel of `aibus run --json` (06 section 2): a headless session driven by
//! `Permission` and `SendMessage` lines.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use common::{command, schema};
use serde_json::{json, Value};
use aibus_testkit::TempDir;

/// A tool call that needs permission under `default` mode, then an answer that depends on what
/// the conversation holds.
const SCRIPT: &str = r#"[
    ToolCall(name: "bash", input: {"command": "echo ran > ran.txt"}),
    Stop(ToolUse),
    MatchPrompt(contains: "remember-marker", then: [Text("got the message"), Stop(EndTurn)]),
    Text("plain answer"),
]"#;

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
}

impl Session {
    fn start(home: &Path) -> Self {
        let script = home.join("ctl.ron");
        fs::write(&script, SCRIPT).unwrap();
        let mut child = command(home)
            .args(["run", "--json", "--agent", "general", "--script"])
            .arg(&script)
            .arg("go")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, out }
    }

    /// The next event; every one is checked against the schema on the way.
    fn next(&mut self) -> Option<Value> {
        let mut line = String::new();
        if self.out.read_line(&mut line).unwrap() == 0 {
            return None;
        }
        let v: Value = serde_json::from_str(&line).unwrap();
        schema::assert_valid("run-event", &v);
        Some(v)
    }

    fn until(&mut self, kind: &str) -> Value {
        while let Some(v) = self.next() {
            if v["type"] == kind {
                return v;
            }
        }
        panic!("no {kind} before the stream ended");
    }

    fn send(&mut self, v: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn rest(mut self) -> (Vec<Value>, i32) {
        let mut all = Vec::new();
        while let Some(v) = self.next() {
            all.push(v);
        }
        drop(self.stdin.take());
        let code = self.child.wait().unwrap().code().unwrap();
        (all, code)
    }
}

#[test]
fn a_permission_allow_and_a_message_drive_the_session() {
    let home = TempDir::new().unwrap();
    let mut s = Session::start(home.path());
    let started = s.until("RunStarted");
    let run = started["run"].as_str().unwrap().to_owned();

    let needed = s.until("PermissionNeeded");
    assert_eq!(needed["tool"], "bash");
    assert_eq!(needed["input"]["command"], "echo ran > ran.txt");
    // The message waits for the next turn; the allow lets the tool run now.
    s.send(&json!({"type": "SendMessage", "run": run, "text": "remember-marker please"}));
    s.send(&json!({"type": "Permission", "id": needed["id"], "decision": "allow"}));

    let (rest, code) = s.rest();
    let decided = rest.iter().find(|e| e["type"] == "PermissionDecided").unwrap();
    assert_eq!(decided["decision"], "allowed");
    assert_eq!(decided["id"], needed["id"]);
    assert!(decided["reason"].is_null());
    let fin = rest.iter().find(|e| e["type"] == "ToolCallFinished").unwrap();
    assert_eq!(fin["ok"], true);
    let done = rest.iter().find(|e| e["type"] == "RunFinished").unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["summary"], "got the message");
    assert_eq!(code, 0);
    assert_eq!(fs::read_to_string(home.path().join("proj/ran.txt")).unwrap(), "ran\n");
}

#[test]
fn a_permission_deny_keeps_the_tool_from_running() {
    let home = TempDir::new().unwrap();
    let mut s = Session::start(home.path());
    let needed = s.until("PermissionNeeded");
    s.send(&json!({"type": "Permission", "id": needed["id"], "decision": "deny"}));
    let (rest, _) = s.rest();
    let decided = rest.iter().find(|e| e["type"] == "PermissionDecided").unwrap();
    assert_eq!(decided["decision"], "denied");
    assert!(!home.path().join("proj/ran.txt").exists());
}

#[test]
fn bad_lines_are_reported_on_stderr_and_do_not_stop_the_session() {
    let home = TempDir::new().unwrap();
    let script = home.path().join("ctl.ron");
    fs::write(&script, SCRIPT).unwrap();
    let mut child = command(home.path())
        .args(["run", "--json", "--agent", "general", "--script"])
        .arg(&script)
        .arg("go")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut needed = Value::Null;
    let mut line = String::new();
    while needed.is_null() {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0);
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["type"] == "PermissionNeeded" {
            needed = v;
        }
    }
    writeln!(stdin, "this is not json").unwrap();
    writeln!(stdin, r#"{{"type":"Quit"}}"#).unwrap();
    writeln!(stdin, r#"{{"type":"Permission","id":{},"decision":"allow"}}"#, needed["id"]).unwrap();
    drop(stdin);
    let result = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&result.stderr);
    assert!(err.contains("control: not JSON"), "{err}");
    assert!(err.contains("control: unknown type"), "{err}");
    assert_eq!(result.status.code(), Some(0), "{err}");
}

#[test]
fn a_closed_stdin_refuses_what_would_ask_so_the_run_cannot_hang() {
    let home = TempDir::new().unwrap();
    let mut s = Session::start(home.path());
    let needed = s.until("PermissionNeeded");
    // Nobody is left to answer.
    drop(s.stdin.take());
    let (rest, code) = s.rest();
    let decided = rest.iter().find(|e| e["type"] == "PermissionDecided").unwrap();
    assert_eq!(decided["decision"], "denied");
    assert_eq!(decided["reason"], "no tty");
    assert_eq!(decided["id"], needed["id"]);
    assert!(rest.iter().any(|e| e["type"] == "RunFinished"));
    assert!(code == 0 || code == 6 || code == 10, "{code}");
}
