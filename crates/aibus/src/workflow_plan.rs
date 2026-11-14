//! `workflow plan --dry-run`, `workflow diff`, `workflow manifest` and `workflow export` (#283):
//! the compiler from the command line. Nothing here runs a step or a gate; `export` writes only
//! with the hash of the diff it showed.

use std::io::Read;
use std::path::{Path, PathBuf};

use aibus_core::workflow::compile::{compile, plan_hash};
use aibus_core::workflow::manifest::{diff, run_manifest, Change, RunFacts};
use aibus_core::workflow::{self, native, validate, Problem, Workflow};
use aibus_core::{AgentDef, AgentName, AibusError, Redactor};
use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::cli::Global;

/// The biggest workflow file or saved plan these commands read.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Exit status when a plan differs from the one it is locked to.
const DRIFT: u8 = 13;

/// Exit status for a workflow that does not validate, as `workflow validate` uses.
const INVALID: u8 = 12;

/// The file `target` names: a path (it ends in `.toml` or `.json`, has a separator, or is a file
/// in the project), else a workflow name, looked up as `workflow run` would.
fn resolve(g: &Global, target: &str) -> Result<PathBuf, AibusError> {
    let cwd = g.project_dir()?;
    let is_path = target.ends_with(".toml")
        || target.ends_with(".json")
        || target.contains(std::path::MAIN_SEPARATOR)
        || target.contains('/')
        || cwd.join(target).is_file();
    if is_path {
        return Ok(cwd.join(target));
    }
    let dirs = workflow::dirs(&cwd, Some(&g.paths()?.config));
    workflow::list(&dirs)
        .into_iter()
        .find(|(n, _, _)| n == target)
        .map(|(_, path, _)| path)
        .ok_or_else(|| {
            AibusError::Usage(format!("no workflow named {target} (aibus workflow list)"))
        })
}

/// The text of `path`, refused (a usage error) when it is too big or not UTF-8.
fn read(path: &Path) -> Result<String, AibusError> {
    let bad = |why: String| AibusError::Usage(format!("{}: {why}", path.display()));
    let meta = std::fs::metadata(path).map_err(|e| bad(e.to_string()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(bad(format!(
            "{} bytes, the most these commands read is {MAX_FILE_BYTES}",
            meta.len()
        )));
    }
    std::fs::read_to_string(path).map_err(|e| bad(e.to_string()))
}

fn agents(g: &Global) -> Result<IndexMap<AgentName, AgentDef>, AibusError> {
    Ok(crate::agent_cmd::Env::load(g)?.discover(g)?.agents)
}

/// Prints what `workflow validate` would for a workflow that gets no plan, and gives its exit
/// status.
fn refuse(g: &Global, path: &Path, errors: &[Problem]) -> u8 {
    let row =
        |p: &Problem| json!({"path": path, "step": p.step, "field": p.field, "message": p.message});
    if g.json {
        println!(
            "{}",
            json!({"ok": false, "errors": errors.iter().map(row).collect::<Vec<_>>(), "warnings": []})
        );
    } else {
        for p in errors {
            let step = p.step.map_or(String::new(), |n| format!("step {n}: "));
            let field = p
                .field
                .as_deref()
                .map_or(String::new(), |f| format!("{f}: "));
            println!("error: {}: {step}{field}{}", path.display(), p.message);
        }
    }
    INVALID
}

/// Compiles `target`. `Err(code)` is a workflow that got no plan, already reported.
fn plan_of(
    g: &Global,
    target: &str,
    agents: &IndexMap<AgentName, AgentDef>,
    redactor: &Redactor,
) -> Result<Result<(PathBuf, Workflow, Value), u8>, AibusError> {
    let path = resolve(g, target)?;
    let text = read(&path)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("workflow");
    let roster: Vec<AgentName> = agents.keys().cloned().collect();
    let checked = validate(&text, stem, &roster);
    let wf = match Workflow::parse(stem, &text) {
        Ok(wf) if checked.ok => wf,
        _ => return Ok(Err(refuse(g, &path, &checked.errors))),
    };
    match compile(&wf, &text, agents, redactor) {
        Ok(plan) => Ok(Ok((path, wf, plan))),
        Err(problems) => Ok(Err(refuse(g, &path, &problems))),
    }
}

/// `workflow plan <target> --dry-run [--locked <plan.json>]`.
pub fn plan(
    g: &Global,
    target: &str,
    dry_run: bool,
    locked: Option<&Path>,
    redactor: &Redactor,
) -> Result<u8, AibusError> {
    if !dry_run {
        return Err(AibusError::Usage(
            "workflow plan only runs as --dry-run: running a plan is the scheduler's job (#288)"
                .into(),
        ));
    }
    let agents = agents(g)?;
    let (path, _, plan) = match plan_of(g, target, &agents, redactor)? {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    if let Some(lock) = locked {
        let saved = saved_plan(lock)?;
        let changes = diff(&saved, &plan);
        if !changes.is_empty() {
            print_changes(g, &saved, &plan, &changes);
            eprintln!(
                "aibus: the plan of {} differs from {}",
                path.display(),
                lock.display()
            );
            return Ok(DRIFT);
        }
    }
    if g.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).unwrap_or_default()
        );
    } else {
        print_plan(&path, &plan);
    }
    Ok(0)
}

/// A plan saved with `plan --dry-run --json`. One whose stored hash does not match its content
/// was edited by hand, which is said, and it is still compared as it is.
fn saved_plan(path: &Path) -> Result<Value, AibusError> {
    let text = read(path)?;
    let plan: Value = serde_json::from_str(&text)
        .map_err(|e| AibusError::Usage(format!("{}: not a saved plan: {e}", path.display())))?;
    if plan["kind"] != "aibus.workflow.plan" {
        return Err(AibusError::Usage(format!(
            "{}: not a saved plan (aibus workflow plan --dry-run --json)",
            path.display()
        )));
    }
    if plan["plan_hash"].as_str() != Some(plan_hash(&plan).as_str()) {
        eprintln!("aibus: warning: {} was edited after it was planned: its plan_hash does not match its content", path.display());
    }
    Ok(plan)
}

/// A value on one line, for a person.
fn short(v: &Value) -> String {
    let s = match v {
        Value::Null => "(none)".to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.chars().count() > 120 {
        format!("{}...", s.chars().take(117).collect::<String>())
    } else {
        s
    }
}

fn print_changes(g: &Global, a: &Value, b: &Value, changes: &[Change]) {
    if g.json {
        println!(
            "{}",
            json!({"same": changes.is_empty(), "a": a["plan_hash"], "b": b["plan_hash"], "changes": changes})
        );
    } else if changes.is_empty() {
        println!("same plan ({})", short(&a["plan_hash"]));
    } else {
        for c in changes {
            println!("{}: {} -> {}", c.path, short(&c.a), short(&c.b));
        }
    }
}

fn print_plan(path: &Path, plan: &Value) {
    let w = &plan["workflow"];
    let revision = w["revision"]
        .as_str()
        .map(|r| format!("  revision {r}"))
        .unwrap_or_default();
    println!("{}{revision}  ({})", short(&w["name"]), path.display());
    println!("plan_hash {}", short(&plan["plan_hash"]));
    match plan["plan_only_because"].as_str() {
        None => println!("runs now: yes, as a chain"),
        Some(why) => println!("runs now: no: {why}"),
    }
    println!("\nsteps:");
    for s in plan["steps"].as_array().into_iter().flatten() {
        let mode = if s["mode"] == s["mode_requested"] {
            short(&s["mode"])
        } else {
            format!(
                "{} (asked {})",
                short(&s["mode"]),
                short(&s["mode_requested"])
            )
        };
        let parent = s["parent"]
            .as_str()
            .map(|p| format!("  under {p}"))
            .unwrap_or_default();
        println!(
            "  {} {}  agent {}  {}  {mode}{parent}  prompt {}",
            s["n"],
            short(&s["key"]),
            short(&s["agent"]),
            short(&s["runtime"]),
            short(&s["prompt"]["sha256"])
        );
        let inv = &s["invocation"];
        match inv["kind"].as_str() {
            Some("process") => {
                let argv: Vec<String> = inv["argv"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(short)
                    .collect();
                println!("      runs: {} {}", short(&inv["program"]), argv.join(" "));
            }
            Some("native_subagent") => println!(
                "      native subagent of {}, passed with --agents",
                short(&inv["launched_by"])
            ),
            _ => println!(
                "      an aibus run of {} under the lead",
                short(&s["agent"])
            ),
        }
        if let Some(gate) = s["gate"].as_str() {
            println!("      gate: {gate}");
        }
    }
    println!("\nedges:");
    for e in plan["edges"].as_array().into_iter().flatten() {
        let mut line = format!(
            "  {} {} -> {}",
            short(&e["kind"]),
            short(&e["from"]),
            short(&e["to"])
        );
        for (k, label) in [
            ("when", "when"),
            ("via", "via"),
            ("lineage", "lineage"),
            ("max_iterations", "at most"),
        ] {
            if !e[k].is_null() {
                line.push_str(&format!("  {label} {}", short(&e[k])));
            }
        }
        println!("{line}");
    }
    let env = &plan["envelope"];
    println!(
        "\nenvelope: up to {} agent runs; max_agent_runs {}, max_parallel {}, max_wall_seconds {}",
        env["worst_case_agent_runs"],
        short(&env["max_agent_runs"]),
        short(&env["max_parallel"]),
        short(&env["max_wall_seconds"])
    );
    if let Some(held) = plan["approvals"]["steps"]
        .as_object()
        .filter(|m| !m.is_empty())
    {
        println!("approvals:");
        for (step, kinds) in held {
            let kinds: Vec<String> = kinds.as_array().into_iter().flatten().map(short).collect();
            println!("  {step}: {}", kinds.join(", "));
        }
    }
    for note in plan["notes"].as_array().into_iter().flatten() {
        println!("note: {}", short(note));
    }
}

/// `workflow diff <a> <b>`: each side a workflow (name or `.toml`) or a saved plan (`.json`).
pub fn diff_cmd(g: &Global, a: &str, b: &str, redactor: &Redactor) -> Result<u8, AibusError> {
    let agents = agents(g)?;
    let mut sides = Vec::new();
    for target in [a, b] {
        if target.ends_with(".json") {
            sides.push(saved_plan(&resolve(g, target)?)?);
        } else {
            match plan_of(g, target, &agents, redactor)? {
                Ok((_, _, plan)) => sides.push(plan),
                Err(code) => return Ok(code),
            }
        }
    }
    print_changes(g, &sides[0], &sides[1], &diff(&sides[0], &sides[1]));
    Ok(0)
}

/// One line of git's output in the project, with the repository's own hooks and filesystem
/// monitor turned off (a repository is someone else's input). `Err` is why not.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git did not run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err("not a git repository, or one with no commit yet".into())
    }
}

/// `workflow manifest <target> [--input <text>|-]`.
pub fn manifest(
    g: &Global,
    target: &str,
    input: Option<&str>,
    redactor: &Redactor,
) -> Result<u8, AibusError> {
    let input = match input {
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .take(MAX_FILE_BYTES + 1)
                .read_to_string(&mut text)
                .map_err(|e| AibusError::Usage(format!("stdin: {e}")))?;
            Some(text)
        }
        other => other.map(str::to_owned),
    };
    let agents = agents(g)?;
    let (_, _, plan) = match plan_of(g, target, &agents, redactor)? {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let dir = g.project_dir()?;
    let limits = aibus_core::config::Limits::default();
    let facts = RunFacts {
        input,
        aibus_version: env!("CARGO_PKG_VERSION").to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        base_commit: git(&dir, &["rev-parse", "--verify", "HEAD"]),
        dirty: git(&dir, &["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty()),
        env_names: std::env::vars_os()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect(),
        budgets: json!({
            "max_turns": limits.max_turns,
            "max_tokens_total": limits.max_tokens_total,
            "max_cost_usd": limits.max_cost_usd,
            "max_concurrent_runs": limits.max_concurrent_runs,
        }),
    };
    let mut m = run_manifest(&plan, &facts);
    redactor.redact_json(&mut m);
    println!("{}", serde_json::to_string_pretty(&m).unwrap_or_default());
    Ok(0)
}

/// `workflow export <target> [--dir <dir>] [--apply <hash>]`.
pub fn export(
    g: &Global,
    target: &str,
    dir: Option<&Path>,
    apply: Option<&str>,
    redactor: &Redactor,
) -> Result<u8, AibusError> {
    let agents = agents(g)?;
    let (_, wf, _) = match plan_of(g, target, &agents, redactor)? {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let project = g.project_dir()?;
    let dir = dir.map_or_else(|| project.join(".claude/agents"), |d| project.join(d));
    let preview = native::preview(&wf, &agents, &dir).map_err(AibusError::Usage)?;
    if let Some(consent) = apply {
        let written = native::apply(&preview, consent).map_err(AibusError::Usage)?;
        if g.json {
            println!("{}", json!({ "written": written }));
        } else {
            for p in &written {
                println!("wrote {}", p.display());
            }
            if written.is_empty() {
                println!("nothing to write");
            }
        }
        return Ok(0);
    }
    if g.json {
        let files: Vec<&str> = preview.files.iter().map(|f| f.name.as_str()).collect();
        println!(
            "{}",
            json!({ "dir": preview.dir, "files": files, "diff": preview.diff, "hash": preview.hash })
        );
    } else if preview.files.is_empty() {
        println!("{} already matches; nothing to write", dir.display());
    } else {
        print!("{}", preview.diff);
        println!(
            "\nnothing was written. To write these files into {}:",
            dir.display()
        );
        println!("  aibus workflow export {target} --apply {}", preview.hash);
    }
    Ok(0)
}
