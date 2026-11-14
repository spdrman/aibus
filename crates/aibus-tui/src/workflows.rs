//! The file side of `/workflow list`, `show` and `save`. The reducer cannot read a directory, so a
//! `Command::WorkflowFile` reaches the terminal loop, which calls `apply` and steps the messages
//! that come back (`Msg::WorkflowsLoaded`, and the modal to open). Nothing here draws.

use std::path::PathBuf;
use std::time::SystemTime;

use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::state::{AGENTS_DEFS, WORKFLOW_DRAFT};
use aibus_core::workflow::{self, SaveError};
use aibus_core::{AgentName, App, Command, Msg, Redactor, Screen, WorkflowFile, WorkflowShown};
use aibus_graph::{Graph, Value};

/// Where the workflows live and what may be shown of them.
pub struct WorkflowFiles {
    /// `workflow::dirs`: the project's first, then the user's.
    pub dirs: Vec<PathBuf>,
    /// The most of a file `show` puts on screen (`result_max_bytes`).
    pub cap: usize,
    pub redactor: Redactor,
}

/// The agents the session has, which is what a step may name.
fn roster<G: Graph>(app: &App<G>) -> Vec<AgentName> {
    let defs = app.graph.read_key(AGENTS_DEFS).map_or(&[][..], as_list);
    defs.iter().filter_map(|d| get(d, "name").and_then(as_str)).filter_map(|n| AgentName::new(n).ok()).collect()
}

/// The drafted TOML, if there is one.
fn draft<G: Graph>(app: &App<G>) -> Option<String> {
    match get(app.graph.read_key(WORKFLOW_DRAFT)?, "toml")? {
        Value::Handle(h) => app.store.text(h).map(|t| t.to_string()),
        v => as_str(v).map(str::to_owned),
    }
}

/// What `op` does, as messages for the loop to step. `at` stamps a saved file.
pub fn apply<G: Graph>(op: &WorkflowFile, app: &App<G>, files: &WorkflowFiles, at: SystemTime) -> Vec<Msg> {
    let loaded = |shown: Option<WorkflowShown>, said: Option<String>| Msg::WorkflowsLoaded {
        rows: workflow::rows(&files.dirs),
        shown,
        said,
    };
    let open = |screen| Msg::Command(Command::OpenScreen(screen));
    // The reducer logs `io error in <context>`, so the sentence goes in the context.
    let refused = |why: String| Msg::IoError { context: why, error: String::new() };
    let roster = roster(app);
    match op {
        WorkflowFile::List => vec![loaded(None, None), open(Screen::Workflows)],
        WorkflowFile::Show(name) => {
            let Some((_, path, _)) = workflow::list(&files.dirs).into_iter().find(|(n, ..)| n == name) else {
                return vec![refused(format!("no workflow named {name} (/workflow list)"))];
            };
            let raw = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => return vec![refused(format!("{}: {e}", path.display()))],
            };
            let (_, line) = workflow::summarise(&raw, name, &roster);
            let mut text = files.redactor.redact(&raw).into_owned();
            if text.len() > files.cap {
                let mut cut = files.cap;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                text.push_str("\n... cut here");
            }
            let shown = WorkflowShown { name: name.clone(), text, summary: line };
            vec![loaded(Some(shown), None), open(Screen::WorkflowShow)]
        }
        WorkflowFile::Save { name, user, force } => {
            let Some(text) = draft(app) else {
                return vec![refused("workflow save: there is no drafted workflow, /workflow new <goal> makes one".into())];
            };
            if files.redactor.redact(&text).as_ref() != text.as_str() {
                return vec![refused("workflow save: the draft contains something that looks like a credential, nothing was written".into())];
            }
            match workflow::save(&files.dirs, *user, name, &text, *force, &roster, at) {
                Ok(path) => vec![loaded(None, Some(format!("saved workflow {name} to {}", path.display())))],
                Err(SaveError::Invalid(problems)) => {
                    let lines: Vec<String> = problems
                        .iter()
                        .map(|p| {
                            let step = p.step.map_or(String::new(), |n| format!("step {n}: "));
                            let field = p.field.as_deref().map_or(String::new(), |f| format!("{f}: "));
                            format!("{step}{field}{}", p.message)
                        })
                        .collect();
                    let shown = WorkflowShown {
                        name: name.clone(),
                        text: lines.join("\n"),
                        summary: format!("not saved: the draft has {} error(s)", lines.len()),
                    };
                    vec![loaded(Some(shown), None), open(Screen::WorkflowShow)]
                }
                Err(e) => vec![refused(format!("workflow save: {e}"))],
            }
        }
    }
}
