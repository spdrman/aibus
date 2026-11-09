//! C1.5: `agent import` copies definitions with their hooks off (09 section 6), and
//! `.claude/agents` is only ever read when `agents.extra_dirs` names it (Q1).

use std::fs;
use std::path::Path;

use aibus_core::agent::discover::discover;
use aibus_core::agent::import::{import, Outcome};
use aibus_core::{AgentName, HookKind};
use aibus_testkit::TempDir;

const REVIEWER: &str = "---
name: reviewer
description: Reviews a diff.
tools: [read, grep]
hooks:
  pre_tool_use:
    - matcher: bash
      command: ./scripts/audit.sh --strict
  stop:
    - type: http
      url: https://hooks.example/stop
---
You review diffs.
";

const PLAIN: &str = "---\nname: scout\ndescription: Finds things.\n---\nLook around.\n";

fn write(dir: &Path, name: &str, text: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(name), text).unwrap();
}

#[test]
fn definitions_are_copied_and_every_hook_is_reported_and_switched_off() {
    let project = TempDir::new().unwrap();
    let from = project.path().join(".claude/agents");
    let to = project.path().join(".aibus/agents");
    write(&from, "reviewer.md", REVIEWER);
    write(&from, "scout.md", PLAIN);
    write(&from, "notes.txt", "not a definition");

    let done = import(&from, &to).unwrap();
    assert_eq!(done.len(), 2);
    let reviewer = done.iter().find(|i| i.file.ends_with("reviewer.md")).unwrap();
    assert_eq!(reviewer.outcome, Outcome::Copied(to.join("reviewer.md")));

    // Every hook is listed with the text that would run.
    let mut found: Vec<_> = reviewer.hooks.iter().map(|h| (h.event.as_str(), h.kind, h.matcher.as_deref(), h.text.as_str())).collect();
    found.sort_by_key(|h| h.0);
    assert_eq!(
        found,
        [
            ("pre_tool_use", HookKind::Command, Some("bash"), "./scripts/audit.sh --strict"),
            ("stop", HookKind::Http, None, "https://hooks.example/stop"),
        ]
    );

    // The copy keeps the hooks as text under a key aibus ignores, and the rest as it was.
    let copy = fs::read_to_string(to.join("reviewer.md")).unwrap();
    assert_eq!(copy, REVIEWER.replacen("hooks:", "disabled_hooks:", 1));
    assert_eq!(fs::read_to_string(to.join("scout.md")).unwrap(), PLAIN);

    // Loaded the usual way it has no hooks and no warning about the parked key.
    let found = discover(None, project.path(), None, &[], &[]).unwrap();
    let def = &found.agents[&"reviewer".parse::<AgentName>().unwrap()];
    assert!(def.hooks.is_empty());
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    assert_eq!(def.system_prompt, "You review diffs.");
}

#[test]
fn nothing_is_overwritten_and_a_broken_file_is_skipped_with_its_reason() {
    let project = TempDir::new().unwrap();
    let from = project.path().join("theirs");
    let to = project.path().join(".aibus/agents");
    write(&from, "scout.md", PLAIN);
    write(&from, "broken.md", "no frontmatter here");
    write(&to, "scout.md", "---\nname: scout\ndescription: Mine.\n---\nmine\n");

    let done = import(&from, &to).unwrap();
    let outcome = |name: &str| done.iter().find(|i| i.file.ends_with(name)).unwrap().outcome.clone();
    assert!(matches!(outcome("scout.md"), Outcome::Skipped(why) if why.contains("already exists")));
    assert!(matches!(outcome("broken.md"), Outcome::Skipped(why) if why.contains("no frontmatter")));
    assert!(fs::read_to_string(to.join("scout.md")).unwrap().contains("Mine."));
    assert!(!to.join("broken.md").exists());

    assert!(import(&project.path().join("missing"), &to).unwrap_err().contains("is not a directory"));
}

#[test]
fn a_bom_or_a_quoted_key_cannot_keep_a_hook_switched_on() {
    let project = TempDir::new().unwrap();
    let from = project.path().join("theirs");
    let to = project.path().join(".aibus/agents");
    write(&from, "bom.md", &format!("\u{feff}{REVIEWER}").replace("reviewer", "bom"));
    write(&from, "quoted.md", &REVIEWER.replace("reviewer", "quoted").replace("hooks:", "\"hooks\":"));
    let done = import(&from, &to).unwrap();
    for i in &done {
        match &i.outcome {
            // Either it was copied with the hooks off, or it was not copied at all.
            Outcome::Copied(p) => {
                let (def, _) = aibus_core::agent::def::parse(
                    &fs::read_to_string(p).unwrap(),
                    aibus_core::DefSource { path: None, scope: aibus_core::Scope::Project },
                )
                .unwrap();
                assert!(def.hooks.is_empty(), "{}", p.display());
            }
            Outcome::Skipped(_) => assert!(!to.join(i.file.file_name().unwrap()).exists()),
        }
    }
}

/// Q1: aibus does not look in `.claude/agents` by itself. Only `agents.extra_dirs` brings it in,
/// and then the hooks of those definitions are stripped unless the directory is trusted.
#[test]
fn claude_agents_are_not_read_unless_extra_dirs_names_them() {
    let project = TempDir::new().unwrap();
    write(&project.path().join(".claude/agents"), "reviewer.md", REVIEWER);

    let without = discover(None, project.path(), None, &[], &[]).unwrap();
    assert!(!without.agents.keys().any(|k| k.as_str() == "reviewer"));

    let extra = [project.path().join(".claude/agents")];
    let with = discover(None, project.path(), None, &extra, &[]).unwrap();
    let def = &with.agents[&"reviewer".parse::<AgentName>().unwrap()];
    assert!(def.hooks.is_empty(), "an untrusted directory's hooks are stripped");

    let trusted = discover(None, project.path(), None, &extra, &extra).unwrap();
    assert_eq!(trusted.agents[&"reviewer".parse::<AgentName>().unwrap()].hooks.len(), 2);
}
