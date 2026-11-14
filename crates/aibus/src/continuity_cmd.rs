//! `aibus ledger`, `aibus handoff` and `aibus memory` (06 section 1): the continuity files
//! (01 section 8) from the command line. None of them starts a session or needs an account.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::config::{self, Config};
use aibus_core::continuity::handoff::{self, Front, Handoff};
use aibus_core::continuity::learn::{self, Learning};
use aibus_core::continuity::recall::MemoryIndex;
use aibus_core::continuity::{clean_topic, ledger, memory, Layout};
use aibus_core::persist::index::Index;
use aibus_core::persist::paths::project_dir;
use aibus_core::persist::time::date;
use aibus_core::{AibusError, MemoryScope, Redactor};
use serde_json::{json, Value};

use crate::cli::{Global, HandoffCmd, LedgerCmd, MemoryCmd};

fn usage(msg: impl Into<String>) -> AibusError {
    AibusError::Usage(msg.into())
}

/// The layout for `--topic` or the newest ledger, and the config it came from.
fn open(g: &Global) -> Result<(Layout, Config), AibusError> {
    let paths = g.paths()?;
    let cwd = g.project_dir()?;
    let loaded = config::load(
        &paths.config_file(),
        Some(&project_dir(&cwd).join("settings.toml")),
    )?;
    for w in &loaded.warnings {
        eprintln!("aibus: warning: {w}");
    }
    let layout =
        Layout::new(&cwd, &loaded.config, &paths.data, g.topic.as_deref()).map_err(usage)?;
    Ok((layout, loaded.config))
}

fn today() -> String {
    date(SystemTime::now())
}

fn print_json(v: &Value) -> Result<(), AibusError> {
    println!(
        "{}",
        serde_json::to_string_pretty(v).map_err(std::io::Error::other)?
    );
    Ok(())
}

fn ledger_json(l: &ledger::Ledger, path: &Path) -> Value {
    let sections: serde_json::Map<String, Value> = l
        .sections
        .iter()
        .map(|(n, b)| (n.clone(), b.clone().into()))
        .collect();
    json!({ "topic": l.topic, "path": path, "updated": l.updated, "sections": sections })
}

pub fn ledger(cmd: &LedgerCmd, g: &Global) -> Result<u8, AibusError> {
    let (mut layout, _) = open(g)?;
    match cmd {
        LedgerCmd::Show => {
            let l = ledger::read(&layout)?.ok_or_else(|| {
                usage(format!(
                    "no ledger for the topic {}; `aibus ledger new {}` starts one",
                    layout.topic, layout.topic
                ))
            })?;
            if g.json {
                print_json(&ledger_json(&l, &layout.ledger()))?;
            } else {
                print!("{}", l.render());
            }
        }
        LedgerCmd::New { topic } => {
            layout.topic = clean_topic(topic).map_err(usage)?;
            let l = ledger::create(&layout, &today()).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => usage(e.to_string()),
                _ => e.into(),
            })?;
            if g.json {
                print_json(&ledger_json(&l, &layout.ledger()))?;
            } else {
                println!("{}", layout.ledger().display());
            }
        }
        LedgerCmd::Update {
            section,
            text,
            append,
        } => {
            let l =
                ledger::update(&layout, section, text, *append, &today()).map_err(|e| {
                    match e.kind() {
                        std::io::ErrorKind::InvalidInput => usage(e.to_string()),
                        _ => e.into(),
                    }
                })?;
            if g.json {
                print_json(&ledger_json(&l, &layout.ledger()))?;
            } else {
                println!("{}", layout.ledger().display());
            }
        }
    }
    Ok(0)
}

/// One handoff as `handoff list --json` shows it.
fn summary(path: &Path) -> Value {
    let topic = path
        .parent()
        .and_then(Path::file_name)
        .map(|t| t.to_string_lossy().into_owned());
    let mut v = json!({ "path": path, "topic": topic });
    if let Some(h) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| Handoff::parse(&t).ok())
    {
        for k in ["session", "date", "status", "outcome"] {
            v[k] = h.text(k).map_or(Value::Null, Value::String);
        }
        v["goal"] = h.goal().into();
        v["now"] = h.now().into();
    } else {
        v["error"] = "does not parse as a handoff".into();
    }
    v
}

pub fn handoff(cmd: &HandoffCmd, g: &Global) -> Result<u8, AibusError> {
    let (layout, _) = open(g)?;
    // `list` and `show` look in one topic only when `--topic` names it.
    let topic = g.topic.as_ref().map(|_| layout.topic.as_str());
    match cmd {
        HandoffCmd::Create { outcome, goal, now } => {
            let outcome = crate::env::outcome(outcome.as_deref(), |v| std::env::var(v).ok())?;
            let ledger = ledger::read(&layout)?;
            let mut body = handoff::summarise(ledger.as_ref(), None);
            if let Some(g) = goal {
                body.insert("goal".into(), g.clone().into());
            } else if ledger.as_ref().is_none_or(|l| l.goal().trim().is_empty()) {
                return Err(usage(format!(
                    "no goal: the ledger for {} has none, so pass --goal or `aibus ledger update goal <text>`",
                    layout.topic
                )));
            }
            if let Some(n) = now {
                body.insert("now".into(), n.clone().into());
            }
            let status = match outcome.as_deref() {
                Some("SUCCEEDED") => "complete",
                Some("FAILED") => "blocked",
                _ => "partial",
            };
            let at = SystemTime::now();
            let front = Front {
                session: &layout.topic,
                date: &date(at),
                status,
                outcome: outcome.as_deref(),
            };
            let h = Handoff::build(&front, &body).map_err(usage)?;
            let path = handoff::write(&layout, &h, false, at)?;
            if g.json {
                print_json(&json!({ "path": path }))?;
            } else {
                println!("{}", path.display());
            }
        }
        HandoffCmd::List => {
            let found = handoff::list(&layout.handoffs(), topic);
            if g.json {
                print_json(
                    &json!({ "handoffs": found.iter().map(|p| summary(p)).collect::<Vec<_>>() }),
                )?;
            } else {
                for p in &found {
                    let s = summary(p);
                    let field = |k: &str| s[k].as_str().unwrap_or("-").to_owned();
                    println!(
                        "{}\t{}\t{}\t{}",
                        p.display(),
                        field("status"),
                        field("outcome"),
                        field("goal")
                    );
                }
            }
        }
        HandoffCmd::Show { path } => {
            let path: PathBuf = match path {
                Some(p) => p.clone(),
                None => handoff::latest(&layout.handoffs(), topic)
                    .ok_or_else(|| usage("no handoff yet; `aibus handoff create` writes one"))?,
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| usage(format!("{}: {e}", path.display())))?;
            let h = Handoff::parse(&text).map_err(|e| usage(format!("{}: {e}", path.display())))?;
            if g.json {
                print_json(&h.to_json())?;
            } else {
                print!("{text}");
            }
        }
    }
    Ok(0)
}

pub fn memory(cmd: &MemoryCmd, g: &Global, redactor: &Redactor) -> Result<u8, AibusError> {
    let (layout, cfg) = open(g)?;
    match cmd {
        MemoryCmd::Recall { query, k } => {
            let mut index = Index::open(&layout.index())?;
            index.sync(&layout)?;
            let k = k.unwrap_or(cfg.memory.recall_k) as usize;
            let hits = index.recall(&layout, query, k)?;
            if g.json {
                print_json(&json!(hits))?;
            } else {
                for h in &hits {
                    println!("{:.3}\t{}\t{}", h.score, h.path, h.snippet);
                }
            }
        }
        MemoryCmd::Add { text, kind, scope } => {
            let kind = kind.as_deref().unwrap_or("fact");
            if !learn::KINDS.contains(&kind) {
                return Err(usage(format!(
                    "--type {kind}: want {}",
                    learn::KINDS.join(", ")
                )));
            }
            let scope = match scope.as_deref() {
                Some(s) => MemoryScope::parse(s)
                    .ok_or_else(|| usage(format!("--scope {s}: want user, project or local")))?,
                None => *layout
                    .scopes
                    .first()
                    .ok_or_else(|| usage("memory.scopes is empty"))?,
            };
            if text.trim().is_empty() {
                return Err(usage("nothing to add"));
            }
            let dir = memory::scope_dir(scope, "lead", &layout.project, &layout.data);
            if let Some(project) = memory::local_project(&dir) {
                memory::ensure_local_ignored(project)?;
            }
            let file = dir.join("LEARNINGS.md");
            let entry = Learning {
                kind: kind.to_owned(),
                confidence: "high".into(),
                content: text.split_whitespace().collect::<Vec<_>>().join(" "),
            };
            learn::append(
                &file,
                &[entry],
                &format!("{} added by hand", today()),
                redactor,
            )?;
            // Findable at once (when `memory.scopes` covers the scope).
            crate::continuity::reindex(&layout);
            if g.json {
                print_json(&json!({ "path": file, "scope": scope.as_str() }))?;
            } else {
                println!("{}", file.display());
            }
        }
        MemoryCmd::Index => {
            let mut index = Index::open(&layout.index())?;
            let synced = index.sync(&layout)?;
            if g.json {
                print_json(&json!(synced))?;
            } else {
                println!(
                    "indexed: {} added, {} updated, {} removed, {} unchanged",
                    synced.added, synced.updated, synced.removed, synced.unchanged
                );
            }
        }
    }
    Ok(0)
}
