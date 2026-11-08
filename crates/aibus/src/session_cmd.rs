//! `aibus session list | show | log`: reads what a headless run leaves in its session directory
//! (`session.json`, `commits.jsonl`). `resume <id> --run <run>` starts a headless run that
//! continues one run from its transcript; `export`, `replay` and `prune`, and resuming a whole
//! session, arrive with the persistence task.

use std::path::{Path, PathBuf};

use aibus_core::{AibusError, Redactor, RunId, SessionId};
use serde_json::{json, Value};

use crate::cli::{Global, RunArgs, SessionCmd};

fn sessions_dir(g: &Global) -> Result<PathBuf, AibusError> {
    Ok(g.paths()?.state.join("sessions"))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The session directory for an id the user typed. Only a well-formed id gets to name a path.
fn find(g: &Global, id: &str) -> Result<PathBuf, AibusError> {
    id.parse::<SessionId>()
        .map_err(|_| AibusError::Usage(format!("session {id}: not a session id")))?;
    let dir = sessions_dir(g)?.join(id);
    if dir.join("session.json").is_file() {
        Ok(dir)
    } else {
        Err(AibusError::Usage(format!("no session {id}")))
    }
}

/// The cost of a session: the sum over its runs.
fn cost(session: &Value) -> f64 {
    session["runs"]
        .as_array()
        .map_or(0.0, |runs| runs.iter().filter_map(|r| r["usage"]["cost_usd"].as_f64()).sum())
}

/// `session resume <id> --run <run> "<message>"` as the arguments of a headless run that
/// continues that run in that session (01 section 5.4).
pub fn resume_args(
    g: &Global,
    id: &str,
    run: &str,
    message: Option<String>,
) -> Result<RunArgs, AibusError> {
    let dir = find(g, id)?;
    let run_id: RunId = run
        .parse()
        .map_err(|_| AibusError::Usage(format!("run {run}: not a run id")))?;
    if !dir.join("runs").join(run).join("transcript.jsonl").is_file() {
        return Err(AibusError::Usage(format!("session {id} has no transcript for run {run}")));
    }
    let message = message
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| AibusError::Usage("session resume --run <run> needs a message".into()))?;
    Ok(RunArgs {
        prompt: Some(message),
        session_dir: Some(dir),
        resume: Some(run_id),
        ..RunArgs::default()
    })
}

pub fn session(cmd: &SessionCmd, g: &Global, redactor: &Redactor) -> Result<u8, AibusError> {
    match cmd {
        SessionCmd::List { names } => {
            let mut rows = Vec::new();
            for entry in std::fs::read_dir(sessions_dir(g)?).into_iter().flatten().flatten() {
                if let Some(s) = read_json(&entry.path().join("session.json")) {
                    rows.push(s);
                }
            }
            // Ids are ULIDs: sorting by id is sorting by start.
            rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            if *names {
                for s in &rows {
                    println!("{}", s["id"].as_str().unwrap_or_default());
                }
            } else if g.json {
                let mut all = Value::Array(rows
                    .iter()
                    .map(|s| {
                        json!({
                            "id": s["id"],
                            "started_at": s["started"],
                            "ended_at": s["ended"],
                            "topic": s["topic"],
                            "runs": s["runs"].as_array().map_or(0, Vec::len),
                            "outcome": s["outcome"],
                            "cost_usd": cost(s),
                        })
                    })
                    .collect());
                redactor.redact_json(&mut all);
                println!("{all}");
            } else {
                for s in &rows {
                    let text = |k: &str| s[k].as_str().unwrap_or("-").to_owned();
                    println!(
                        "{}  {}  {}  {}",
                        text("id"),
                        text("started"),
                        text("outcome"),
                        text("topic")
                    );
                }
            }
        }
        SessionCmd::Show { id } => {
            let dir = find(g, id)?;
            let s = read_json(&dir.join("session.json"))
                .ok_or_else(|| AibusError::Usage(format!("session {id}: session.json is unreadable")))?;
            if g.json {
                let mut shown = json!({
                    "session": s,
                    "runs": s["runs"],
                    "handoff": null,
                    "ledger": null,
                });
                redactor.redact_json(&mut shown);
                println!("{shown}");
            } else {
                let text = |k: &str| s[k].as_str().unwrap_or("-").to_owned();
                println!("session  {}", text("id"));
                println!("started  {}", text("started"));
                println!("ended    {}", text("ended"));
                println!("topic    {}", text("topic"));
                println!("outcome  {}", text("outcome"));
                for r in s["runs"].as_array().into_iter().flatten() {
                    println!(
                        "run      {} {} {} ({} turns)",
                        r["id"].as_str().unwrap_or("-"),
                        r["agent"].as_str().unwrap_or("-"),
                        r["status"].as_str().unwrap_or("-"),
                        r["turns"]
                    );
                }
            }
        }
        SessionCmd::Log { id } => {
            // Already NDJSON, in the shape 06 section 4 gives; printed as it is.
            let dir = find(g, id)?;
            print!("{}", std::fs::read_to_string(dir.join("commits.jsonl")).unwrap_or_default());
        }
        SessionCmd::Resume { .. }
        | SessionCmd::Export { .. }
        | SessionCmd::Replay { .. }
        | SessionCmd::Prune { .. } => {
            eprintln!("aibus: session: not yet");
            return Ok(2);
        }
    }
    Ok(0)
}
