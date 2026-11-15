//! The sandbox host: the only place a manifest becomes a process. The extension chooses nothing
//! the host does not allow: the executable is one of its declared file names found on the host's
//! own search path (by `cli::discover`, which refuses a relative directory and a file others can
//! write), the argv is its static tokens plus the optional flags the probe found, the environment
//! is `cli::bounded_env` over its declared names, the directory is the one the host granted, and
//! the limits are the smaller of what it asked and the host's ceiling. The program has to be on
//! the host's allowlist of official CLIs ([`OFFICIAL_CLIS`] and the user's additions). The keychain
//! is never opened here and no `AIBUS_*`, credential-shaped or loader variable is passed. This is
//! not an operating system sandbox: the CLI runs as the user, with `HOME`, and can reach what the
//! user can.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use std::time::Duration;

use aibus_supervisor::cli::{self, Probe as CliProbe, ProbeError, HOST_ENV_ALLOW};
use aibus_supervisor::{Engine, Launch, Limits};
use aibus_trace::AttemptRef;

use crate::manifest::{Capability, Manifest, ValueKind};
use crate::negotiate::{check_host, negotiate, Incompatible, Negotiated};

/// What the host grants one launch. The extension cannot widen it.
/// The program names the host runs as an official CLI. A user can add their own in the policy
/// file (`extra_official_clis`); a manifest never can.
pub const OFFICIAL_CLIS: [&str; 4] = ["claude", "codex", "gemini", "kimi"];

/// One agent the host defines for a run (`--agents`): the name, what it is for and its prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub description: String,
    pub prompt: String,
}

/// A value the host fills in from the plan for a typed flag (#334).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypedValue {
    /// A model id (`gpt-5`, `claude-sonnet-4`).
    Model(String),
    Agents(Vec<Agent>),
    /// A path relative to the working directory.
    Path(String),
}

#[derive(Clone, Debug)]
pub struct Grant {
    /// The only directory the CLI runs in. Absolute, existing; resolved before use.
    pub workdir: PathBuf,
    /// Absolute directories searched for the executable, in order.
    pub search_dirs: Vec<PathBuf>,
    /// The host's environment, from which only allowed names are taken.
    pub parent_env: Vec<(OsString, OsString)>,
    /// The most any extension may use; the manifest's request is clamped to it.
    pub ceiling: Limits,
    /// Program names the user added to the allowlist in their own policy file.
    pub extra_programs: Vec<String>,
    /// `macos`, `linux` or `windows`.
    pub os: String,
    /// The values of the typed flags, by flag name. A flag with no value here is left out.
    pub typed: std::collections::BTreeMap<String, TypedValue>,
}

#[derive(Debug)]
pub struct Prepared {
    /// Ready for `supervise`, but for the prompt, which the caller puts in `stdin`.
    pub launch: Launch,
    pub negotiated: Negotiated,
    pub probe: CliProbe,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("none of {names:?} was found as a safe executable: {why:?}")]
    NoExecutable {
        names: Vec<String>,
        why: Vec<String>,
    },
    #[error("{0} is not on the host's allowlist of official CLIs")]
    NotOfficial(String),
    #[error("probe: {0}")]
    Probe(#[from] ProbeError),
    #[error("{0}")]
    Incompatible(#[from] Incompatible),
    #[error("the extension asked for {0} but the CLI does not support it")]
    CapabilityMissing(String),
    #[error("environment: {0}")]
    Env(String),
    #[error("workdir: {0}")]
    Workdir(String),
    #[error("refused: {0}")]
    Refused(String),
}

fn refused(why: impl Into<String>) -> SandboxError {
    SandboxError::Refused(why.into())
}

/// The checks every launch has to pass whoever built it: an absolute executable whose file name is
/// one the manifest declared, argv that is the manifest's tokens plus its optional flags only, an
/// environment of allowed names only, and an existing absolute working directory.
pub fn check_launch(m: &Manifest, launch: &Launch, extra: &[String]) -> Result<(), SandboxError> {
    let name = launch
        .exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if !launch.exe.is_absolute() || !m.cli.executable_candidates.iter().any(|c| c == name) {
        return Err(refused(format!(
            "{} is not an absolute path to one of the declared executables {:?}",
            launch.exe.display(),
            m.cli.executable_candidates
        )));
    }
    if !OFFICIAL_CLIS.contains(&name) && !extra.iter().any(|e| e == name) {
        return Err(SandboxError::NotOfficial(name.to_owned()));
    }
    let fixed: Vec<OsString> = m.execution.argv.iter().map(OsString::from).collect();
    if launch.args.len() < fixed.len() || launch.args[..fixed.len()] != fixed[..] {
        return Err(refused(
            "the arguments do not start with the manifest's argv",
        ));
    }
    // After the manifest's argv: its optional flags, and its typed flags each once with a value of
    // the kind it declared. Nothing else.
    let mut statics: Vec<String> = fixed
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut tail = launch.args[fixed.len()..].iter();
    let mut seen: Vec<&str> = Vec::new();
    while let Some(arg) = tail.next() {
        let text = arg.to_string_lossy();
        if let Some(t) = m.execution.typed.iter().find(|t| t.flag == text) {
            let value = tail
                .next()
                .and_then(|v| v.to_str())
                .ok_or_else(|| refused(format!("{text} has no value")))?;
            if seen.contains(&t.flag.as_str()) {
                return Err(refused(format!("{text} is given twice")));
            }
            seen.push(&t.flag);
            check_typed(t.kind, value, &launch.cwd)
                .map_err(|why| refused(format!("{text}: {why}")))?;
        } else if m
            .execution
            .optional
            .iter()
            .any(|o| OsString::from(&o.flag) == *arg)
        {
            statics.push(text.into_owned());
        } else {
            return Err(refused(format!(
                "{arg:?} is not an optional or typed flag the manifest declared"
            )));
        }
    }
    // The same list of flags the manifest was read against, over what is really about to run.
    crate::flags::check(name, &statics).map_err(refused)?;
    for (k, v) in &launch.env {
        let Some(k) = k.to_str() else {
            return Err(refused("an environment name is not UTF-8"));
        };
        if k == "PATH" {
            if v != "/usr/bin:/bin" {
                return Err(refused("PATH is the host's, /usr/bin:/bin"));
            }
        } else if !HOST_ENV_ALLOW.contains(&k) && !m.execution.env_allow.iter().any(|n| n == k) {
            return Err(refused(format!(
                "{k} is not an environment name the manifest declared"
            )));
        }
    }
    if !launch.cwd.is_absolute() || !launch.cwd.is_dir() {
        return Err(refused(format!(
            "{} is not an existing absolute directory",
            launch.cwd.display()
        )));
    }
    Ok(())
}

/// The argument pairs for the typed flags the manifest declared and the plan has a value for (#334),
/// in the manifest's order. The value of a flag is one argument, built here from what the plan
/// holds and checked by [`check_typed`], the same check `check_launch` runs on a finished launch.
/// A value for a flag the manifest did not declare, or of another kind, is refused; a declared flag
/// with no value is left out.
pub fn typed_args(
    m: &Manifest,
    values: &std::collections::BTreeMap<String, TypedValue>,
    workdir: &Path,
) -> Result<Vec<OsString>, SandboxError> {
    if let Some(flag) = values
        .keys()
        .find(|k| !m.execution.typed.iter().any(|t| &t.flag == *k))
    {
        return Err(refused(format!(
            "{flag} is not a typed flag the manifest declared"
        )));
    }
    let root = std::fs::canonicalize(workdir)
        .map_err(|e| SandboxError::Workdir(format!("{}: {e}", workdir.display())))?;
    let mut out = Vec::new();
    for t in &m.execution.typed {
        let Some(v) = values.get(&t.flag) else {
            continue;
        };
        let text = match (t.kind, v) {
            (ValueKind::ModelId, TypedValue::Model(id)) => id.clone(),
            (ValueKind::AgentList, TypedValue::Agents(list)) => {
                let mut object = serde_json::Map::new();
                for a in list {
                    let one =
                        serde_json::json!({ "description": a.description, "prompt": a.prompt });
                    if object.insert(a.name.clone(), one).is_some() {
                        return Err(refused(format!(
                            "{}: the agent {:?} is listed twice",
                            t.flag, a.name
                        )));
                    }
                }
                serde_json::Value::Object(object).to_string()
            }
            (ValueKind::WorkdirPath, TypedValue::Path(rel)) => {
                let plain = !rel.is_empty()
                    && rel.split('/').all(|p| !p.is_empty() && p != ".")
                    && Path::new(rel).is_relative()
                    && Path::new(rel)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)));
                if !plain {
                    return Err(refused(format!(
                        "{}: {rel:?} is not a plain relative path",
                        t.flag
                    )));
                }
                root.join(rel).to_string_lossy().into_owned()
            }
            _ => {
                return Err(refused(format!(
                    "{}: the plan's value is not a {:?}",
                    t.flag, t.kind
                )))
            }
        };
        check_typed(t.kind, &text, &root).map_err(|why| refused(format!("{}: {why}", t.flag)))?;
        out.push(OsString::from(&t.flag));
        out.push(OsString::from(text));
    }
    Ok(out)
}

/// Whether `value` is a well-formed value of `kind` for a CLI running in `workdir`. This is the
/// invariant of the typed flags: nothing the manifest or the plan holds reaches the argv except
/// through it.
fn check_typed(kind: ValueKind, value: &str, workdir: &Path) -> Result<(), String> {
    let re = |pattern: &str, v: &str| regex::Regex::new(pattern).expect("valid regex").is_match(v);
    match kind {
        ValueKind::ModelId => {
            if !re(r"^[A-Za-z0-9][A-Za-z0-9._:/+-]{0,127}$", value) {
                return Err(format!("{value:?} is not a model id"));
            }
        }
        ValueKind::AgentList => {
            let v: serde_json::Value =
                serde_json::from_str(value).map_err(|_| "the agents are not JSON".to_owned())?;
            let list = v.as_object().filter(|o| (1..=16).contains(&o.len()));
            let list = list.ok_or("the agents are not an object of one to 16 agents")?;
            for (name, def) in list {
                let fields = def.as_object().filter(|d| d.len() == 2);
                let text = |k: &str, max: usize| {
                    fields
                        .and_then(|d| d.get(k))
                        .and_then(serde_json::Value::as_str)
                        .filter(|t| (1..=max).contains(&t.chars().count()) && !t.contains('\0'))
                };
                if !re(r"^[a-z][a-z0-9-]{0,47}$", name)
                    || text("description", 200).is_none()
                    || text("prompt", 8000).is_none()
                {
                    return Err(format!("the agent {name:?} is not a name, a description and a prompt within the limits"));
                }
            }
        }
        ValueKind::WorkdirPath => {
            let root = std::fs::canonicalize(workdir).map_err(|e| e.to_string())?;
            let path = Path::new(value);
            let name = path.file_name().and_then(|n| n.to_str());
            if value.len() > 512
                || value.chars().any(char::is_control)
                || !path.is_absolute()
                || name.is_none()
            {
                return Err(format!("{value:?} is not a plain absolute path"));
            }
            // The directory it is in has to resolve inside the working directory, and so does the
            // path itself when it exists, so a link cannot lead out.
            let inside = |p: &Path| {
                std::fs::canonicalize(p)
                    .ok()
                    .is_some_and(|c| c.starts_with(&root))
            };
            let parent_ok = path.parent().is_some_and(inside);
            let self_ok = path.symlink_metadata().is_err() || inside(path);
            if !parent_ok || !self_ok {
                return Err(format!("{value:?} is not inside the working directory"));
            }
        }
    }
    Ok(())
}

/// Finds and probes the CLI, negotiates, and builds the launch. `want` are the optional
/// capabilities the caller would like the optional flags for; one the CLI lacks is an error, not a
/// silent drop.
pub async fn prepare(
    m: &Manifest,
    grant: &Grant,
    want: &[Capability],
) -> Result<Prepared, SandboxError> {
    check_host(m, &grant.os)?;
    // Which programs may run is the host's decision (its four, and the user's own additions), never
    // the manifest's: a manifest names a program, and only a name on the list is searched for.
    let official =
        |n: &str| OFFICIAL_CLIS.contains(&n) || grant.extra_programs.iter().any(|e| e == n);
    let programs: Vec<String> = m
        .cli
        .executable_candidates
        .iter()
        .filter(|n| official(n))
        .cloned()
        .collect();
    if programs.is_empty() {
        return Err(SandboxError::NotOfficial(
            m.cli.executable_candidates[0].clone(),
        ));
    }
    let names: Vec<&str> = m.execution.env_allow.iter().map(String::as_str).collect();
    let env = cli::bounded_env(grant.parent_env.iter().cloned(), &names, &[])
        .map_err(|e| SandboxError::Env(e.to_string()))?;

    let mut why = Vec::new();
    let mut found = None;
    for name in &programs {
        let d = cli::discover_from(
            &grant.search_dirs,
            name,
            &env,
            crate::version::Version(0, 0, 0),
        )
        .await;
        why.extend(d.candidates.iter().filter_map(|c| {
            c.verdict
                .as_ref()
                .err()
                .map(|e| format!("{}: {e}", c.path.display()))
        }));
        if let Some(c) = d.chosen.and_then(|i| d.candidates.get(i)) {
            found = Some(c.clone());
            break;
        }
    }
    let Some(found) = found else {
        return Err(SandboxError::NoExecutable {
            names: m.cli.executable_candidates.clone(),
            why,
        });
    };
    let real = found.resolved.clone().unwrap_or_else(|| found.path.clone());
    // `claude` that is really a shell or an interpreter is not the official CLI.
    let target = real
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if crate::manifest::forbidden_program(target) {
        return Err(SandboxError::NotOfficial(target.to_owned()));
    }
    let say = |args: &'static [&'static str]| async {
        cli::capture(&real, args, &env, Duration::from_secs(10), 256 * 1024)
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| SandboxError::NoExecutable {
                names: m.cli.executable_candidates.clone(),
                why: vec![format!("{}: {e}", real.display())],
            })
    };
    let (version_out, help_out) = (say(&["--version"]).await?, say(&["--help"]).await?);
    let probe = match m.normalizer.engine {
        // The host's own Claude gate: required flags and the bare-mode refusal.
        crate::manifest::Engine::ClaudeStreamJson => cli::probe(&version_out, &help_out)?,
        crate::manifest::Engine::DeclarativeJsonl => cli::parse_help(&version_out, &help_out)?,
    };
    let negotiated = negotiate(m, &grant.os, &probe)?;

    let cwd = std::fs::canonicalize(&grant.workdir)
        .ok()
        .filter(|p| grant.workdir.is_absolute() && p.is_dir())
        .ok_or_else(|| {
            SandboxError::Workdir(format!(
                "{} is not an existing absolute directory",
                grant.workdir.display()
            ))
        })?;

    let mut args: Vec<OsString> = m.execution.argv.iter().map(OsString::from).collect();
    // The host's own values for the typed flags, from the plan, right after the manifest's argv.
    args.extend(typed_args(m, &grant.typed, &cwd)?);
    for c in want {
        let Some(opt) = m.execution.optional.iter().find(|o| o.capability == *c) else {
            return Err(SandboxError::CapabilityMissing(format!(
                "{c:?} (the manifest has no optional flag for it)"
            )));
        };
        if !negotiated.supports(*c) {
            return Err(SandboxError::CapabilityMissing(format!(
                "{c:?} ({} is not offered by this CLI)",
                opt.flag
            )));
        }
        args.push(OsString::from(&opt.flag));
    }

    // The smaller of what the manifest asked and the host's ceiling, field by field.
    let ask = m.limits.clone().unwrap_or_default();
    let mut limits = grant.ceiling;
    if let Some(s) = ask.wall_time_secs {
        limits.wall_time = limits.wall_time.min(Duration::from_secs(s));
    }
    if let Some(b) = ask.max_stdout_bytes {
        limits.max_stdout_bytes = limits.max_stdout_bytes.min(b);
    }
    if let Some(b) = ask.max_line_bytes {
        limits.max_line_bytes = limits
            .max_line_bytes
            .min(usize::try_from(b).unwrap_or(usize::MAX));
    }
    if let Some(b) = ask.max_stderr_bytes {
        limits.max_stderr_bytes = limits.max_stderr_bytes.min(b);
    }
    if let Some(n) = ask.lines_per_second {
        limits.lines_per_second = limits.lines_per_second.min(n);
    }

    let launch = Launch {
        // The name the manifest declared, in the directory the host searched. Its target was
        // checked by `discover`.
        exe: found.path.clone(),
        args,
        cwd,
        env,
        stdin: Vec::new(),
        limits,
    };
    check_launch(m, &launch, &grant.extra_programs)?;
    Ok(Prepared {
        launch,
        negotiated,
        probe,
    })
}

/// The engine that reads this extension's events.
pub fn engine(m: &Manifest, attempt: AttemptRef) -> Box<dyn Engine> {
    match m.normalizer.engine {
        crate::manifest::Engine::ClaudeStreamJson => {
            Box::new(aibus_supervisor::Normalizer::new(attempt))
        }
        crate::manifest::Engine::DeclarativeJsonl => {
            Box::new(crate::declarative::Declarative::new(
                attempt,
                m.normalizer.rule.clone(),
                m.runtime.usage_quality,
            ))
        }
    }
}
