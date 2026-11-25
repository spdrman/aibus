//! `aibus agent stats <name>` and `aibus agent eval <name>` (H1.5): what the index knows about an
//! agent's past runs, and a directory of prompt plus `done_when` cases run against it.
//!
//! Both print counts, case names and exit codes. Neither prints anything a run said, so the lines
//! are safe to paste into a prompt.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aibus_core::persist::paths::project_dir;
use aibus_core::{AgentName, AibusError};
use serde_json::json;

use crate::agent_cmd::Env;
use crate::cli::Global;
use crate::session_cmd;

/// Bumped when a field of the eval scorecard is added, removed or changes meaning.
const SCORECARD_VERSION: &str = "1.0.0";

fn known(env: &Env, g: &Global, name: &str) -> Result<AgentName, AibusError> {
    let wanted = AgentName::new(name).map_err(|e| AibusError::Usage(e.to_string()))?;
    if env.discover(g)?.agents.contains_key(&wanted) {
        Ok(wanted)
    } else {
        Err(AibusError::UnknownAgent(name.to_owned()))
    }
}

/// `agent stats`: the newest `last` runs of the agent, by status, with turns and cost.
pub fn stats(env: &Env, g: &Global, name: &str, last: u32) -> Result<u8, AibusError> {
    let name = known(env, g, name)?;
    let index = session_cmd::index(&env.paths, false)?;
    let s = index.agent_stats(name.as_str(), last)?;
    if g.json {
        println!(
            "{}",
            json!({
                "agent": name.as_str(), "runs": s.runs, "completed": s.completed, "partial": s.partial,
                "failed": s.failed, "other": s.runs - s.completed - s.partial - s.failed,
                "turns": s.turns, "cost_usd": s.cost_usd,
            })
        );
    } else {
        println!(
            "{name}: {} runs: {} completed, {} partial, {} failed; {} turns, ${:.4}",
            s.runs, s.completed, s.partial, s.failed, s.turns, s.cost_usd
        );
    }
    Ok(0)
}

/// One case: `prompt`, the `done_when` command that decides it, and optionally a RON `script` for
/// the fake provider (a path relative to the case file). Without a script the run uses whatever
/// account the configuration picks.
struct Case {
    name: String,
    prompt: String,
    done_when: String,
    script: Option<PathBuf>,
}

fn load_cases(dir: &Path) -> Result<Vec<Case>, AibusError> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| AibusError::Usage(format!("{}: {e}", dir.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for f in files {
        let bad = |why: &str| AibusError::Usage(format!("{}: {why}", f.display()));
        let table: toml::Table = fs::read_to_string(&f)?
            .parse()
            .map_err(|e| bad(&format!("{e}")))?;
        let text = |k: &str| table.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        cases.push(Case {
            name: f
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            prompt: text("prompt")
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| bad("needs a prompt"))?,
            done_when: text("done_when")
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| bad("needs a done_when"))?,
            script: text("script").map(|s| f.parent().unwrap_or(dir).join(s)),
        });
    }
    if cases.is_empty() {
        return Err(AibusError::Usage(format!(
            "no *.toml cases in {}",
            dir.display()
        )));
    }
    Ok(cases)
}

/// The exit code of `cmd`, or `None` when it did not start or ran past `limit` (it is killed then).
fn exit_within(mut cmd: Command, limit: Duration) -> Option<i32> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `agent eval`: each case is one `aibus run --agent <name>`, then its `done_when` in the project
/// directory; the case passes when both exit 0. The scorecard names every case. Exit 1 when any
/// case fails.
pub fn eval(
    env: &Env,
    g: &Global,
    name: &str,
    cases: Option<&Path>,
    timeout_secs: u64,
) -> Result<u8, AibusError> {
    let name = known(env, g, name)?;
    let dir = cases.map_or_else(
        || project_dir(&env.cwd).join("evals").join(name.as_str()),
        |c| env.cwd.join(c),
    );
    let cases = load_cases(&dir)?;
    let exe = std::env::current_exe()?;
    let limit = Duration::from_secs(timeout_secs);
    let mut rows = Vec::new();
    for c in &cases {
        let mut run = Command::new(&exe);
        run.current_dir(&env.cwd)
            .args(["run", "--json", "--agent", name.as_str()]);
        if let Some(cfg) = &g.config {
            run.arg("--config").arg(cfg);
        }
        if let Some(s) = &c.script {
            run.arg("--script").arg(s);
        }
        run.arg(&c.prompt);
        let run_exit = exit_within(run, limit);
        let mut check = Command::new("sh");
        check.current_dir(&env.cwd).args(["-c", &c.done_when]);
        let done_exit = if run_exit == Some(0) {
            exit_within(check, Duration::from_secs(60))
        } else {
            None
        };
        let reason = match (run_exit, done_exit) {
            (Some(0), Some(0)) => None,
            (Some(0), Some(n)) => Some(format!("done_when exited {n}")),
            (Some(0), None) => Some("done_when did not finish".to_owned()),
            (Some(n), _) => Some(format!("the run exited {n}")),
            (None, _) => Some(format!("the run did not finish in {timeout_secs}s")),
        };
        rows.push(json!({
            "case": c.name, "result": if reason.is_none() { "pass" } else { "fail" },
            "reason": reason, "run_exit": run_exit, "done_when_exit": done_exit,
        }));
    }
    let passed = rows.iter().filter(|r| r["result"] == "pass").count();
    let card = json!({
        "scorecard_version": SCORECARD_VERSION,
        "agent": name.as_str(),
        "summary": { "passed": passed, "of": rows.len() },
        "cases": rows,
    });
    if g.json {
        println!("{card}");
    } else {
        println!("eval {name}: {passed} of {} cases passed", rows.len());
        for r in card["cases"].as_array().into_iter().flatten() {
            let why = r["reason"]
                .as_str()
                .map_or(String::new(), |w| format!(": {w}"));
            println!(
                "  {}  {}{why}",
                if r["result"] == "pass" {
                    "pass"
                } else {
                    "FAIL"
                },
                r["case"].as_str().unwrap_or("?")
            );
        }
    }
    Ok(u8::from(passed != rows.len()))
}
