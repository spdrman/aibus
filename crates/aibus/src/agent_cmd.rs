//! The `agent` subcommands that need no engine: list, show, new, validate and where
//! (06 section 1). `agent import` is C1.5.

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use aibus_core::agent::def::{model_str, to_json, to_markdown};
use aibus_core::agent::discover::{discover, project_root, scope_dirs, Discovered};
use aibus_core::agent::validate::{validate_path, Report, ValidateCx};
use aibus_core::config::{self, Config};
use aibus_core::persist::paths::{project_dir, Paths};
use aibus_core::{AccountName, AgentName, AibusError, ModelRef, Scope};
use aibus_provider::ProviderRegistry;
use serde_json::{json, Value};

use crate::cli::{AgentCmd, Global};
use crate::providers::{builtin_factories, provider_configs};

/// What every subcommand starts from: where definitions live and the config that says so.
pub(crate) struct Env {
    cwd: PathBuf,
    user_dir: PathBuf,
    config: Config,
    paths: Paths,
}

impl Env {
    pub(crate) fn load(g: &Global) -> Result<Self, AibusError> {
        let cwd = g.project_dir()?;
        let paths = g.paths()?;
        let loaded = config::load(&paths.config_file(), Some(&project_dir(&cwd).join("settings.toml")))?;
        for w in &loaded.warnings {
            eprintln!("aibus: warning: {w}");
        }
        Ok(Self {
            user_dir: paths.config.join("agents"),
            cwd,
            config: loaded.config,
            paths,
        })
    }

    /// `agents.extra_dirs` or `agents.trusted_dirs`, relative to the project.
    fn dirs(&self, listed: &[String]) -> Vec<PathBuf> {
        listed.iter().map(|d| self.cwd.join(d)).collect()
    }

    fn extra_dirs(&self) -> Vec<PathBuf> {
        self.dirs(&self.config.agents.extra_dirs)
    }

    pub(crate) fn discover(&self, g: &Global) -> Result<Discovered, AibusError> {
        let found = discover(
            g.agents.as_deref(),
            &self.cwd,
            Some(&self.user_dir),
            &self.extra_dirs(),
            &self.dirs(&self.config.agents.trusted_dirs),
        )
        .map_err(|e| AibusError::Usage(e.to_string()))?;
        for w in &found.warnings {
            eprintln!("aibus: warning: {w}");
        }
        Ok(found)
    }
}

pub fn agent(cmd: &AgentCmd, g: &Global) -> Result<u8, AibusError> {
    let env = Env::load(g)?;
    match cmd {
        AgentCmd::List { names } => list(&env, g, *names),
        AgentCmd::Show { name } => show(&env, g, name),
        AgentCmd::New { name } => new(&env, name),
        AgentCmd::Validate { path } => validate(&env, g, path.as_deref()),
        AgentCmd::Where => where_(&env, g),
        AgentCmd::Import { dir } => import(&env, g, dir),
    }
}

/// `agent import <dir>`: copies the definitions into `.aibus/agents/` with their hooks turned off
/// and prints every hook with its command text (09 section 6).
fn import(env: &Env, g: &Global, dir: &Path) -> Result<u8, AibusError> {
    use aibus_core::agent::import::{import, Outcome};
    let from = env.cwd.join(dir);
    let to = project_dir(&env.cwd).join("agents");
    let done = import(&from, &to).map_err(AibusError::Usage)?;
    if g.json {
        let rows: Vec<Value> = done
            .iter()
            .map(|i| {
                let (status, detail) = match &i.outcome {
                    Outcome::Copied(p) => ("copied", p.display().to_string()),
                    Outcome::Skipped(why) => ("skipped", why.clone()),
                };
                json!({
                    "file": i.file,
                    "status": status,
                    "detail": detail,
                    "hooks": i.hooks.iter().map(|h| json!({
                        "event": h.event, "matcher": h.matcher, "text": h.text, "enabled": false,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", Value::Array(rows));
        return Ok(0);
    }
    for i in &done {
        match &i.outcome {
            Outcome::Copied(p) => println!("imported {} -> {}", i.file.display(), p.display()),
            Outcome::Skipped(why) => println!("skipped {}: {why}", i.file.display()),
        }
        for h in &i.hooks {
            let matcher = h.matcher.as_deref().map_or(String::new(), |m| format!(" [{m}]"));
            println!("  hook DISABLED {}{matcher}: {}", h.event, h.text);
        }
    }
    if done.iter().any(|i| !i.hooks.is_empty()) {
        println!("hooks are off. To turn one on, read it and rename `disabled_hooks` to `hooks` in the file.");
    }
    if done.is_empty() {
        println!("no definitions (*.md) in {}", from.display());
    }
    Ok(0)
}

fn scope_name(s: &Scope) -> &'static str {
    match s {
        Scope::Cli => "cli",
        Scope::Project => "project",
        Scope::User => "user",
        Scope::Extra(_) => "extra",
        Scope::Builtin => "builtin",
    }
}

fn list(env: &Env, g: &Global, names: bool) -> Result<u8, AibusError> {
    let found = env.discover(g)?;
    if names {
        for name in found.agents.keys() {
            println!("{name}");
        }
        return Ok(0);
    }
    let rows: Vec<Value> = found
        .agents
        .values()
        .map(|d| {
            let full = to_json(d);
            json!({
                "name": d.name.as_str(),
                "scope": scope_name(&d.source.scope),
                "path": d.source.path,
                "role": full["role"],
                "model": model_str(&d.model),
                "tools": full.get("tools"),
                "description": d.description,
            })
        })
        .collect();
    if g.json {
        println!("{}", Value::Array(rows));
    } else {
        for r in &rows {
            let text = |k: &str| r[k].as_str().unwrap_or("-").to_owned();
            println!(
                "{}\t{}\t{}\t{}\t{}",
                text("name"),
                text("scope"),
                text("role"),
                text("model"),
                text("description")
            );
        }
    }
    Ok(0)
}

fn show(env: &Env, g: &Global, name: &str) -> Result<u8, AibusError> {
    let wanted = AgentName::new(name).map_err(|e| AibusError::Usage(e.to_string()))?;
    let found = env.discover(g)?;
    let def = found
        .agents
        .get(&wanted)
        .ok_or_else(|| AibusError::Usage(format!("no agent named {wanted}")))?;
    if g.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&to_json(def)).map_err(std::io::Error::other)?
        );
    } else {
        print!("{}", to_markdown(def));
    }
    Ok(0)
}

fn new(env: &Env, name: &str) -> Result<u8, AibusError> {
    let name = AgentName::new(name).map_err(|e| AibusError::Usage(e.to_string()))?;
    // A project gets its own definition (it is meant for version control); with no project
    // it goes in the user scope.
    let dir = match project_root(&env.cwd) {
        Some(root) => root.join(".aibus/agents"),
        None => env.user_dir.clone(),
    };
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.md"));
    let template = format!(
        "---\nname: {name}\ndescription: Say when the lead should hand a task to this agent.\n\
         role: worker\nmodel: inherit\ntools: [read, grep, glob, ls]\n---\n\n\
         Say what this agent does and how it reports back.\n"
    );
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                AibusError::Usage(format!("{} already exists", path.display()))
            }
            _ => e.into(),
        })?;
    file.write_all(template.as_bytes())?;
    drop(file);
    println!("wrote {}", path.display());

    // Without a terminal there is nobody to edit it, so leave the template where it is.
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|e| !e.is_empty())
        .or_else(|| std::env::var("EDITOR").ok().filter(|e| !e.is_empty()));
    let Some(editor) = editor.filter(|_| std::io::stdin().is_terminal()) else {
        return Ok(0);
    };
    let mut words = editor.split_whitespace();
    let status = std::process::Command::new(words.next().unwrap_or("vi"))
        .args(words)
        .arg(&path)
        .status()?;
    if !status.success() {
        return Ok(1);
    }
    let report = check(env, &path)?;
    print_report(&report);
    Ok(if report.ok { 0 } else { 12 })
}

/// Validates `path` against the configured accounts, the providers this binary has, the tools
/// and the hook commands (relative to the project).
fn check(env: &Env, path: &Path) -> Result<Report, AibusError> {
    let cfg = &env.config;
    let mut accounts = vec![AccountName("fake".into())];
    accounts.extend(cfg.accounts.iter().map(|a| AccountName(a.name.as_str().into())));

    // "Resolves on some account": the fake account and every enabled one. A provider this
    // build does not have cannot be asked, so its accounts give the benefit of the doubt.
    let configs = provider_configs(&cfg.accounts, &env.paths)?;
    let registry = ProviderRegistry::new(&builtin_factories(), &configs)?;
    let mut ids = vec!["fake"];
    ids.extend(
        cfg.accounts
            .iter()
            .filter(|a| a.enabled)
            .map(|a| a.provider.as_str()),
    );
    let providers: Vec<_> = ids.iter().map(|id| registry.get(id).ok()).collect();
    let resolves = |r: &ModelRef| {
        providers
            .iter()
            .any(|p| p.as_ref().is_none_or(|p| p.resolve_model(r, &cfg.models).is_some()))
    };
    let project = project_root(&env.cwd).unwrap_or(&env.cwd);
    let cx = ValidateCx {
        accounts: &accounts,
        model_resolves: &resolves,
        project_dir: project,
    };
    Ok(validate_path(path, &cx))
}

fn validate(env: &Env, g: &Global, path: Option<&Path>) -> Result<u8, AibusError> {
    let report = match path {
        Some(p) => check(env, p)?,
        None => {
            let mut all = Report::default();
            // A scope nobody has created yet is empty, not an error.
            let dirs = scope_dirs(&env.cwd, Some(&env.user_dir), &env.extra_dirs());
            for (_, dir) in dirs.iter().filter(|(_, d)| d.is_dir()) {
                all.merge(check(env, dir)?);
            }
            all.ok = all.errors.is_empty();
            all
        }
    };
    if g.json {
        println!("{}", serde_json::to_string(&report).map_err(std::io::Error::other)?);
    } else {
        print_report(&report);
    }
    Ok(if report.ok { 0 } else { 12 })
}

fn print_report(report: &Report) {
    for (kind, list) in [("error", &report.errors), ("warning", &report.warnings)] {
        for f in list {
            let path = f.path.as_deref().map_or("-".into(), |p| p.display().to_string());
            println!("{kind}: {path}: {}: {}", f.field, f.message);
        }
    }
    if report.ok {
        println!("ok");
    }
}

fn where_(env: &Env, g: &Global) -> Result<u8, AibusError> {
    let mut rows: Vec<(&str, Option<PathBuf>)> = Vec::new();
    let dirs = scope_dirs(&env.cwd, Some(&env.user_dir), &env.extra_dirs());
    if !dirs.iter().any(|(s, _)| *s == Scope::Project) {
        rows.push(("project", None));
    }
    rows.extend(dirs.into_iter().map(|(s, d)| (scope_name(&s), Some(d))));
    rows.push(("builtin", None));
    if g.json {
        let list: Vec<Value> = rows
            .iter()
            .map(|(scope, dir)| {
                json!({ "scope": scope, "path": dir, "exists": dir.as_ref().map(|d| d.is_dir()) })
            })
            .collect();
        println!("{}", Value::Array(list));
    } else {
        for (scope, dir) in &rows {
            match dir {
                Some(d) => println!("{scope}\t{}", d.display()),
                None if *scope == "builtin" => println!("{scope}\t(compiled in)"),
                None => println!("{scope}\t(no project here)"),
            }
        }
    }
    Ok(0)
}
