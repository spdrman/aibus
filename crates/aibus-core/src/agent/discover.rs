//! Finding definitions across the five scopes (01 section 1.2). The first match by name wins.

use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::Value;

use super::builtin::BUILTINS;
use super::def::{from_fields, parse, DefError};
use super::{AgentDef, DefSource, Scope};
use crate::ids::AgentName;

#[derive(Debug, Default)]
pub struct Discovered {
    /// In precedence order: `--agents`, project, user, extra dirs, built-ins.
    pub agents: IndexMap<AgentName, AgentDef>,
    pub warnings: Vec<String>,
}

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

    // `<project>` is the nearest ancestor of the cwd with `.aibus/` or `.git/`.
    if let Some(root) = project_root(cwd) {
        found.extend(load_dir(
            &root.join(".aibus/agents"),
            &Scope::Project,
            &mut warnings,
        ));
    }
    if let Some(dir) = user_dir {
        found.extend(load_dir(dir, &Scope::User, &mut warnings));
    }
    for dir in extra_dirs {
        let trusted = trusted_dirs.iter().any(|t| {
            t == dir
                || matches!((fs::canonicalize(t), fs::canonicalize(dir)), (Ok(a), Ok(b)) if a == b)
        });
        for mut def in load_dir(dir, &Scope::Extra(dir.clone()), &mut warnings) {
            if !trusted && !def.hooks.is_empty() {
                def.hooks.clear();
                warnings.push(format!(
                    "{}: hooks ignored, {} is not in agents.trusted_dirs",
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
    Ok(Discovered { agents, warnings })
}

/// The nearest ancestor of `cwd` (itself included) with `.aibus/` or `.git/`.
pub fn project_root(cwd: &Path) -> Option<&Path> {
    cwd.ancestors()
        .find(|d| d.join(".aibus").is_dir() || d.join(".git").exists())
}

/// Every `*.md` in `dir`, in file name order. A missing directory is an empty scope.
fn load_dir(dir: &Path, scope: &Scope, warnings: &mut Vec<String>) -> Vec<AgentDef> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .collect();
    files.sort();
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
