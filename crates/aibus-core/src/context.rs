//! What a worker starts with (01 section 5.1). It is built from scratch for each run: the
//! definition, the project and the task, and nothing of any parent's conversation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::agent::discover::project_root;
use crate::agent::{AgentDef, MemoryScope};
use crate::run::{ContentBlock, Conversation, Message, MessageRole};

/// `git status --short` and the branch, as they were when the run started.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Git {
    pub branch: String,
    pub status: String,
}

pub struct ContextInput<'a> {
    pub def: &'a AgentDef,
    /// The task message: the `prompt` the parent passed.
    pub prompt: &'a str,
    pub cwd: &'a Path,
    pub platform: &'a str,
    /// `YYYY-MM-DD`; the caller reads the clock so a test can pin it.
    pub date: &'a str,
    pub git: Option<Git>,
    /// The aibus data dir (`~/.local/share/aibus`), where `user` scope memory lives.
    pub data_home: Option<&'a Path>,
}

/// The branch and short status of the repository at `cwd`; `None` outside one or without `git`.
pub fn gather_git(cwd: &Path) -> Option<Git> {
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
    };
    Some(Git {
        branch: git(&["rev-parse", "--abbrev-ref", "HEAD"])?,
        status: git(&["status", "--short"])?,
    })
}

/// Builds the run's conversation. The system prompt is the definition body, an environment
/// block and the preloaded skills. The single user message carries, in this order and when they
/// apply, the project rules, the git status, the memory index and the task. `explore` and
/// `plan` get no rules and no git status.
pub fn build(input: &ContextInput<'_>) -> Conversation {
    let def = input.def;
    let root = project_root(input.cwd).unwrap_or(input.cwd);
    let lean = matches!(def.name.as_str(), "explore" | "plan");

    // `AIBUS.md`, `.aibus/rules/*.md`, then `CLAUDE.md` and `.claude/rules/*.md` for
    // compatibility; each under its path relative to the project.
    fn project_rules(root: &Path) -> Option<String> {
        let mut files: Vec<PathBuf> = vec![root.join("AIBUS.md")];
        files.extend(markdown_in(&root.join(".aibus/rules")));
        files.push(root.join("CLAUDE.md"));
        files.extend(markdown_in(&root.join(".claude/rules")));
        let parts: Vec<String> = files
            .iter()
            .filter_map(|f| {
                let body = fs::read_to_string(f).ok()?;
                let rel = f.strip_prefix(root).unwrap_or(f);
                Some(format!("### {}\n{}", rel.display(), body.trim_end()))
            })
            .collect();
        (!parts.is_empty()).then(|| format!("Project rules:\n\n{}", parts.join("\n\n")))
    }

    fn memory_block(dir: &Path) -> Option<String> {
        let index = fs::read_to_string(dir.join("MEMORY.md")).ok();
        let others: Vec<String> = fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n != "MEMORY.md")
            .collect();
        if index.is_none() && others.is_empty() {
            return None;
        }
        let mut others = others;
        others.sort();
        let mut out = format!("Memory directory {}:\n", dir.display());
        if let Some(mut text) = index {
            // The first 2,000 tokens, at the 4 bytes per token heuristic.
            let mut end = text.len().min(8_000);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            out.push_str(&format!("\nMEMORY.md:\n{}\n", text.trim_end()));
        }
        if !others.is_empty() {
            out.push_str(&format!("\nOther files: {}\n", others.join(", ")));
        }
        Some(out)
    }


    let mut system = format!(
        "{}\n\n<environment>\ncwd: {}\nplatform: {}\ndate: {}\n",
        def.system_prompt.trim_end(),
        input.cwd.display(),
        input.platform,
        input.date
    );
    if let Some(git) = &input.git {
        system.push_str(&format!("git branch: {}\n", git.branch));
    }
    system.push_str("</environment>\n");

    let mut seen: Vec<&str> = Vec::new();
    for name in &def.skills {
        let safe = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !name.starts_with('.');
        if !safe || seen.contains(&name.as_str()) || seen.len() == 32 {
            continue;
        }
        seen.push(name);
        let file = root.join(".aibus/skills").join(name).join("SKILL.md");
        if let Ok(body) = fs::read_to_string(file) {
            system.push_str(&format!("\n## Skill: {name}\n{}\n", body.trim_end()));
        }
    }

    let mut blocks = Vec::new();
    if !lean && !def.omit_project_rules {
        if let Some(rules) = project_rules(root) {
            blocks.push(rules);
        }
    }
    if let (false, Some(git)) = (lean, &input.git) {
        blocks.push(format!(
            "Git status at the start of the run (branch {}):\n{}",
            git.branch,
            if git.status.is_empty() {
                "(clean)"
            } else {
                &git.status
            }
        ));
    }
    if let Some(memory) = def.memory.and_then(|scope| {
        let dir = match scope {
            MemoryScope::User => input
                .data_home?
                .join("agent-memory")
                .join(def.name.as_str()),
            MemoryScope::Project => root.join(".aibus/agent-memory").join(def.name.as_str()),
            MemoryScope::Local => root
                .join(".aibus/agent-memory-local")
                .join(def.name.as_str()),
        };
        memory_block(&dir)
    }) {
        blocks.push(memory);
    }
    blocks.push(input.prompt.to_owned());

    Conversation {
        system,
        messages: vec![Message {
            role: MessageRole::User,
            blocks: blocks.into_iter().map(ContentBlock::Text).collect(),
            at: SystemTime::UNIX_EPOCH,
        }],
        ..Conversation::default()
    }
}

/// The `*.md` files directly in `dir`, by name.
fn markdown_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .collect();
    files.sort();
    files
}
