//! #274: the live-session tools. Each is behind its own `--allow` flag and is not listed without
//! it (a call is `-32602`). What comes back from the session goes through the redactor, a line or
//! a text past the cap never reaches the session, the calls that change the session share a rate
//! limit, every call leaves an audit line tagged `via: "mcp"` with the session, and the
//! confirmation asks the user to retype the session's short id.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibus_core::{AccountName, Redactor};
use aibus_mcp::{Caps, SessionLink, SESSION_CHANGES_PER_MINUTE, SESSION_TEXT_MAX};
use rmcp::model::{ElicitResult, ElicitationAction, JsonObject};
use serde_json::{json, Value};

const SESSION: &str = "01JCABCDEF0123456789XYZABC";
const KEY: &str = "sk-test-SESSION-0123456789abcdef";

/// A session that answers every request with what `answer` holds and keeps what it was sent.
#[derive(Default)]
struct Fake {
    sent: Mutex<Vec<(String, Value)>>,
    answer: Mutex<Option<Value>>,
}

impl SessionLink for Fake {
    fn list(&self) -> Result<Value, String> {
        Ok(
            json!({"sessions": [{"id": SESSION, "cwd": "~/app", "lead": "lead", "model": "sonnet", "started": "2026-11-14T10:00:00Z"}]}),
        )
    }

    fn target(&self) -> Result<String, String> {
        Ok(SESSION.into())
    }

    fn call(&self, client: &str, request: Value) -> Result<Value, String> {
        self.sent.lock().unwrap().push((client.into(), request));
        Ok(self
            .answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(json!({"accepted": true})))
    }
}

const TOOLS: [(&str, &str); 7] = [
    ("session-list", "session_list"),
    ("session-status", "session_status"),
    ("session-send", "session_send"),
    ("session-command", "session_command"),
    ("session-cancel", "session_cancel"),
    ("session-transcript", "session_transcript"),
    ("permission", "session_permission"),
];

fn caps(flags: &[&str]) -> Caps {
    let mut caps = Caps::default();
    for f in flags {
        caps.allow(f).unwrap();
    }
    caps
}

fn server(
    flags: &[&str],
    redactor: Redactor,
) -> (aibus_mcp::Server, aibus_testkit::Xdg, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let (s, home) = common::server(caps(flags), redactor);
    (s.with_session(fake.clone()), home, fake)
}

fn args(v: Value) -> Option<JsonObject> {
    v.as_object().cloned()
}

fn names(s: &aibus_mcp::Server) -> Vec<String> {
    s.tools().iter().map(|t| t.name.to_string()).collect()
}

#[test]
fn each_session_tool_is_listed_only_with_its_own_flag() {
    let (plain, _home, _) = server(&[], Redactor::new());
    let base = names(&plain);
    assert!(!base.iter().any(|n| n.starts_with("session_")), "{base:?}");
    for (flag, tool) in TOOLS {
        let (s, _home, fake) = server(&[flag], Redactor::new());
        let got = names(&s);
        let extra: Vec<&String> = got.iter().filter(|n| !base.contains(n)).collect();
        assert_eq!(extra, [tool], "--allow {flag}");
        // Every other session tool is unknown, the same as a tool that never existed.
        for (_, off) in TOOLS.iter().filter(|(_, t)| *t != tool) {
            let e = s
                .call(None, off, args(json!({})), Instant::now())
                .unwrap_err();
            assert_eq!(e.code.0, -32602, "{off} with --allow {flag}");
        }
        assert!(
            fake.sent.lock().unwrap().is_empty(),
            "nothing reached the session"
        );
    }
    // A flag the server does not know is a usage error, not a silent no-op.
    assert!(Caps::default().allow("session-everything").is_err());
}

#[test]
fn what_the_session_answers_is_redacted() {
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), KEY);
    let flags: Vec<&str> = TOOLS.iter().map(|(f, _)| *f).collect();
    let (s, _home, fake) = server(&flags, redactor);
    *fake.answer.lock().unwrap() =
        Some(json!({"transcript": format!("the key is {KEY}"), "cards": [{"agent": KEY}]}));
    for (tool, a) in [
        ("session_status", json!({})),
        ("session_transcript", json!({})),
        ("session_send", json!({"text": "hello"})),
    ] {
        let got = s.call(None, tool, args(a), Instant::now()).unwrap();
        let text = serde_json::to_string(&got).unwrap();
        assert!(!text.contains(KEY), "{tool}: {text}");
        assert!(
            text.contains("[redacted]") || text.contains("REDACTED"),
            "{tool}: {text}"
        );
    }
    let listed = s
        .call(None, "session_list", args(json!({})), Instant::now())
        .unwrap();
    assert_eq!(
        listed.structured_content.unwrap()["sessions"][0]["id"],
        SESSION
    );
}

#[test]
fn each_tool_sends_the_request_its_arguments_name() {
    let flags: Vec<&str> = TOOLS.iter().map(|(f, _)| *f).collect();
    let (s, _home, fake) = server(&flags, Redactor::new());
    let id = aibus_core::PermissionId::new().to_string();
    let calls = [
        ("session_status", json!({}), json!({"op": "status"})),
        (
            "session_send",
            json!({"text": "fix it"}),
            json!({"op": "send", "text": "fix it"}),
        ),
        (
            "session_command",
            json!({"line": "/model haiku"}),
            json!({"op": "command", "line": "/model haiku"}),
        ),
        (
            "session_cancel",
            json!({"run": "search"}),
            json!({"op": "cancel", "run": "search"}),
        ),
        (
            "session_transcript",
            json!({"run": "search"}),
            json!({"op": "transcript", "run": "search"}),
        ),
        (
            "session_permission",
            json!({"id": id, "decision": "deny"}),
            json!({"op": "permission", "id": id, "decision": "deny"}),
        ),
    ];
    for (tool, a, _) in &calls {
        let got = s.call(None, tool, args(a.clone()), Instant::now()).unwrap();
        assert_ne!(got.is_error, Some(true), "{tool}: {got:?}");
    }
    let sent: Vec<Value> = fake
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, r)| r.clone())
        .collect();
    let want: Vec<Value> = calls.iter().map(|(_, _, w)| w.clone()).collect();
    assert_eq!(sent, want);
    // Arguments the schema does not have are invalid, and reach nothing.
    let e = s
        .call(
            None,
            "session_send",
            args(json!({"text": "x", "as": "user"})),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(e.code.0, -32602);
    let e = s
        .call(
            None,
            "session_send",
            args(json!({"text": 7})),
            Instant::now(),
        )
        .unwrap_err();
    assert_eq!(e.code.0, -32602);
    assert_eq!(fake.sent.lock().unwrap().len(), calls.len());
}

#[test]
fn text_past_the_cap_never_reaches_the_session() {
    let (s, _home, fake) = server(&["session-send", "session-command"], Redactor::new());
    let long = "a".repeat(SESSION_TEXT_MAX + 1);
    let got = s
        .call(
            None,
            "session_send",
            args(json!({"text": long})),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(got.is_error, Some(true));
    let got = s
        .call(
            None,
            "session_command",
            args(json!({"line": format!("/{long}")})),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(got.is_error, Some(true));
    assert!(fake.sent.lock().unwrap().is_empty());
    let fits = "a".repeat(SESSION_TEXT_MAX);
    let got = s
        .call(
            None,
            "session_send",
            args(json!({"text": fits})),
            Instant::now(),
        )
        .unwrap();
    assert_ne!(got.is_error, Some(true));
}

#[test]
fn the_calls_that_change_the_session_share_a_rate_limit() {
    let (s, _home, fake) = server(
        &["session-send", "session-cancel", "session-status"],
        Redactor::new(),
    );
    let now = Instant::now();
    for i in 0..SESSION_CHANGES_PER_MINUTE {
        let tool = if i % 2 == 0 {
            "session_send"
        } else {
            "session_cancel"
        };
        let a = if i % 2 == 0 {
            json!({"text": "go"})
        } else {
            json!({"run": "lead"})
        };
        let got = s.call(None, tool, args(a), now).unwrap();
        assert_ne!(got.is_error, Some(true), "call {i}");
    }
    let got = s
        .call(None, "session_send", args(json!({"text": "one more"})), now)
        .unwrap();
    assert_eq!(got.is_error, Some(true));
    let said = serde_json::to_string(&got).unwrap();
    assert!(said.contains("rate limit"), "{said}");
    // Reading is not a change, and the bucket refills.
    let got = s
        .call(None, "session_status", args(json!({})), now)
        .unwrap();
    assert_ne!(got.is_error, Some(true));
    let later = now + Duration::from_secs(60);
    let got = s
        .call(None, "session_send", args(json!({"text": "later"})), later)
        .unwrap();
    assert_ne!(got.is_error, Some(true));
    assert_eq!(
        fake.sent.lock().unwrap().len() as u32,
        SESSION_CHANGES_PER_MINUTE + 2
    );
}

#[test]
fn a_session_call_leaves_an_audit_line_tagged_with_the_session() {
    let (s, home, _) = server(&["session-send"], Redactor::new());
    s.call(
        None,
        "session_send",
        args(json!({"text": "fix it"})),
        Instant::now(),
    )
    .unwrap();
    let lines = common::audit_lines(&home);
    let line = lines
        .iter()
        .find(|l| l["tool"] == "session_send")
        .expect("a line for the call");
    assert_eq!(line["via"], "mcp", "{line}");
    assert_eq!(line["session"], SESSION, "{line}");
    // The start line records the session flags with the rest.
    assert_eq!(lines[0]["method"], "server/start");
    assert_eq!(lines[0]["args"]["session_send"], true, "{}", lines[0]);
    assert_eq!(lines[0]["args"]["permission"], false, "{}", lines[0]);
}

#[test]
fn the_confirmation_asks_to_retype_the_session_short_id() {
    let (s, _home, _) = server(&["session-send", "session-status"], Redactor::new());
    let a = json!({"text": "fix it"});
    let form = s
        .confirmation_form("session_send", &a)
        .expect("a form for a change");
    let shown = serde_json::to_string(&form).unwrap();
    assert!(shown.contains(&SESSION[..8]), "{shown}");
    assert!(
        s.confirmation_form("session_status", &json!({})).is_none(),
        "reading asks nothing"
    );
    let answer = |typed: &str| ElicitResult {
        action: ElicitationAction::Accept,
        content: Some(json!({"name": typed})),
        meta: None,
    };
    assert!(s
        .confirmation_check("session_send", &a, &answer(&SESSION[..8]))
        .is_ok());
    assert!(s
        .confirmation_check("session_send", &a, &answer("01JCXXXX"))
        .is_err());
}
