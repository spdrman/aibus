//! The `config` subcommands, which need no engine.

use std::io::IsTerminal;
use std::sync::Arc;

use aibus_core::config;
use aibus_core::persist::paths::project_dir;
use aibus_core::{AibusError, Redactor};
use serde_json::json;

use crate::account::{self, Io};
use crate::cli::{AccountCmd, ConfigCmd, Global};
use crate::keychain::OsKeychain;
use crate::providers::builtin_factories;

/// The account commands against the real keychain, environment, terminal and providers.
pub fn account(cmd: &AccountCmd, g: &Global, redactor: &Redactor) -> Result<u8, AibusError> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut stdout = std::io::stdout().lock();
    let ask = || rpassword::prompt_password("key (input hidden): ");
    let mut io = Io {
        paths: g.paths()?,
        keychain: Arc::new(OsKeychain),
        factories: builtin_factories(),
        env: Arc::new(|v| std::env::var(v).ok()),
        redactor: redactor.clone(),
        // No provider ships an OAuth flow yet: `--oauth` says so (C2.5a).
        oauth: None,
        stdin: &mut input,
        prompt: std::io::stdin().is_terminal().then_some(&ask),
        out: &mut stdout,
    };
    account::run(cmd, g.json, &mut io)
}

/// `workflow list`: the workflows of `.aibus/workflows` and the user config (01 section 8.4), by
/// name, the project's winning a name both have. `--names` is the helper the completion scripts
/// call: names only.
pub fn workflow_list(g: &Global, names: bool) -> Result<u8, AibusError> {
    let dirs = aibus_core::workflow::dirs(&g.project_dir()?, Some(&g.paths()?.config));
    let found = aibus_core::workflow::list(&dirs);
    if names {
        for (name, _, _) in &found {
            println!("{name}");
        }
        return Ok(0);
    }
    if g.json {
        let rows: Vec<_> = found
            .iter()
            .map(|(name, path, parsed)| match parsed {
                Ok(w) => json!({
                    "name": name,
                    "path": path,
                    "description": w.description,
                    "steps": w.steps.iter().map(|s| s.agent.as_str()).collect::<Vec<_>>(),
                }),
                Err(e) => json!({"name": name, "path": path, "error": e.to_string()}),
            })
            .collect();
        println!("{}", json!(rows));
        return Ok(0);
    }
    for (name, path, parsed) in &found {
        match parsed {
            Ok(w) => {
                let agents: Vec<&str> = w.steps.iter().map(|s| s.agent.as_str()).collect();
                println!("{name}\t{}\t{}", w.description, agents.join(" > "));
            }
            Err(e) => println!("{name}\t(invalid: {e})\t{}", path.display()),
        }
    }
    Ok(0)
}

/// `aibus init`: puts the four shipped workflows in `.aibus/workflows/`, leaving any file that is
/// already there alone.
pub fn init(g: &Global) -> Result<u8, AibusError> {
    let dir = aibus_core::workflow::dirs(&g.project_dir()?, None).remove(0);
    std::fs::create_dir_all(&dir)?;
    let mut wrote = Vec::new();
    for (name, text) in aibus_core::workflow::EXAMPLES {
        let path = dir.join(format!("{name}.toml"));
        if path.exists() {
            continue;
        }
        std::fs::write(&path, text)?;
        wrote.push(path);
    }
    if g.json {
        println!("{}", json!({ "workflows": wrote }));
    } else if wrote.is_empty() {
        println!("{} already has the example workflows", dir.display());
    } else {
        for p in &wrote {
            println!("wrote {}", p.display());
        }
    }
    Ok(0)
}

pub fn config(cmd: &ConfigCmd, g: &Global) -> Result<u8, AibusError> {
    let paths = g.paths()?;
    let user = paths.config_file();
    let project = project_dir(&g.project_dir()?).join("settings.toml");
    match cmd {
        ConfigCmd::Path => {
            if g.json {
                println!("{}", json!({ "user": user, "project": project }));
            } else {
                println!("user\t{}", user.display());
                println!("project\t{}", project.display());
            }
        }
        ConfigCmd::Show => {
            let mut loaded = config::load(&user, Some(&project))?;
            crate::env::apply_limits(&mut loaded.config, |v| std::env::var(v).ok())?;
            for w in &loaded.warnings {
                eprintln!("aibus: warning: {w}");
            }
            if g.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&loaded.config).map_err(std::io::Error::other)?
                );
            } else {
                print!(
                    "{}",
                    toml::to_string_pretty(&loaded.config).map_err(std::io::Error::other)?
                );
            }
        }
        ConfigCmd::Init => {
            let text = toml::to_string_pretty(&config::Config::default())
                .map_err(std::io::Error::other)?;
            if let Some(dir) = user.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&user)?;
            std::io::Write::write_all(
                &mut file,
                format!("# aibus config: every value shown is the default (07 section 3).\n{text}")
                    .as_bytes(),
            )?;
            println!("wrote {}", user.display());
        }
        ConfigCmd::Validate => {
            let loaded = config::load(&user, Some(&project))?;
            for w in &loaded.warnings {
                eprintln!("aibus: warning: {w}");
            }
            if g.json {
                println!("{}", json!({ "ok": true, "warnings": loaded.warnings }));
            } else {
                println!("ok");
            }
        }
        ConfigCmd::Edit => {
            // `$EDITOR` may carry arguments ("code -w"), so the shell splits it; the path is
            // passed as $1 and never spliced into the script.
            let editor = std::env::var("EDITOR")
                .ok()
                .filter(|e| !e.trim().is_empty())
                .unwrap_or_else(|| "vi".into());
            if let Some(dir) = user.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{editor} \"$1\""))
                .arg("sh")
                .arg(&user)
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other(format!("{editor}: {status}")).into());
            }
            // A config that no longer parses is exit 3 now, not at the next run.
            let loaded = config::load(&user, Some(&project))?;
            for w in &loaded.warnings {
                eprintln!("aibus: warning: {w}");
            }
        }
    }
    Ok(0)
}

/// `workflow validate [<name> | <path>]`: the roster is whatever agent discovery finds, so a
/// workflow naming a project agent passes in that project. With no argument, every `*.toml` in
/// both workflow directories is checked, hidden names included. Exit 12 on errors, as
/// `agent validate` does; warnings leave it at 0.
pub fn workflow_validate(g: &Global, target: Option<&str>) -> Result<u8, AibusError> {
    let cwd = g.project_dir()?;
    let dirs = aibus_core::workflow::dirs(&cwd, Some(&g.paths()?.config));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    match target {
        Some(t) if t.ends_with(".toml") || t.contains(std::path::MAIN_SEPARATOR) || cwd.join(t).is_file() => {
            files.push(cwd.join(t));
        }
        Some(name) => {
            let found = aibus_core::workflow::list(&dirs);
            let (_, path, _) = found
                .into_iter()
                .find(|(n, _, _)| n == name)
                .ok_or_else(|| AibusError::Usage(format!("no workflow named {name} (aibus workflow list)")))?;
            files.push(path);
        }
        None => {
            for dir in &dirs {
                let Ok(read) = std::fs::read_dir(dir) else { continue };
                let mut here: Vec<_> = read.flatten().map(|e| e.path()).collect();
                here.sort();
                files.extend(here.into_iter().filter(|p| p.extension().is_some_and(|x| x == "toml")));
            }
        }
    }
    let env = crate::agent_cmd::Env::load(g)?;
    let roster: Vec<_> = env.discover(g)?.agents.into_keys().collect();
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    for path in &files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let text = std::fs::read_to_string(path)
            .map_err(|e| AibusError::Usage(format!("{}: {e}", path.display())))?;
        let v = aibus_core::workflow::validate(&text, stem, &roster);
        let tag = |list: Vec<aibus_core::workflow::Problem>| {
            list.into_iter().map(|p| (path.clone(), p)).collect::<Vec<_>>()
        };
        errors.extend(tag(v.errors));
        warnings.extend(tag(v.warnings));
    }
    let row = |(path, p): &(std::path::PathBuf, aibus_core::workflow::Problem)| {
        json!({"path": path, "step": p.step, "field": p.field, "message": p.message})
    };
    let ok = errors.is_empty();
    if g.json {
        let report = json!({
            "ok": ok,
            "errors": errors.iter().map(row).collect::<Vec<_>>(),
            "warnings": warnings.iter().map(row).collect::<Vec<_>>(),
        });
        println!("{report}");
    } else {
        for (kind, list) in [("error", &errors), ("warning", &warnings)] {
            for (path, p) in list {
                let step = p.step.map_or(String::new(), |n| format!("step {n}: "));
                let field = p.field.as_deref().map_or(String::new(), |f| format!("{f}: "));
                println!("{kind}: {}: {step}{field}{}", path.display(), p.message);
            }
        }
        if ok {
            println!("ok");
        }
    }
    Ok(if ok { 0 } else { 12 })
}

/// `workflow show <name>`: the workflow as it will run, from whichever directory wins the name.
pub fn workflow_show(g: &Global, name: &str) -> Result<u8, AibusError> {
    let dirs = aibus_core::workflow::dirs(&g.project_dir()?, Some(&g.paths()?.config));
    let (_, path, parsed) = aibus_core::workflow::list(&dirs)
        .into_iter()
        .find(|(n, _, _)| n == name)
        .ok_or_else(|| AibusError::Usage(format!("no workflow named {name} (aibus workflow list)")))?;
    let w = parsed.map_err(|e| AibusError::Usage(format!("{}: {e}", path.display())))?;
    let on_fail = |retries: u32| (retries > 0).then(|| format!("retry:{retries}"));
    if g.json {
        let steps: Vec<_> = w
            .steps
            .iter()
            .map(|s| {
                let mut step = json!({"agent": s.agent.as_str(), "prompt": s.prompt});
                if let Some(g) = &s.gate {
                    step["gate"] = json!(g);
                }
                if let Some(r) = on_fail(s.retries) {
                    step["on_fail"] = json!(r);
                }
                step
            })
            .collect();
        println!("{}", json!({"name": w.name, "path": path, "description": w.description, "steps": steps}));
        return Ok(0);
    }
    println!("{}  ({})", w.name, path.display());
    if !w.description.is_empty() {
        println!("{}", w.description);
    }
    for (i, s) in w.steps.iter().enumerate() {
        println!("\nstep {}: {}", i + 1, s.agent);
        for line in s.prompt.trim_end().lines() {
            println!("  {line}");
        }
        if let Some(gate) = &s.gate {
            println!("  gate: {gate}");
        }
        if let Some(r) = on_fail(s.retries) {
            println!("  on_fail: {r}");
        }
    }
    Ok(0)
}
