//! The workflow author's side of the binary (G6, G8): the program's own `aibus mcp` as the
//! server it talks to, and the call the lead makes to start it.
//!
//! The server is read-only unless the owner starts the author with `/workflow new --write` or
//! `/workflow edit`. Then, and only then, a second child is started with `--allow write` (never
//! `gates`, never `run`), registered under another server name, and offered to that one run.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use aibus_core::config::McpServer;
use aibus_core::mcp::{self, Client};
use serde_json::{json, Value};

use crate::launch::ChildLauncher;

/// `<this program> mcp`, with `--allow write` and nothing else when `write`. `Err` is why the
/// program cannot start itself.
pub fn server(write: bool) -> Result<McpServer, String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("this program's own path does not resolve: {e}"))?;
    let mut args = vec!["mcp".to_owned()];
    if write {
        args.extend(["--allow".to_owned(), "write".to_owned()]);
    }
    Ok(McpServer { command: exe.to_string_lossy().into_owned(), args, ..McpServer::default() })
}

/// What the lead is asked to hand the author. `served` is the server's `author_workflow` prompt
/// (it already carries the goal); without it the definition's own instructions carry the task.
/// An edit tells the author to read the file first and to pass the revision it got back.
fn task(goal: &str, write: bool, edit: Option<&str>, served: Option<String>) -> String {
    if let Some(name) = edit {
        return format!(
            "Change the saved workflow `{name}`: {goal}\n\nRead it first with workflow_get (name {name}) and keep the revision it returns. \
             Make the change, check the result with workflow_validate, and save it with workflow_update, passing that revision as \
             expected_revision. If the file changed in the meantime you get a conflict: read it again and tell the owner, do not retry \
             blindly. Leave gates out; a draft with a gate is refused."
        );
    }
    let mut text = served.unwrap_or_else(|| format!("Draft a workflow for this goal: {goal}"));
    if write {
        text.push_str(
            "\n\nThe owner has allowed you to save. When the draft validates, call workflow_create once (scope `project`), then \
             say which name you wrote. Leave gates out. Each write asks the owner first and they may refuse; if they do, say so \
             and stop.",
        );
    }
    text
}

/// What starting the author needs from the session.
pub struct Author {
    /// The read-only child the session started with, when it did.
    pub read: Option<Arc<Client>>,
    write: tokio::sync::Mutex<Option<Arc<Client>>>,
    launcher: Arc<ChildLauncher>,
    env: Vec<(String, String)>,
    cwd: PathBuf,
    session: String,
}

impl Author {
    pub fn new(read: Option<Arc<Client>>, launcher: Arc<ChildLauncher>, cwd: PathBuf, session: String) -> Self {
        Self { read, write: tokio::sync::Mutex::new(None), launcher, env: std::env::vars().collect(), cwd, session }
    }

    /// The write child, started the first time it is asked for, with its two write tools added
    /// to the running executor under `mcp:aibus-write:*`.
    async fn writer(&self) -> Result<Arc<Client>, String> {
        let mut slot = self.write.lock().await;
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let (client, listed) =
            Client::connect(mcp::WRITE_SERVER, &server(true)?, &self.env, &self.cwd, &self.session).await?;
        for tool in mcp::tools_of(mcp::WRITE_SERVER, &client, listed) {
            self.launcher.tools.register_late(tool);
        }
        *slot = Some(client.clone());
        Ok(client)
    }

    /// The `delegate` call for the lead to make. It also says, for the next author run only,
    /// whether that run may use the write child: every command sets it, so a plain
    /// `/workflow new` after a write one clears it.
    pub async fn delegate(&self, goal: &str, write: bool, edit: Option<&str>) -> Result<Value, String> {
        let client = if write { Some(self.writer().await?) } else { self.read.clone() };
        let served = match &client {
            Some(c) => c.prompt("author_workflow", json!({"goal": goal})).await.ok(),
            None => None,
        };
        self.launcher.author_write.store(write, Ordering::SeqCst);
        Ok(json!({"agent": "workflow-author", "prompt": task(goal, write, edit, served), "background": false}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_flag_adds_allow_write_and_nothing_else() {
        assert_eq!(server(false).unwrap().args, ["mcp"]);
        assert_eq!(server(true).unwrap().args, ["mcp", "--allow", "write"]);
        assert!(server(true).unwrap().env.is_empty() && server(true).unwrap().pass_env.is_empty());
    }

    #[test]
    fn no_way_to_start_the_child_gets_gates_or_run() {
        for write in [false, true] {
            let args = server(write).unwrap().args;
            for banned in ["gates", "run", "--allow-tool", "--no-provenance", "--http"] {
                assert!(!args.iter().any(|a| a == banned), "{write}: {args:?}");
            }
            assert_eq!(args.iter().filter(|a| *a == "--allow").count(), usize::from(write));
        }
    }

    #[test]
    fn edit_reads_then_updates_with_the_revision() {
        let t = task("add a test step", true, Some("review"), Some("the server's prompt".into()));
        assert!(t.contains("workflow_get (name review)"), "{t}");
        assert!(t.contains("expected_revision"), "{t}");
        assert!(t.contains("conflict"), "{t}");
        // The edit's own instructions are the task, not the draft-from-scratch prompt.
        assert!(!t.contains("the server's prompt"));
    }

    #[test]
    fn a_new_draft_is_told_it_may_save_only_with_the_flag() {
        let read = task("review a PR", false, None, Some("SERVED".into()));
        assert_eq!(read, "SERVED");
        assert_eq!(task("review a PR", false, None, None), "Draft a workflow for this goal: review a PR");
        let write = task("review a PR", true, None, Some("SERVED".into()));
        assert!(write.starts_with("SERVED") && write.contains("workflow_create once"), "{write}");
    }
}
