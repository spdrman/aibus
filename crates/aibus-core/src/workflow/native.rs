//! Exporting a workflow's native subagents as Claude Code agent files (#283, spec section 4.2).
//!
//! The compiled invocation hands native subagents to the CLI inline (`--agents <json>`), which
//! writes nothing. This export is for a person who wants the files anyway: `preview` shows the
//! exact diff against what is on disk, and `apply` writes only with the hash of the diff that was
//! shown, so nothing is overwritten that the person did not see.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use super::compile::{content_hash, native_agents};
use super::Workflow;
use crate::agent::AgentDef;
use crate::ids::AgentName;
use crate::persist::fs::write_atomic;

/// One file the export would write: its name in the directory, what is there now and what would
/// be.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExportFile {
    pub name: String,
    pub old: Option<String>,
    pub new: String,
}

/// A previewed export: the directory, the files, the diff a person reads and its hash, which is
/// the consent `apply` takes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Export {
    pub dir: PathBuf,
    pub files: Vec<ExportFile>,
    pub diff: String,
    pub hash: String,
}

/// What is at `path` now: `None` when nothing is, the text of a plain file, and an error for
/// anything else (a symlink, a directory, a file that is not UTF-8), which is never written to.
fn current(path: &Path) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(md) if md.file_type().is_symlink() || !md.is_file() => Err(format!(
            "{} is not a plain file, refusing to write through it",
            path.display()
        )),
        Ok(_) => std::fs::read_to_string(path)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// A Claude Code agent file: YAML frontmatter (the description as a JSON string, which YAML
/// reads as the same string) and the prompt.
fn agent_file(name: &str, agent: &serde_json::Value) -> String {
    let tools: Vec<&str> = agent["tools"]
        .as_array()
        .map(|t| t.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let mut out = format!("---\nname: {name}\ndescription: {}\n", agent["description"]);
    if !tools.is_empty() {
        out.push_str(&format!("tools: {}\n", tools.join(", ")));
    }
    if let Some(m) = agent["model"].as_str() {
        out.push_str(&format!("model: {m}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(agent["prompt"].as_str().unwrap_or_default());
    out.push('\n');
    out
}

/// A line diff of `old` and `new`: ` ` for a line both have, `-` and `+` for the rest. A pair too
/// big to compare line by line is shown as all of one and all of the other.
fn line_diff(old: &str, new: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let mut out = String::new();
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        a.iter().for_each(|l| out.push_str(&format!("-{l}\n")));
        b.iter().for_each(|l| out.push_str(&format!("+{l}\n")));
        return out;
    }
    // lcs[i][j]: the longest common run of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push_str(&format!(" {}\n", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push_str(&format!("+{}\n", b[j]));
            j += 1;
        } else {
            out.push_str(&format!("-{}\n", a[i]));
            i += 1;
        }
    }
    out
}

/// What exporting `wf`'s native subagents into `dir` would change. Files that would not change
/// are left out; nothing is written.
pub fn preview(
    wf: &Workflow,
    agents: &IndexMap<AgentName, AgentDef>,
    dir: &Path,
) -> Result<Export, String> {
    if std::fs::symlink_metadata(dir).is_ok_and(|md| md.file_type().is_symlink()) {
        return Err(format!(
            "{} is a symlink, refusing to write through it",
            dir.display()
        ));
    }
    let mut files = Vec::new();
    let mut diff = String::new();
    for parent in 0..wf.steps.len() {
        for (name, agent) in native_agents(wf, agents, parent) {
            let file = format!("{name}.md");
            let new = agent_file(&name, &agent);
            let old = current(&dir.join(&file))?;
            if old.as_deref() == Some(new.as_str()) {
                continue;
            }
            let from = if old.is_some() {
                file.as_str()
            } else {
                "/dev/null"
            };
            diff.push_str(&format!("--- {from}\n+++ {file}\n"));
            diff.push_str(&line_diff(old.as_deref().unwrap_or_default(), &new));
            files.push(ExportFile {
                name: file,
                old,
                new,
            });
        }
    }
    let hash = content_hash(&format!("{}\n{diff}", dir.display()));
    Ok(Export {
        dir: dir.to_path_buf(),
        files,
        diff,
        hash,
    })
}

/// Writes the previewed files, if `consent` is the hash of the diff the person saw and the disk
/// still matches it.
pub fn apply(export: &Export, consent: &str) -> Result<Vec<PathBuf>, String> {
    if consent != export.hash {
        return Err(format!(
            "{consent} is not the hash of this export's diff; preview it again and pass the hash it prints"
        ));
    }
    for f in &export.files {
        if current(&export.dir.join(&f.name))? != f.old {
            return Err(format!(
                "{} changed since the preview; preview again, nothing was written",
                f.name
            ));
        }
    }
    if !export.files.is_empty() {
        std::fs::create_dir_all(&export.dir)
            .map_err(|e| format!("{}: {e}", export.dir.display()))?;
    }
    let mut written = Vec::new();
    for f in &export.files {
        let path = export.dir.join(&f.name);
        write_atomic(&path, &f.new).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}
