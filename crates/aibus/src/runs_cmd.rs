//! `aibus runs` (#284, spec 7.3 and 8.3): the recorded runs of an external CLI, read from the
//! trace store. Every command here reads files and prints what `aibus_trace::view` renders; none
//! of them starts a provider, a process or a connection. `rerun` is refused until the scheduler
//! (#288, #289) can start a new attempt, and says that a rerun's outcome could differ.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use aibus_core::AibusError;
use aibus_trace::causal::{inspector, render_reproducibility, reproducibility};
use aibus_trace::compare::{compare_runs, render, result_mismatch, RunInput};
use aibus_trace::eval::EvaluationResult;
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

/// An `EvaluationResult` read from a file the user named: a regular file of at most 4 MiB (a
/// symlink to one is followed, a FIFO or device is refused before it is opened, so a FIFO with no
/// writer cannot hang the command), opened once so a file that grows after the check is not read
/// whole. The message is cleaned for a terminal because a parse error echoes the file's text.
fn result_file(path: &Path) -> Result<EvaluationResult, AibusError> {
    let bad = |why: String| {
        let msg = format!("evaluation result {}: {why}", path.display());
        AibusError::Usage(
            String::from_utf8_lossy(&view::terminal_safe_line(msg.as_bytes())).into_owned(),
        )
    };
    if !std::fs::metadata(path)
        .map_err(|e| bad(e.to_string()))?
        .is_file()
    {
        return Err(bad("not a regular file of at most 4 MiB".into()));
    }
    let file = std::fs::File::open(path).map_err(|e| bad(e.to_string()))?;
    if !file.metadata().map_err(|e| bad(e.to_string()))?.is_file() {
        return Err(bad("not a regular file of at most 4 MiB".into()));
    }
    let mut bytes = Vec::new();
    file.take((4 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| bad(e.to_string()))?;
    if bytes.len() > 4 << 20 {
        return Err(bad("not a regular file of at most 4 MiB".into()));
    }
    serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))
}

/// `v` as one line of JSON for a terminal: serde escapes the C0 controls, and `terminal_safe_line`
/// writes bidi, zero-width and C1 characters as `\uXXXX` escapes, which is the same JSON value.
fn json_line(json: serde_json::Result<String>) -> Result<String, AibusError> {
    let line = json.map_err(std::io::Error::other)? + "\n";
    Ok(String::from_utf8_lossy(&view::terminal_safe_line(line.as_bytes())).into_owned())
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
                let missing = || {
                    AibusError::Usage(format!(
                        "run {name} has no agent, tool use or attempt {}",
                        view::clean(id, 160)
                    ))
                };
                if g.json {
                    let v = inspector(&rec, id).ok_or_else(missing)?;
                    json_line(serde_json::to_string(&v))?
                } else {
                    view::inspect(&rec, id).ok_or_else(missing)?
                }
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
        RunsCmd::Compare {
            a,
            b,
            root: r,
            result_a,
            result_b,
        } => {
            let root = root(r)?;
            let (name_a, rec_a) = load(&root, a)?;
            let (name_b, rec_b) = load(&root, b)?;
            let res_a = result_a.as_deref().map(result_file).transpose()?;
            let res_b = result_b.as_deref().map(result_file).transpose()?;
            for (name, rec, res) in [(&name_a, &rec_a, &res_a), (&name_b, &rec_b, &res_b)] {
                if let Some(why) = res.as_ref().and_then(|r| result_mismatch(rec, r)) {
                    return Err(AibusError::Usage(format!(
                        "evaluation result for run {}: {why}",
                        view::clean(name, 160)
                    )));
                }
            }
            let c = compare_runs(
                &RunInput {
                    id: &name_a,
                    rec: &rec_a,
                    result: res_a.as_ref(),
                },
                &RunInput {
                    id: &name_b,
                    rec: &rec_b,
                    result: res_b.as_ref(),
                },
            );
            if g.json {
                out.write_all(json_line(serde_json::to_string(&c))?.as_bytes())?;
            } else {
                out.write_all(render(&c).as_bytes())?;
            }
        }
        RunsCmd::Repro { run, root: r } => {
            let (name, rec) = load(&root(r)?, run)?;
            let rep = reproducibility(&name, &rec);
            if g.json {
                out.write_all(json_line(serde_json::to_string(&rep))?.as_bytes())?;
            } else {
                out.write_all(render_reproducibility(&rep).as_bytes())?;
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
