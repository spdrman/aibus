//! The `session` subcommands (06 section 1): they read what `aibus run` left in the state
//! directory, and the index that summarises it.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aibus_core::config;
use aibus_core::persist::commits;
use aibus_core::persist::frames::read_lines;
use aibus_core::persist::index::Index;
use aibus_core::persist::paths::{project_dir, Paths};
use aibus_core::persist::retention::{parse_age, prune, worktrees};
use aibus_core::persist::session::{load, read_snapshot, SessionMeta, MSGS};
use aibus_core::replay::replay_hash;
use aibus_core::state::value::{get, value_to_json};
use aibus_core::state::{HANDOFF_LATEST, LEDGER};
use aibus_core::{AibusError, App, Redactor, RunId, SessionId};
use aibus_graph::mini::MiniGraph;
use serde_json::{json, Value as Json};

use crate::cli::{Global, RunArgs, SessionCmd};
use crate::startup::now_ms;

/// `session resume <id> --run <run> "<message>"` as the arguments of a headless run that
/// continues that run in that session (01 section 5.4).
pub fn resume_args(
    g: &Global,
    id: &str,
    run: &str,
    message: Option<String>,
) -> Result<RunArgs, AibusError> {
    id.parse::<SessionId>()
        .map_err(|_| AibusError::Usage(format!("session {id}: not a session id")))?;
    let dir = g.paths()?.state.join("sessions").join(id);
    if !dir.join("session.json").is_file() {
        return Err(AibusError::Usage(format!("no session {id}")));
    }
    let run_id: RunId = run
        .parse()
        .map_err(|_| AibusError::Usage(format!("run {run}: not a run id")))?;
    if !dir.join("runs").join(run).join("conversation.jsonl").is_file() {
        return Err(AibusError::Usage(format!("session {id} has no saved conversation for run {run}")));
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
    let paths = g.paths()?;
    let mut out = std::io::stdout().lock();
    // Only a well-formed id gets to name a path.
    let dir_of = |id: &str| -> Result<PathBuf, AibusError> {
        id.parse::<SessionId>()
            .map_err(|_| AibusError::Usage(format!("session {id}: not a session id")))?;
        let dir = paths.state.join("sessions").join(id);
        if dir.join("session.json").is_file() {
            Ok(dir)
        } else {
            Err(AibusError::Usage(format!("no session {id}")))
        }
    };
    match cmd {
        SessionCmd::List { rebuild, names } => {
            let mut rows = index(&paths, *rebuild)?.list()?;
            if *names {
                for r in &rows {
                    writeln!(out, "{}", r["id"].as_str().unwrap_or_default())?;
                }
            } else if g.json {
                let mut all = Json::Array(rows);
                redactor.redact_json(&mut all);
                writeln!(out, "{all}")?;
            } else {
                for r in &mut rows {
                    redactor.redact_json(r);
                    let col = |k: &str| r[k].as_str().unwrap_or("-").to_owned();
                    writeln!(
                        out,
                        "{}  {}  {:>3} runs  {:<10}  ${:.4}  {}",
                        col("id"),
                        col("started_at"),
                        r["runs"],
                        col("outcome"),
                        r["cost_usd"].as_f64().unwrap_or(0.0),
                        col("topic")
                    )?;
                }
            }
        }
        SessionCmd::Show { id } => {
            let dir = dir_of(id)?;
            let mut shown = index(&paths, false)?
                .show(id)?
                .ok_or_else(|| AibusError::Usage(format!("no session {id}")))?;
            // `session` is session.json as it is on disk, which has `started`, `lead` and `cwd`.
            shown["session"] = serde_json::to_value(SessionMeta::read(&dir)?).map_err(std::io::Error::other)?;
            // The latest handoff and the ledger are what the graph held at the last snapshot.
            let snap = read_snapshot(&dir).ok();
            let input = |k: &str| snap.as_ref().and_then(|s| s.inputs.get(k));
            shown["handoff"] = input(HANDOFF_LATEST)
                .and_then(|h| get(h, "path"))
                .map_or(Json::Null, value_to_json);
            shown["ledger"] = input(LEDGER).map_or(Json::Null, value_to_json);
            redactor.redact_json(&mut shown);
            if g.json {
                writeln!(out, "{shown}")?;
            } else {
                writeln!(out, "{}", serde_json::to_string_pretty(&shown).map_err(std::io::Error::other)?)?;
                for w in worktrees(&paths, id) {
                    writeln!(out, "worktree kept: {}", w.display())?;
                }
            }
        }
        SessionCmd::Log { id } => {
            for rec in commits::read_all(&dir_of(id)?)? {
                writeln!(out, "{}", json!({"time": rec.time, "intent": rec.intent, "changed": rec.changed.len(), "at": rec.at}))?;
            }
        }
        SessionCmd::Replay { id, .. } => {
            let recorded: String = read_lines(&dir_of(id)?.join(MSGS))?
                .iter()
                .map(|l| format!("{l}\n"))
                .collect();
            let hash = replay_hash(&recorded).map_err(AibusError::Usage)?;
            if g.json {
                writeln!(out, "{}", json!({ "hash": hash }))?;
            } else {
                writeln!(out, "{hash}")?;
            }
        }
        SessionCmd::Export { id, format } => {
            let dir = dir_of(id)?;
            let md = format.as_deref() != Some("jsonl");
            for run in aibus_core::persist::session::read_runs(&dir) {
                let path = dir.join("runs").join(&run.id).join("transcript.jsonl");
                let lines = fs::read_to_string(&path)?
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Json>(l).ok())
                    .map(|mut v| {
                        redactor.redact_json(&mut v);
                        v
                    })
                    .collect::<Vec<_>>();
                if md {
                    let file = dir.join("export").join(format!("{}.md", run.id));
                    aibus_core::persist::fs::create_private_dir(&dir.join("export"))?;
                    aibus_core::persist::fs::write_atomic(&file, markdown(&run.id, &run.agent, &lines))?;
                    writeln!(out, "{}", file.display())?;
                } else {
                    for v in lines {
                        writeln!(out, "{v}")?;
                    }
                }
            }
        }
        SessionCmd::Prune { older_than } => {
            let age = match older_than {
                Some(a) => parse_age(a).map_err(AibusError::Usage)?,
                None => {
                    let project = project_dir(&g.project_dir()?).join("settings.toml");
                    let cfg = config::load(&paths.config_file(), Some(&project))?.config;
                    Duration::from_secs(u64::from(cfg.persist.session_retention_days) * 86_400)
                }
            };
            let removed = prune(&paths, SystemTime::now(), age)?;
            if g.json {
                writeln!(out, "{}", json!({ "removed": removed }))?;
            } else {
                for id in &removed {
                    writeln!(out, "removed {id}")?;
                }
                writeln!(out, "{} sessions removed", removed.len())?;
            }
        }
        SessionCmd::Resume { id, run, message, .. } => {
            let dir = dir_of(id)?;
            let (snap, tail) = load(&dir)?;
            let mut app = App::new(MiniGraph::new(), now_ms);
            let at = snap.now;
            app.hydrate(snap, &tail)?;
            let runs = aibus_core::persist::session::read_runs(&dir).len();
            if g.json {
                writeln!(out, "{}", json!({"session": id, "runs": runs, "snapshot_time": at, "replayed": tail.len()}))?;
            } else {
                writeln!(out, "resumed {id}: {runs} runs, snapshot at {at}, {} commits replayed", tail.len())?;
            }
            if run.is_some() || message.is_some() {
                eprintln!("aibus: session resume: continuing a run needs the interactive loop: not yet");
                return Ok(2);
            }
        }
    }
    Ok(0)
}

/// The index, opened now and not before. A missing file, or one that disagrees with the
/// directories on how many sessions there are, is rebuilt (07 section 2.3).
fn index(paths: &Paths, rebuild: bool) -> Result<Index, AibusError> {
    let sessions = paths.state.join("sessions");
    let path = paths.data.join("index.sqlite");
    let fresh = !path.is_file();
    let mut idx = Index::open(&path)?;
    let dirs = fs::read_dir(&sessions).map_or(0, |d| d.flatten().filter(|e| e.path().join("session.json").is_file()).count());
    if (rebuild || fresh || idx.list()?.len() != dirs) && sessions.is_dir() {
        idx.rebuild(&sessions)?;
    }
    Ok(idx)
}

/// One run's transcript as Markdown.
fn markdown(run: &str, agent: &str, lines: &[Json]) -> String {
    let mut md = format!("# Run {run} ({agent})\n");
    for l in lines {
        let when = l["t"].as_str().unwrap_or("");
        match l["type"].as_str().unwrap_or("") {
            "message" => {
                md.push_str(&format!("\n## {} ({when})\n", l["role"].as_str().unwrap_or("?")));
                for b in l["blocks"].as_array().into_iter().flatten() {
                    match b["type"].as_str().unwrap_or("") {
                        "tool_use" => md.push_str(&format!(
                            "\nTool call `{}`:\n\n```json\n{}\n```\n",
                            b["name"].as_str().unwrap_or("?"),
                            b["input"]
                        )),
                        "thinking" => md.push_str(&format!("\n> {}\n", b["text"].as_str().unwrap_or(""))),
                        _ => md.push_str(&format!("\n{}\n", b["text"].as_str().unwrap_or(""))),
                    }
                }
            }
            "tool_result" => {
                let err = if l["is_error"] == true { " (error)" } else { "" };
                md.push_str(&format!(
                    "\n### Result{err}, {} bytes\n\n```text\n{}\n```\n",
                    l["bytes"],
                    l["content"].as_str().unwrap_or("")
                ));
                if l["truncated"] == true {
                    md.push_str(&format!("\nCut; the whole text is in `{}`.\n", l["full"].as_str().unwrap_or("")));
                }
            }
            "compact_boundary" => md.push_str(&format!("\n---\nCompacted at {} tokens.\n", l["pre_tokens"])),
            "end" => md.push_str(&format!("\n---\nEnded {} after {} turns.\n", l["status"].as_str().unwrap_or("?"), l["turns"])),
            _ => {}
        }
    }
    md
}
