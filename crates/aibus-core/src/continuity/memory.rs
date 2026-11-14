//! Per-agent memory directories (01 section 8.3): where each scope keeps its files, how a tool
//! path is kept inside one, and the `.gitignore` line for the local scope.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::agent::{AgentDef, MemoryScope};

const LOCAL: &str = ".aibus/agent-memory-local/";

/// Where `agent`'s memory lives in `scope`: `<data>/agent-memory/<agent>` for the user,
/// `<project>/.aibus/agent-memory/<agent>` and `<project>/.aibus/agent-memory-local/<agent>`
/// for the project's two.
pub fn scope_dir(scope: MemoryScope, agent: &str, project: &Path, data: &Path) -> PathBuf {
    match scope {
        MemoryScope::User => data.join("agent-memory").join(agent),
        MemoryScope::Project => project.join(".aibus/agent-memory").join(agent),
        MemoryScope::Local => project.join(".aibus/agent-memory-local").join(agent),
    }
}

/// The directory a definition's `memory` names, or `None` without one (or, for the user scope,
/// without a data dir).
pub fn memory_dir(def: &AgentDef, project: &Path, data: Option<&Path>) -> Option<PathBuf> {
    let scope = def.memory?;
    let data = data.unwrap_or(Path::new(""));
    if scope == MemoryScope::User && data.as_os_str().is_empty() {
        return None;
    }
    Some(scope_dir(scope, def.name.as_str(), project, data))
}

/// Keeps `.aibus/agent-memory-local/` out of version control (07 section 1): adds the line to
/// the project's `.gitignore` unless it is there. There is no `aibus init` command yet, so this
/// runs whenever the local scope gets written to.
pub fn ensure_local_ignored(project: &Path) -> io::Result<()> {
    let file = project.join(".gitignore");
    let text = match fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let listed = text
        .lines()
        .map(str::trim)
        .any(|l| l.trim_start_matches('/') == LOCAL || l.trim_start_matches('/') == LOCAL.trim_end_matches('/'));
    if listed {
        return Ok(());
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    fs::write(&file, format!("{text}{sep}{LOCAL}\n"))
}

/// The project a local-scope memory directory belongs to, for `ensure_local_ignored`.
pub fn local_project(dir: &Path) -> Option<&Path> {
    let scope = dir.parent()?;
    (scope.file_name()? == "agent-memory-local").then_some(())?;
    scope.parent()?.parent()
}

/// `rel` inside `dir`, for the `memory_*` tools. Absolute paths and `..` are refused, and so is
/// anything that a symlink would carry out of the directory, whether or not the file exists
/// yet. `dir` has to exist.
pub fn confine(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut clean = PathBuf::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(n) => clean.push(n),
            Component::CurDir => {}
            _ => return Err(format!("{rel}: the path has to stay inside the memory directory")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("the path is empty".into());
    }
    let root = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let joined = root.join(&clean);
    // The deepest part that exists decides where the path really lands.
    let existing = joined
        .ancestors()
        .find(|a| a.symlink_metadata().is_ok())
        .unwrap_or(&root);
    let real = existing
        .canonicalize()
        .map_err(|e| format!("{rel}: {e}"))?;
    if real.starts_with(&root) {
        Ok(joined)
    } else {
        Err(format!("{rel}: the path leaves the memory directory"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_scopes_live_where_the_spec_says() {
        let (p, d) = (Path::new("/p"), Path::new("/d"));
        assert_eq!(scope_dir(MemoryScope::User, "lead", p, d), Path::new("/d/agent-memory/lead"));
        assert_eq!(scope_dir(MemoryScope::Project, "lead", p, d), Path::new("/p/.aibus/agent-memory/lead"));
        assert_eq!(
            scope_dir(MemoryScope::Local, "lead", p, d),
            Path::new("/p/.aibus/agent-memory-local/lead")
        );
        assert_eq!(
            local_project(&scope_dir(MemoryScope::Local, "lead", p, d)),
            Some(Path::new("/p"))
        );
        assert_eq!(local_project(&scope_dir(MemoryScope::Project, "lead", p, d)), None);
    }

    #[test]
    fn the_local_scope_is_gitignored_once() {
        let dir = tempfile::tempdir().unwrap();
        ensure_local_ignored(dir.path()).unwrap();
        ensure_local_ignored(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(text, ".aibus/agent-memory-local/\n");
        fs::write(dir.path().join(".gitignore"), "target").unwrap();
        ensure_local_ignored(dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "target\n.aibus/agent-memory-local/\n"
        );
        fs::write(dir.path().join(".gitignore"), "/.aibus/agent-memory-local\n").unwrap();
        ensure_local_ignored(dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "/.aibus/agent-memory-local\n"
        );
    }

    #[test]
    fn paths_stay_inside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mem = dir.path().join("mem");
        fs::create_dir_all(&mem).unwrap();
        let root = mem.canonicalize().unwrap();
        assert_eq!(confine(&mem, "notes/a.md").unwrap(), root.join("notes/a.md"));
        assert_eq!(confine(&mem, "./a.md").unwrap(), root.join("a.md"));
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/.."] {
            assert!(confine(&mem, bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_carry_a_path_out() {
        let dir = tempfile::tempdir().unwrap();
        let (mem, outside) = (dir.path().join("mem"), dir.path().join("outside"));
        fs::create_dir_all(&mem).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, mem.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), mem.join("file")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), mem.join("dangling")).unwrap();
        for bad in ["link/secret", "link/new.md", "file", "dangling"] {
            assert!(confine(&mem, bad).is_err(), "{bad}");
        }
    }
}
