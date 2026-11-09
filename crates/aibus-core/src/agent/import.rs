//! `aibus agent import <dir>` (09 section 6, Q1): copies definitions from another tool's directory
//! (`.claude/agents`) into `.aibus/agents/`, and turns every hook in them off. The `hooks` key is
//! renamed `disabled_hooks`, which aibus ignores, and each hook is reported with its command text
//! so the user can look at it and put the key back on purpose.
//!
//! Nothing in `.claude/agents` is read by aibus itself unless `agents.extra_dirs` names it; this
//! is the one step that looks, and only because the user asked for it.

use std::path::{Path, PathBuf};

use super::def::parse;
use super::discover::list_md;
use super::{DefSource, HookKind, Scope};

/// One hook the import found, now off.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FoundHook {
    pub event: String,
    pub kind: HookKind,
    pub matcher: Option<String>,
    /// The command line, the prompt or the URL.
    pub text: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Written to `.0`.
    Copied(PathBuf),
    /// Not written, and why (it did not parse, or the name is already taken).
    Skipped(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Imported {
    pub file: PathBuf,
    pub outcome: Outcome,
    pub hooks: Vec<FoundHook>,
}

/// Renames the top-level `hooks:` key of the frontmatter to `disabled_hooks:`, and leaves the
/// rest of the file byte for byte as it was.
fn disable_hooks(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut fences = 0;
    let mut out = String::with_capacity(text.len() + 9);
    for line in text.split_inclusive('\n') {
        if line.trim_end() == "---" {
            fences += 1;
        }
        // Between the first fence and the second is the frontmatter.
        match line.strip_prefix("hooks:").filter(|_| fences == 1) {
            Some(rest) => {
                out.push_str("disabled_hooks:");
                out.push_str(rest);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Imports every `*.md` in `from` into `to` (created if needed). A file that does not parse, or
/// whose name `to` already has, is skipped and said so; nothing is overwritten.
pub fn import(from: &Path, to: &Path) -> Result<Vec<Imported>, String> {
    if !from.is_dir() {
        return Err(format!("{} is not a directory", from.display()));
    }
    let (files, _) = list_md(from);
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let source = DefSource {
            path: Some(file.clone()),
            scope: Scope::Extra(from.to_path_buf()),
        };
        let def = match parse(&text, source) {
            Ok((def, _)) => def,
            Err(e) => {
                out.push(Imported {
                    file,
                    outcome: Outcome::Skipped(e.to_string()),
                    hooks: Vec::new(),
                });
                continue;
            }
        };
        let hooks = def
            .hooks
            .iter()
            .flat_map(|(event, specs)| {
                specs.iter().map(|h| FoundHook {
                    event: event.to_string(),
                    kind: h.kind,
                    matcher: h.matcher.clone(),
                    text: h.command.clone(),
                })
            })
            .collect();
        let dest = to.join(format!("{}.md", def.name));
        let outcome = if dest.exists() {
            Outcome::Skipped(format!("{} already exists", dest.display()))
        } else {
            // The copy has to parse with no hooks left, or nothing is written.
            let copy = disable_hooks(&text);
            let source = DefSource { path: None, scope: Scope::Project };
            if parse(&copy, source).is_ok_and(|(d, _)| d.hooks.is_empty()) {
                std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
                std::fs::write(&dest, copy).map_err(|e| format!("{}: {e}", dest.display()))?;
                Outcome::Copied(dest)
            } else {
                Outcome::Skipped("its hooks could not be turned off".into())
            }
        };
        out.push(Imported { file, outcome, hooks });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_frontmatter_key_is_renamed() {
        let text = "---\nname: a\nhooks:\n  pre_tool_use:\n    - command: x\n---\nhooks: not a key here\n";
        assert_eq!(
            disable_hooks(text),
            "---\nname: a\ndisabled_hooks:\n  pre_tool_use:\n    - command: x\n---\nhooks: not a key here\n"
        );
        assert_eq!(disable_hooks("---\nname: a\n---\nbody"), "---\nname: a\n---\nbody");
    }
}
