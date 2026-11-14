//! Both ends against each other: an `Endpoint` served on a runtime of its own, a stand-in for the
//! terminal loop that answers what reaches it, and a `Link` called from the test thread the way
//! `aibus mcp` calls it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibus_core::remote::{Ask, Remote};
use aibus_core::{AccountName, Redactor};
use aibus_mcp::SessionLink;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::*;

const KEY: &str = "sk-test-CONTROL-0123456789abcdef";

/// What the stand-in loop saw: `connected <client>`, `call <client> <ask>`, `disconnected <client>`.
type Seen = Arc<Mutex<Vec<String>>>;

struct Live {
    _rt: tokio::runtime::Runtime,
    dir: PathBuf,
    _home: aibus_testkit::Xdg,
    seen: Seen,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A session directory in a throwaway home, made the way the binary makes it.
fn sessions(home: &aibus_testkit::Xdg) -> PathBuf {
    let dir = runtime_dir(Some(home.path().join("run").as_os_str()), home.path());
    private_dir(&dir).unwrap();
    dir
}

fn info(id: &str) -> Value {
    json!({"id": id, "cwd": "~/app", "lead": "lead", "model": "sonnet", "started": format!("2026-11-14T10:00:0{}Z", id.len() % 10)})
}

/// The stand-in loop: every call is answered with `{"ask": <debug>, "key": KEY}`, so a test can
/// see that the answer is redacted on the way out.
async fn stand_in(mut rx: mpsc::Receiver<Remote>, seen: Seen) {
    while let Some(event) = rx.recv().await {
        let line = match event {
            Remote::Connected { client } => format!("connected {client}"),
            Remote::Disconnected { client } => format!("disconnected {client}"),
            Remote::Call { client, ask, reply } => {
                let said = format!("{ask:?}");
                let _ = reply.send(match &ask {
                    Ask::Command { line } if line.starts_with("/login") => {
                        Err(format!("refused {KEY}"))
                    }
                    _ => Ok(json!({"ask": said, "key": KEY})),
                });
                format!("call {client} {said}")
            }
        };
        seen.lock().unwrap().push(line);
    }
}

fn live_with(id: &str, tweak: impl FnOnce(&mut Endpoint)) -> (Live, Endpoint) {
    let home = aibus_testkit::xdg_temp();
    let dir = sessions(&home);
    let rt = runtime();
    let seen: Seen = Arc::default();
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), KEY);
    let mut ep = Endpoint::open(&dir, id, &info(id)).unwrap();
    tweak(&mut ep);
    let (tx, rx) = mpsc::channel(16);
    rt.spawn(stand_in(rx, seen.clone()));
    {
        let _in = rt.enter();
        ep.serve(tx, redactor).unwrap();
    }
    (
        Live {
            _rt: rt,
            dir,
            _home: home,
            seen,
        },
        ep,
    )
}

fn live(id: &str) -> (Live, Endpoint) {
    live_with(id, |_| {})
}

fn wait_for(seen: &Seen, what: &str) -> bool {
    for _ in 0..100 {
        if seen.lock().unwrap().iter().any(|l| l.contains(what)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

/// A raw connection that says `hello` with `token`, and the first line it gets back.
fn raw_hello(socket: &Path, token: &str) -> (UnixStream, Value) {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(
        s,
        "{}",
        json!({"hello": 1, "token": token, "client": "raw"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(s.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    (s, serde_json::from_str(&line).unwrap_or(Value::Null))
}

#[test]
fn the_socket_token_and_info_are_private_and_gone_after_drop() {
    let (l, ep) = live("01JCAAAAAAAAAAAAAAAAAAAAAA");
    assert_eq!(mode(&l.dir), 0o700);
    for path in [ep.socket(), ep.token_file(), ep.info_file()] {
        assert!(path.starts_with(&l.dir), "{}", path.display());
        assert_eq!(mode(&path), 0o600, "{}", path.display());
    }
    let token = std::fs::read_to_string(ep.token_file()).unwrap();
    assert_eq!(token.trim().len(), 64, "32 random bytes as hex");
    let (socket, token_file, info_file) = (ep.socket(), ep.token_file(), ep.info_file());
    drop(ep);
    for path in [socket, token_file, info_file] {
        assert!(!path.exists(), "{} is left behind", path.display());
    }
    drop(l);
}

#[test]
fn a_stale_socket_from_a_crash_is_replaced_and_swept() {
    let home = aibus_testkit::xdg_temp();
    let dir = sessions(&home);
    // A session that died without cleaning up: its socket file is there but nobody listens.
    for stale in ["01JCSTALE0000000000000000A", "01JCSTALE0000000000000000B"] {
        let listener =
            std::os::unix::net::UnixListener::bind(dir.join(format!("{stale}.sock"))).unwrap();
        drop(listener);
        std::fs::write(dir.join(format!("{stale}.token")), "x").unwrap();
        std::fs::write(dir.join(format!("{stale}.json")), info(stale).to_string()).unwrap();
    }
    // The same id comes back (a resumed session) and a new one starts: both open.
    let again = Endpoint::open(
        &dir,
        "01JCSTALE0000000000000000A",
        &info("01JCSTALE0000000000000000A"),
    )
    .unwrap();
    assert!(again.socket().exists());
    let other = Endpoint::open(
        &dir,
        "01JCNEW0000000000000000000",
        &info("01JCNEW0000000000000000000"),
    )
    .unwrap();
    // The other stale session's files were swept on the way.
    for ext in ["sock", "token", "json"] {
        assert!(
            !dir.join(format!("01JCSTALE0000000000000000B.{ext}"))
                .exists(),
            "{ext}"
        );
    }
    // And a live one is never swept or taken over.
    let rt = runtime();
    let (tx, _rx) = mpsc::channel(4);
    {
        let _in = rt.enter();
        let mut other = other;
        other.serve(tx, Redactor::new()).unwrap();
        let err = Endpoint::open(&dir, "01JCNEW0000000000000000000", &info("x")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
        let _third = Endpoint::open(&dir, "01JCTHIRD00000000000000000", &info("y")).unwrap();
        assert!(other.socket().exists());
    }
    drop(again);
}

#[test]
fn a_connection_without_the_right_token_is_refused_and_never_reaches_the_session() {
    let (l, ep) = live("01JCTOKEN0000000000000000A");
    let (_s, answer) = raw_hello(&ep.socket(), "not-the-token");
    assert_eq!(answer["ok"], false, "{answer}");
    let (_s, answer) = raw_hello(&ep.socket(), "");
    assert_eq!(answer["ok"], false, "{answer}");
    // Garbage before a hello, and nothing at all, are closed too.
    let mut s = UnixStream::connect(ep.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    writeln!(s, "{}", json!({"id": 1, "op": "status"})).unwrap();
    let mut line = String::new();
    let _ = BufReader::new(s).read_line(&mut line);
    assert!(!line.contains("\"ok\":true"), "{line}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        l.seen.lock().unwrap().is_empty(),
        "{:?}",
        l.seen.lock().unwrap()
    );
    // The real token gets in.
    let token = std::fs::read_to_string(ep.token_file()).unwrap();
    let (_s, answer) = raw_hello(&ep.socket(), token.trim());
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["session"], "01JCTOKEN0000000000000000A");
}

#[test]
fn a_peer_that_is_another_user_is_refused() {
    let (l, ep) = live_with("01JCPEER00000000000000000A", |ep| {
        ep.uid = ep.uid.wrapping_add(1)
    });
    let token = std::fs::read_to_string(ep.token_file()).unwrap();
    let mut s = UnixStream::connect(ep.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = writeln!(
        s,
        "{}",
        json!({"hello": 1, "token": token.trim(), "client": "raw"})
    );
    let mut line = String::new();
    let _ = BufReader::new(s).read_line(&mut line);
    assert!(!line.contains("\"ok\":true"), "{line}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        l.seen.lock().unwrap().is_empty(),
        "{:?}",
        l.seen.lock().unwrap()
    );
}

#[test]
fn one_client_controls_the_session_at_a_time() {
    let (l, ep) = live("01JCONE00000000000000000AA");
    let token = std::fs::read_to_string(ep.token_file()).unwrap();
    let (first, answer) = raw_hello(&ep.socket(), token.trim());
    assert_eq!(answer["ok"], true);
    let (_second, answer) = raw_hello(&ep.socket(), token.trim());
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("another client"),
        "{answer}"
    );
    // Once the first one goes, the next may come in, and the session saw both ends of the first.
    drop(first);
    assert!(
        wait_for(&l.seen, "disconnected raw"),
        "{:?}",
        l.seen.lock().unwrap()
    );
    let (_third, answer) = raw_hello(&ep.socket(), token.trim());
    assert_eq!(answer["ok"], true, "{answer}");
    let seen = l.seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter()
            .filter(|l| l.starts_with("connected raw"))
            .count(),
        2,
        "{seen:?}"
    );
}

#[test]
fn a_line_past_the_cap_is_refused_and_the_connection_closed() {
    let (l, ep) = live("01JCSIZE00000000000000000A");
    let token = std::fs::read_to_string(ep.token_file()).unwrap();
    let (mut s, answer) = raw_hello(&ep.socket(), token.trim());
    assert_eq!(answer["ok"], true);
    let big = json!({"id": 1, "op": "send", "text": "a".repeat(LINE_MAX)});
    let _ = writeln!(s, "{big}");
    let mut line = String::new();
    let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
    assert!(
        line.contains("\"ok\":false") && line.contains("bytes"),
        "{line}"
    );
    assert!(wait_for(&l.seen, "disconnected raw"));
    assert!(
        !l.seen.lock().unwrap().iter().any(|x| x.starts_with("call")),
        "nothing reached the session"
    );
}

#[test]
fn past_the_rate_limit_a_change_is_answered_with_an_error() {
    let (l, ep) = live("01JCRATE00000000000000000A");
    let token = std::fs::read_to_string(ep.token_file()).unwrap();
    let (mut s, _) = raw_hello(&ep.socket(), token.trim());
    let mut reader = BufReader::new(s.try_clone().unwrap());
    let mut errors = 0;
    for i in 0..CHANGES_PER_MINUTE + 3 {
        writeln!(s, "{}", json!({"id": i, "op": "send", "text": "go"})).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], i);
        if v["ok"] == false {
            assert!(v["error"].as_str().unwrap().contains("rate limit"), "{v}");
            errors += 1;
        }
    }
    assert_eq!(errors, 3);
    // A read is not a change.
    writeln!(s, "{}", json!({"id": 99, "op": "status"})).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("\"ok\":true"), "{line}");
    let calls = l
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|x| x.starts_with("call"))
        .count() as u32;
    assert_eq!(calls, CHANGES_PER_MINUTE + 1);
}

#[test]
fn the_link_round_trips_and_every_answer_is_redacted() {
    let (l, ep) = live("01JCLINK00000000000000000A");
    let link = Link::new(l.dir.clone(), None);
    assert_eq!(link.target().unwrap(), "01JCLINK00000000000000000A");
    let got = link
        .call("editor", json!({"op": "send", "text": "fix it"}))
        .unwrap();
    assert!(got["ask"].as_str().unwrap().contains("fix it"), "{got}");
    assert!(!got.to_string().contains(KEY), "{got}");
    let why = link
        .call(
            "editor",
            json!({"op": "command", "line": "/login anthropic"}),
        )
        .unwrap_err();
    assert!(!why.contains(KEY), "{why}");
    for request in [
        json!({"op": "status"}),
        json!({"op": "cancel", "run": "search"}),
        json!({"op": "transcript"}),
        json!({"op": "permission", "prompt": "x", "decision": "deny"}),
    ] {
        link.call("editor", request.clone())
            .unwrap_or_else(|e| panic!("{request}: {e}"));
    }
    let seen = l.seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter()
            .filter(|x| x.starts_with("connected editor"))
            .count(),
        1,
        "one connection for every call: {seen:?}"
    );
    assert!(link.call("editor", json!({"op": "reboot"})).is_err());
    drop(link);
    assert!(wait_for(&l.seen, "disconnected editor"));
    drop(ep);
}

#[test]
fn without_an_id_the_link_takes_the_only_live_session_and_refuses_when_there_are_several() {
    let home = aibus_testkit::xdg_temp();
    let dir = sessions(&home);
    let rt = runtime();
    let none = Link::new(dir.clone(), None);
    assert!(
        none.target().unwrap_err().contains("no live"),
        "{}",
        none.target().unwrap_err()
    );

    let mut endpoints = Vec::new();
    for id in ["01JCMANY00000000000000000A", "01JCMANY00000000000000000B"] {
        let mut ep = Endpoint::open(&dir, id, &info(id)).unwrap();
        let (tx, rx) = mpsc::channel(4);
        rt.spawn(stand_in(rx, Arc::default()));
        let _in = rt.enter();
        ep.serve(tx, Redactor::new()).unwrap();
        endpoints.push(ep);
        let one = Link::new(dir.clone(), None);
        if endpoints.len() == 1 {
            assert_eq!(one.target().unwrap(), id);
        } else {
            let why = one.target().unwrap_err();
            assert!(
                why.contains("01JCMANY00000000000000000A")
                    && why.contains("01JCMANY00000000000000000B"),
                "{why}"
            );
            assert!(why.contains("--session"), "{why}");
        }
    }
    // `--session <id>` picks one, and an id that is not live is refused.
    let picked = Link::new(dir.clone(), Some("01JCMANY00000000000000000B".into()));
    assert_eq!(picked.target().unwrap(), "01JCMANY00000000000000000B");
    assert!(picked.call("editor", json!({"op": "status"})).is_ok());
    let gone = Link::new(dir.clone(), Some("01JCNOPE00000000000000000A".into()));
    assert!(gone.target().is_err());
    let listed = none.list().unwrap();
    let ids: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        ["01JCMANY00000000000000000A", "01JCMANY00000000000000000B"]
    );
    // A session that ended is no longer listed.
    endpoints.remove(0);
    let listed = none.list().unwrap();
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(none.target().unwrap(), "01JCMANY00000000000000000B");
}

#[test]
fn a_session_directory_that_is_not_private_is_refused() {
    let home = aibus_testkit::xdg_temp();
    let dir = home.path().join("open");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(private_dir(&dir).is_err());
    assert!(Endpoint::open(&dir, "01JCOPEN00000000000000000A", &info("x")).is_err());
    let link = Link::new(dir, None);
    assert!(link.target().is_err());
}
