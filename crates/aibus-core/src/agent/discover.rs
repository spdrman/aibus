//! Finding definitions across the five scopes (01 section 1.2). The first match by name wins.

use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::Value;

use super::builtin::BUILTINS;
use super::def::{from_fields, parse, DefError};
use super::roster;
use super::{AgentDef, DefSource, PermissionMode, Scope};
use crate::ids::AgentName;

#[derive(Debug, Default)]
pub struct Discovered {
    /// In precedence order: `--agents`, project, user, extra dirs, built-ins.
    pub agents: IndexMap<AgentName, AgentDef>,
    pub warnings: Vec<String>,
    /// The roster text (01 section 1.5), for the `delegate` description.
    pub roster: String,
}

/// Most `*.md` files read from one scope directory (08 section 8).
pub const MAX_FILES_PER_SCOPE: usize = 1_000;

/// Walk the scopes. A file that fails to parse is skipped with a warning; a bad `--agents`
/// value is an error, since the user typed it for this run.
///
/// `user_dir` is the already resolved `$XDG_CONFIG_HOME/aibus/agents`. Definitions from an
/// `extra_dirs` entry lose their hooks unless the same directory is in `trusted_dirs`.
pub fn discover(
    cli_json: Option<&str>,
    cwd: &Path,
    user_dir: Option<&Path>,
    extra_dirs: &[PathBuf],
    trusted_dirs: &[PathBuf],
) -> Result<Discovered, DefError> {
    let mut found = Vec::new();
    let mut warnings = Vec::new();

    if let Some(json) = cli_json {
        let Value::Object(entries) =
            serde_json::from_str(json).map_err(|e| DefError::Cli(e.to_string()))?
        else {
            return Err(DefError::Cli(
                "expected an object of name to definition".into(),
            ));
        };
        for (name, def) in entries {
            let Value::Object(mut fields) = def else {
                return Err(DefError::Cli(format!("{name}: expected an object")));
            };
            fields.insert("name".into(), Value::String(name.clone()));
            let prompt = match fields.remove("prompt") {
                Some(Value::String(p)) => p,
                None => String::new(),
                Some(_) => return Err(DefError::Cli(format!("{name}: prompt must be a string"))),
            };
            let source = DefSource {
                path: None,
                scope: Scope::Cli,
            };
            let (def, w) = from_fields(fields, &prompt, source)
                .map_err(|e| DefError::Cli(format!("{name}: {e}")))?;
            warnings.extend(w.into_iter().map(|w| format!("--agents {name}: {w}")));
            found.push(def);
        }
    }

    for (scope, dir) in scope_dirs(cwd, user_dir, extra_dirs) {
        let defs = load_dir(&dir, &scope, &mut warnings);
        if !matches!(scope, Scope::Extra(_)) {
            found.extend(defs);
            continue;
        }
        let trusted = trusted_dirs.iter().any(|t| {
            t == &dir
                || matches!((fs::canonicalize(t), fs::canonicalize(&dir)), (Ok(a), Ok(b)) if a == b)
        });
        for mut def in defs {
            if !trusted && !def.hooks.is_empty() {
                def.hooks.clear();
                warnings.push(format!(
                    "{}: hooks ignored, {} is not in agents.trusted_dirs",
                    def.name,
                    dir.display()
                ));
            }
            // A file nobody reviewed does not get to switch permission prompts off.
            if def.permission_mode != PermissionMode::Default {
                def.permission_mode = PermissionMode::Default;
                warnings.push(format!(
                    "{}: permission_mode reset to default, {} is an extra directory",
                    def.name,
                    dir.display()
                ));
            }
            found.push(def);
        }
    }
    for text in BUILTINS {
        let source = DefSource {
            path: None,
            scope: Scope::Builtin,
        };
        found.push(
            parse(text, source)
                .expect("built-in definitions parse (see tests/agent_def.rs)")
                .0,
        );
    }

    let mut agents: IndexMap<AgentName, AgentDef> = IndexMap::new();
    for def in found {
        match agents.get(&def.name) {
            // Two files in one scope with one name: the first by file name wins, and it is
            // worth saying so. A definition shadowed by a higher scope is the normal case.
            Some(first) if first.source.scope == def.source.scope => warnings.push(format!(
                "{}: duplicate name in {:?} scope, keeping {}",
                def.name,
                def.source.scope,
                first
                    .source
                    .path
                    .as_deref()
                    .map_or("the first".into(), |p| p.display().to_string()),
            )),
            Some(_) => {}
            None => {
                agents.insert(def.name.clone(), def);
            }
        }
    }
    let roster = roster::text(agents.values());
    warnings.extend(roster::warning(roster::estimate_tokens(&roster)));
    Ok(Discovered {
        agents,
        warnings,
        roster,
    })
}

/// The directories definitions are read from, project first (01 section 1.2). The project one
/// is left out when there is no project root.
pub fn scope_dirs(
    cwd: &Path,
    user_dir: Option<&Path>,
    extra_dirs: &[PathBuf],
) -> Vec<(Scope, PathBuf)> {
    let project = project_root(cwd).map(|r| (Scope::Project, r.join(".aibus/agents")));
    let user = user_dir.map(|d| (Scope::User, d.to_owned()));
    let extra = extra_dirs
        .iter()
        .map(|d| (Scope::Extra(d.clone()), d.clone()));
    project.into_iter().chain(user).chain(extra).collect()
}

/// The nearest ancestor of `cwd` (itself included) with `.aibus/` or `.git/`.
pub fn project_root(cwd: &Path) -> Option<&Path> {
    cwd.ancestors()
        .find(|d| d.join(".aibus").is_dir() || d.join(".git").exists())
}

/// The `*.md` files in `dir` in file name order, at most `MAX_FILES_PER_SCOPE`; the flag says
/// there were more. A missing directory is an empty scope.
pub fn list_md(dir: &Path) -> (Vec<PathBuf>, bool) {
    let Ok(read) = fs::read_dir(dir) else {
        return (Vec::new(), false);
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .collect();
    files.sort();
    let more = files.len() > MAX_FILES_PER_SCOPE;
    files.truncate(MAX_FILES_PER_SCOPE);
    (files, more)
}

/// Every definition in `dir`, up to the file cap, in file name order.
fn load_dir(dir: &Path, scope: &Scope, warnings: &mut Vec<String>) -> Vec<AgentDef> {
    let (files, more) = list_md(dir);
    if more {
        warnings.push(format!(
            "{}: more than {MAX_FILES_PER_SCOPE} definition files, reading the first {MAX_FILES_PER_SCOPE}",
            dir.display()
        ));
    }
    let mut defs = Vec::new();
    for path in files {
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        let source = DefSource {
            path: Some(path.clone()),
            scope: scope.clone(),
        };
        match parse(&text, source) {
            Ok((def, w)) => {
                warnings.extend(w.into_iter().map(|w| format!("{}: {w}", path.display())));
                defs.push(def);
            }
            Err(e) => warnings.push(format!("{}: {e}", path.display())),
        }
    }
    defs
}
