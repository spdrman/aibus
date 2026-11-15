//! `aibus runs` (#284, spec 7.3 and 8.3): the recorded runs of an external CLI, read from the
//! trace store. Every command here reads files and prints what `aibus_trace::view` renders; none
//! of them starts a provider, a process or a connection. `rerun` is refused until the scheduler
//! (#288, #289) can start a new attempt, and says that a rerun's outcome could differ.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use aibus_core::AibusError;
use aibus_trace::view::{self, Filter, Recorded};
use serde_json::Value;

use crate::cli::{Global, RunsCmd};

fn load(root: &Path, run: &str) -> Result<(String, Recorded), AibusError> {
    let dir = view::resolve(root, run).map_err(AibusError::Usage)?;
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let rec = Recorded::load(&dir).map_err(|e| AibusError::Usage(format!("run {name}: {e}")))?;
    Ok((name, rec))
}

pub fn runs(cmd: &RunsCmd, g: &Global) -> Result<u8, AibusError> {
    let root = |r: &Option<PathBuf>| -> Result<PathBuf, AibusError> {
        match r {
            Some(r) => Ok(r.clone()),
            None => Ok(g.paths()?.state.join("traces")),
        }
    };
    let mut out = std::io::stdout().lock();
    match cmd {
        RunsCmd::List { root: r, names } => {
            let rows = view::fleet(&root(r)?);
            if *names {
                for row in &rows {
                    writeln!(out, "{}", row.id)?;
                }
            } else if g.json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&rows).map_err(std::io::Error::other)?
                )?;
            } else {
                out.write_all(view::render_fleet(&rows).as_bytes())?;
            }
        }
        RunsCmd::Inspect {
            run,
            root: r,
            node,
            event,
            agent_id,
            tool_use,
            kind,
            level,
        } => {
            let (name, rec) = load(&root(r)?, run)?;
            let text = if let Some(id) = node {
                view::inspect(&rec, id).ok_or_else(|| {
                    AibusError::Usage(format!(
                        "run {name} has no agent, tool use or attempt {}",
                        view::clean(id, 160)
                    ))
                })?
            } else if let Some(seq) = event {
                view::event(&rec, *seq)
                    .ok_or_else(|| AibusError::Usage(format!("run {name} has no event #{seq}")))?
            } else {
                let bad = |what: &str, v: &str| {
                    AibusError::Usage(format!(
                        "{what} {}: not one of the trace's names",
                        view::clean(v, 160)
                    ))
                };
                let filter = Filter {
                    agent: agent_id.clone(),
                    tool: tool_use.clone(),
                    kind: kind
                        .as_ref()
                        .map(|k| {
                            serde_json::from_value(Value::String(k.clone()))
                                .map_err(|_| bad("--kind", k))
                        })
                        .transpose()?,
                    level: level
                        .as_ref()
                        .map(|l| {
                            serde_json::from_value(Value::String(l.clone()))
                                .map_err(|_| bad("--level", l))
                        })
                        .transpose()?,
                };
                let mut s = view::graph(&rec, &name);
                s.push_str(&view::timeline(&rec, &filter));
                s
            };
            out.write_all(text.as_bytes())?;
        }
        RunsCmd::Replay {
            run,
            root: r,
            offline: _,
            events,
        } => {
            let (name, rec) = load(&root(r)?, run)?;
            if *events {
                // Byte for byte into a pipe or a file; on a terminal, characters it would act on
                // are written as JSON escapes, which reads as the same JSON.
                let tty = std::io::stdout().is_terminal();
                for raw in &rec.raw {
                    if tty {
                        out.write_all(&view::terminal_safe_line(raw))?;
                    } else {
                        out.write_all(raw)?;
                    }
                }
            } else {
                out.write_all(view::replay(&rec, &name).as_bytes())?;
            }
        }
        RunsCmd::Rerun {
            run,
            root: r,
            isolated: _,
        } => {
            let (name, _) = load(&root(r)?, run)?;
            return Err(AibusError::Usage(format!(
                "runs rerun is not available yet: a rerun starts a new attempt of the CLI, which needs the scheduler (#288, #289). \
                 Its outcome may differ from run {name}: a model does not repeat itself. \
                 `aibus runs replay {name}` shows the recorded run offline"
            )));
        }
    }
    out.flush()?;
    Ok(0)
}
