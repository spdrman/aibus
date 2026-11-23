//! The session list.
//!
//! `claude agents --json` is the contract (documented). The files under `<config>/sessions/` are
//! local, not documented: they only fill a field the command left out of a session it listed.
//! They never add a session, a `.key` file is never opened, and a file is used only when it is
//! a regular file of this user, names a pid that is alive and agrees with the pid the command gave.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use aibus_trace::view::clean;
use serde_json::Value;

/// One live session as `claude agents --json` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    /// The background job id the CLI gives a background session.
    pub job_id: Option<String>,
    pub name: String,
    pub cwd: String,
    /// `interactive` or `background`, as the CLI says.
    pub kind: String,
    pub started_ms: Option<i64>,
    /// `state` (background) or `status` (interactive), as the CLI says, or `unknown`.
    pub state: String,
    pub pid: Option<u32>,
    /// Only when a local session file carries one; the command does not.
    pub version: Option<String>,
    /// The inbox socket the session's local file names; the command does not list it.
    pub socket: Option<std::path::PathBuf>,
}

const MAX_JSON: usize = 4 << 20;
const MAX_LOCAL_FILE: u64 = 64 << 10;

/// Parses the output of `claude agents --json`. An entry without a session id is skipped.
pub fn parse_agents(json: &[u8]) -> Result<Vec<Session>, String> {
    let v: Value = serde_json::from_slice(json)
        .map_err(|e| format!("`claude agents --json` did not print JSON: {e}"))?;
    let Value::Array(items) = v else {
        return Err("`claude agents --json` did not print an array".into());
    };
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(items
        .iter()
        .filter_map(|v| {
            let session_id = text(v, "sessionId").filter(|s| !s.is_empty())?;
            Some(Session {
                session_id,
                job_id: text(v, "id"),
                name: text(v, "name").unwrap_or_default(),
                cwd: text(v, "cwd").unwrap_or_default(),
                kind: text(v, "kind").unwrap_or_else(|| "unknown".into()),
                started_ms: v.get("startedAt").and_then(Value::as_i64),
                state: text(v, "state")
                    .or_else(|| text(v, "status"))
                    .unwrap_or_else(|| "unknown".into()),
                pid: v
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok()),
                version: None,
                socket: None,
            })
        })
        .collect())
}

/// Fills fields the command lacked from `<config>/sessions/*.json`; never adds a session.
pub fn merge_local(list: &mut [Session], sessions_dir: &Path) {
    let Ok(dir) = std::fs::read_dir(sessions_dir) else {
        return;
    };
    let mut paths: Vec<_> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        // Only `*.json`: a `.key` file is never opened, whatever it is called.
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    for p in paths.into_iter().take(500) {
        let Some(v) = crate::read_own(&p, MAX_LOCAL_FILE)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        let Some(id) = v.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        let Some(s) = list.iter_mut().find(|s| s.session_id == id) else {
            continue;
        };
        let pid = v
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok());
        // A pid is trusted only if the process is there, and it must be the one the CLI named.
        let Some(pid) = pid.filter(|p| alive(*p)) else {
            continue;
        };
        if s.pid.is_some_and(|known| known != pid) {
            continue;
        }
        s.pid.get_or_insert(pid);
        if s.version.is_none() {
            s.version = v.get("version").and_then(Value::as_str).map(str::to_owned);
        }
        if s.socket.is_none() {
            s.socket = v
                .get("messagingSocketPath")
                .and_then(Value::as_str)
                .and_then(|p| p.strip_prefix("uds:"))
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute());
        }
    }
}

fn alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
}

fn ago(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// The list as text: what each session is and what aibus can do with it. Send needs an inbox socket.
pub fn render_list(list: &[Session], tapped: &[String], now_ms: i64) -> String {
    if list.is_empty() {
        return "no live Claude Code sessions\n".into();
    }
    let mut out = format!(
        "{:<9} {:<8} {:<11} {:<22} {:<9} {:<14} {}\n",
        "SESSION", "CLIENT", "KIND", "NAME", "STATE", "STARTED", "CAN DO / CWD"
    );
    for s in list {
        let id: String = s.session_id.chars().take(8).collect();
        let started = s
            .started_ms
            .map_or_else(|| "unknown".into(), |t| ago(now_ms - t));
        let can = match (s.socket.is_some(), tapped.contains(&s.session_id)) {
            (true, true) => "watch + send (tapped)",
            (true, false) => "watch + send",
            (false, true) => "watch (tapped)",
            (false, false) => "watch",
        };
        out.push_str(&format!(
            "{:<9} {:<8} {:<11} {:<22} {:<9} {:<14} {} {}\n",
            clean(&id, 8),
            "claude",
            clean(&s.kind, 11),
            clean(&s.name, 22),
            clean(&s.state, 9),
            started,
            can,
            clean(&s.cwd, 80),
        ));
    }
    out
}

/// Runs `claude agents --json` (read-only, ten seconds at most) and returns its stdout.
pub fn run_agents_json() -> Result<Vec<u8>, String> {
    let mut child = Command::new("claude")
        .args(["agents", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot run `claude agents --json`: {e} (is Claude Code installed and on PATH?)"
            )
        })?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_JSON as u64 + 1)
            .read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let out = rx.recv_timeout(Duration::from_secs(10)).map_err(|_| {
        let _ = child.kill();
        let _ = child.wait();
        "`claude agents --json` took more than 10 seconds".to_owned()
    })?;
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("`claude agents --json` exited with {status}"));
    }
    if out.len() > MAX_JSON {
        return Err("`claude agents --json` printed more than 4 MiB".into());
    }
    Ok(out)
}

/// The one session `arg` names: a session id, a unique prefix of one, a job id or a unique name.
pub fn resolve<'a>(list: &'a [Session], arg: &str) -> Result<&'a Session, String> {
    if arg.is_empty() {
        return Err("name a session: an id, a prefix of one, a job id or a name".into());
    }
    let exact: Vec<_> = list
        .iter()
        .filter(|s| s.session_id == arg || s.job_id.as_deref() == Some(arg))
        .collect();
    let hits = if exact.is_empty() {
        let by_prefix: Vec<_> = list
            .iter()
            .filter(|s| s.session_id.starts_with(arg))
            .collect();
        if by_prefix.is_empty() {
            list.iter().filter(|s| s.name == arg).collect()
        } else {
            by_prefix
        }
    } else {
        exact
    };
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no live session matches {}", clean(arg, 80))),
        _ => Err(format!(
            "{} matches more than one session; use more of the id",
            clean(arg, 80)
        )),
    }
}
