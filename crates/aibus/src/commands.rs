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

/// The account commands against the real keychain, environment, terminal and providers.
pub fn account(cmd: &AccountCmd, g: &Global, redactor: &Redactor) -> Result<u8, AibusError> {
    let stdin = std::io::stdin();
    // `--oauth` never reads the key from stdin, and its paste fallback reads a line itself, so
    // the lock stays free for it.
    let oauth_add = matches!(cmd, AccountCmd::Add(a) if a.oauth);
    let mut input: Box<dyn std::io::Read> =
        if oauth_add { Box::new(std::io::empty()) } else { Box::new(stdin.lock()) };
    let mut stdout = std::io::stdout().lock();
    let ask = || rpassword::prompt_password("key (input hidden): ");
    let mut io = Io {
        paths: g.paths()?,
        keychain: Arc::new(OsKeychain),
        factories: crate::providers::os_factories(redactor),
        env: Arc::new(|v| std::env::var(v).ok()),
        redactor: redactor.clone(),
        oauth: match cmd {
            AccountCmd::Add(a) if a.oauth => {
                crate::oauth::definition(&a.provider, a.base_url.as_deref()).map(|def| {
                    let tty = std::io::stdin().is_terminal();
                    let mut login = crate::oauth::Login::new(def);
                    (login.open_browser, login.paste) = (tty, tty);
                    login.redactor = redactor.clone();
                    Arc::new(login) as Arc<dyn aibus_core::OAuthFlow>
                })
            }
            _ => None,
        },
        cancel: None,
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
        for f in &found {
            println!("{}", f.name);
        }
        return Ok(0);
    }
    if g.json {
        let rows: Vec<_> = found
            .iter()
            .map(|f| {
                let mut row = match &f.parsed {
                    Ok(w) => json!({
                        "name": f.name,
                        "path": f.path,
                        "description": w.description,
                        "steps": w.steps.iter().map(|s| s.agent.as_str()).collect::<Vec<_>>(),
                    }),
                    Err(e) => json!({"name": f.name, "path": f.path, "error": e.to_string()}),
                };
                row["scope"] = json!(f.scope);
                row["bundle"] = json!(f.bundle);
                row["source"] = json!(f.source());
                row["runnable"] = json!(f.runnable().is_ok());
                if let (Ok(_), Err(why)) = (&f.parsed, f.runnable()) {
                    row["not_runnable"] = json!(why);
                }
                row
            })
            .collect();
        println!("{}", json!(rows));
        return Ok(0);
    }
    // name, description, steps, source; a broken file or a gate from a bundle says so instead.
    for f in &found {
        let (name, source) = (&f.name, f.source());
        match (&f.parsed, f.runnable()) {
            (Ok(w), Ok(_)) => {
                let agents: Vec<&str> = w.steps.iter().map(|s| s.agent.as_str()).collect();
                println!("{name}\t{}\t{}\t{source}", w.description, agents.join(" > "));
            }
            (Ok(_), Err(why)) => println!("{name}\t(not runnable: {why})\t\t{source}"),
            (Err(e), _) => println!("{name}\t(invalid: {e})\t{}\t{source}", f.path.display()),
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
    use aibus_core::workflow::{self, bundle, Problem};
    let cwd = g.project_dir()?;
    let dirs = workflow::dirs(&cwd, Some(&g.paths()?.config));
    // What to check: (the path to report, the name, the text, the bundle it came from). A bundle
    // that is refused is an error on its own path.
    let mut units: Vec<(std::path::PathBuf, String, String, Option<String>)> = Vec::new();
    let mut errors: Vec<(std::path::PathBuf, Problem)> = Vec::new();
    let problem = |message: String| Problem { step: None, field: None, message };
    let add_bundle = |path: std::path::PathBuf, units: &mut Vec<_>, errors: &mut Vec<_>| {
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned());
        match bundle::read_file(&path, &Redactor::new()) {
            Ok(b) => units.extend(b.workflows.into_iter().map(|(n, t)| (path.clone(), n, t, file.clone()))),
            Err(e) => errors.push((path, problem(format!("bundle refused: {e}")))),
        }
    };
    match target {
        Some(t) if t.ends_with(&format!(".{}", bundle::EXTENSION)) => add_bundle(cwd.join(t), &mut units, &mut errors),
        Some(t) if t.ends_with(".toml") || t.contains(std::path::MAIN_SEPARATOR) || cwd.join(t).is_file() => {
            let path = cwd.join(t);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
            let text = std::fs::read_to_string(&path).map_err(|e| AibusError::Usage(format!("{}: {e}", path.display())))?;
            units.push((path, stem, text, None));
        }
        Some(name) => {
            let found = workflow::list(&dirs)
                .into_iter()
                .find(|f| f.name == name)
                .ok_or_else(|| AibusError::Usage(format!("no workflow named {name} (aibus workflow list)")))?;
            let text = found.text.clone().ok_or_else(|| {
                let why = found.parsed.as_ref().err().map(ToString::to_string).unwrap_or_default();
                AibusError::Usage(format!("{}: {why}", found.path.display()))
            })?;
            units.push((found.path, found.name, text, found.bundle));
        }
        None => {
            for dir in &dirs {
                let Ok(read) = std::fs::read_dir(dir) else { continue };
                let mut here: Vec<_> = read.flatten().map(|e| e.path()).collect();
                here.sort();
                for path in here {
                    if path.extension().is_some_and(|x| x == "toml") {
                        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
                        let text = std::fs::read_to_string(&path)
                            .map_err(|e| AibusError::Usage(format!("{}: {e}", path.display())))?;
                        units.push((path, stem, text, None));
                    } else if path.extension().is_some_and(|x| x == bundle::EXTENSION) {
                        add_bundle(path, &mut units, &mut errors);
                    }
                }
            }
        }
    }
    let env = crate::agent_cmd::Env::load(g)?;
    let roster: Vec<_> = env.discover(g)?.agents.into_keys().collect();
    let mut warnings = Vec::new();
    for (path, stem, text, from) in &units {
        let v = workflow::validate(text, stem, &roster);
        let tag = |list: Vec<Problem>| list.into_iter().map(|p| (path.clone(), p)).collect::<Vec<_>>();
        errors.extend(tag(v.errors));
        warnings.extend(tag(v.warnings));
        // A gate from a bundle is someone else's shell command: listed, never run, until trusted.
        if let (Some(b), Ok(w)) = (from, workflow::Workflow::parse(stem, text)) {
            if let Some(i) = w.steps.iter().position(|s| s.gate.is_some()) {
                warnings.push((
                    path.clone(),
                    Problem {
                        step: Some(i + 1),
                        field: Some("gate".into()),
                        message: format!(
                            "{stem} comes from the bundle {b} and has a gate, so it does not run until you trust it: aibus workflow install {} --allow gates",
                            path.display()
                        ),
                    },
                ));
            }
        }
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
    let found = aibus_core::workflow::list(&dirs)
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| AibusError::Usage(format!("no workflow named {name} (aibus workflow list)")))?;
    let (path, source) = (found.path.clone(), found.source());
    let not_runnable = found.runnable().err();
    let w = found.parsed.map_err(|e| AibusError::Usage(format!("{}: {e}", path.display())))?;
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
        let mut shown = json!({"name": w.name, "path": path, "description": w.description, "steps": steps, "source": source});
        if let Some(why) = &not_runnable {
            shown["not_runnable"] = json!(why);
        }
        println!("{shown}");
        return Ok(0);
    }
    println!("{}  ({}, {source})", w.name, path.display());
    if let Some(why) = &not_runnable {
        println!("not runnable: {why}");
    }
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

/// `workflow pack <name>... -o <file>.aibus`: the named workflows (whichever source wins each
/// name), byte for byte, in one bundle named after the file. `--with-agents` adds the project or
/// user definition of each agent the steps name (built-ins need nothing).
pub fn workflow_pack(
    g: &Global,
    names: &[String],
    output: &std::path::Path,
    description: &str,
    with_agents: bool,
    force: bool,
    redactor: &Redactor,
) -> Result<u8, AibusError> {
    use aibus_core::agent::Scope;
    use aibus_core::workflow::{self, bundle};
    let usage = |m: String| AibusError::Usage(format!("workflow pack: {m}"));
    let stem = output
        .file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| f.strip_suffix(&format!(".{}", bundle::EXTENSION)))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| usage(format!("{} is not a <name>.{} file", output.display(), bundle::EXTENSION)))?;
    if !workflow::valid_name(stem) {
        return Err(usage(format!(
            "`{stem}` is the bundle's name and is not a name: lowercase letters, digits, - and _, starting with a letter or digit"
        )));
    }
    let dirs = workflow::dirs(&g.project_dir()?, Some(&g.paths()?.config));
    let found = workflow::list(&dirs);
    let mut flows: Vec<(String, String)> = Vec::new();
    let mut named: Vec<aibus_core::AgentName> = Vec::new();
    for name in names {
        if flows.iter().any(|(n, _)| n == name) {
            continue;
        }
        let f = found
            .iter()
            .find(|f| f.name == *name)
            .ok_or_else(|| usage(format!("no workflow named {name} (aibus workflow list)")))?;
        let (Ok(w), Some(text)) = (&f.parsed, &f.text) else {
            let why = f.parsed.as_ref().err().map(ToString::to_string).unwrap_or_default();
            return Err(usage(format!("{name} does not parse: {why}")));
        };
        named.extend(w.steps.iter().map(|s| s.agent.clone()).filter(|a| !named.contains(a)).collect::<Vec<_>>());
        flows.push((name.clone(), text.clone()));
    }
    let mut agents: Vec<(String, String)> = Vec::new();
    if with_agents {
        let found = crate::agent_cmd::Env::load(g)?.discover(g)?;
        for a in &named {
            let Some(def) = found.agents.get(a) else { continue };
            if let (Scope::Project | Scope::User, Some(path)) = (&def.source.scope, &def.source.path) {
                let text = std::fs::read_to_string(path).map_err(|e| usage(format!("{}: {e}", path.display())))?;
                agents.push((a.to_string(), text));
            }
        }
    }
    let bytes = bundle::pack(stem, description, &flows, &agents, None, redactor).map_err(|e| usage(e.to_string()))?;
    match std::fs::symlink_metadata(output) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(usage(format!("{}: {e}", output.display()))),
        Ok(md) if md.file_type().is_symlink() || !md.is_file() => {
            return Err(usage(format!("{} is not a plain file, refusing to write through it", output.display())))
        }
        Ok(_) if !force => return Err(usage(format!("{} exists, pack again with --force to replace it", output.display()))),
        Ok(_) => {}
    }
    aibus_core::persist::fs::write_atomic(output, &bytes).map_err(|e| usage(format!("{}: {e}", output.display())))?;
    if g.json {
        println!("{}", json!({"path": output, "name": stem, "workflows": names_of(&flows), "agents": names_of(&agents)}));
    } else {
        println!("wrote {} ({} workflow(s), {} agent(s))", output.display(), flows.len(), agents.len());
    }
    Ok(0)
}

fn names_of(list: &[(String, String)]) -> Vec<&str> {
    list.iter().map(|(n, _)| n.as_str()).collect()
}

/// `workflow install <file>.aibus [--user] [--force] [--allow gates]`: the bundle's workflows (and
/// agents) as loose files in the project's `.aibus/` or the user's config directory, through the
/// same checks as `workflow_create`. A gate (or an agent's hooks) needs `--allow gates`: that is
/// how a bundled gate gets trusted.
pub fn workflow_install(
    g: &Global,
    file: &std::path::Path,
    user: bool,
    force: bool,
    allow_gates: bool,
    redactor: &Redactor,
) -> Result<u8, AibusError> {
    use aibus_core::workflow::bundle::{self, Install};
    let usage = |m: String| AibusError::Usage(format!("workflow install: {m}"));
    let md = std::fs::metadata(file).map_err(|e| usage(format!("{}: {e}", file.display())))?;
    if !md.is_file() || md.len() > bundle::MAX_BUNDLE_BYTES as u64 {
        return Err(usage(format!("{} is not a bundle file of at most {} bytes", file.display(), bundle::MAX_BUNDLE_BYTES)));
    }
    let bytes = std::fs::read(file).map_err(|e| usage(format!("{}: {e}", file.display())))?;
    let paths = g.paths()?;
    let base = if user { paths.config.clone() } else { project_dir(&g.project_dir()?) };
    let roster: Vec<_> = crate::agent_cmd::Env::load(g)?.discover(g)?.agents.into_keys().collect();
    let to = Install {
        workflows_dir: base.join("workflows"),
        agents_dir: base.join("agents"),
        roster: &roster,
        force,
        allow_gates,
    };
    let wrote = bundle::install(&bytes, &to, redactor).map_err(|e| usage(e.to_string()))?;
    if g.json {
        println!("{}", json!({ "wrote": wrote }));
    } else {
        for p in &wrote {
            println!("wrote {}", p.display());
        }
    }
    Ok(0)
}
