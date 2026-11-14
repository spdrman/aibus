//! Exporting a workflow's native subagents as Claude Code agent files (#283, spec section 4.2).
//!
//! The compiled invocation hands native subagents to the CLI inline (`--agents <json>`), which
//! writes nothing. This export is for a person who wants the files anyway: `preview` shows the
//! exact diff against what is on disk, and `apply` writes only with the hash of the diff that was
//! shown, so nothing is overwritten that the person did not see.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use super::Workflow;
use crate::agent::AgentDef;
use crate::ids::AgentName;

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

/// What exporting `wf`'s native subagents into `dir` would change.
pub fn preview(
    wf: &Workflow,
    agents: &IndexMap<AgentName, AgentDef>,
    dir: &Path,
) -> Result<Export, String> {
    let _ = (wf, agents, dir);
    unimplemented!("#283")
}

/// Writes the previewed files, if `consent` is the hash of the diff the person saw and the disk
/// still matches it.
pub fn apply(export: &Export, consent: &str) -> Result<Vec<PathBuf>, String> {
    let _ = (export, consent);
    unimplemented!("#283")
}
