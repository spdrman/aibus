//! B2.2 from the command line: resuming a run from its saved conversation, and a worker that
//! works in its own worktree.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aibus_testkit::xdg_temp;
use common::{command, ndjson, stderr};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// `home/proj` as a repository with one commit, and a `scribe` agent that works in a worktree.
fn project(home: &Path) -> PathBuf {
    let proj = home.join("proj");
    let agents = proj.join(".aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("scribe.md"),
        "---\nname: scribe\ndescription: Writes files.\nisolation: worktree\ntools: [write]\npermission_mode: accept_edits\n---\nWrite what you are asked to.\n",
    )
    .unwrap();
    fs::write(proj.join("a.txt"), "one\n").unwrap();
    git(&proj, &["init", "-q", "-b", "main"]);
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "first"]);
    proj
}

/// Every directory under `<cache>/worktrees/<session>/`.
fn kept(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".cache/aibus/worktrees");
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|s| fs::read_dir(s.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
        .collect()
}

fn delegate_to_scribe(home: &Path, script: &str) -> std::process::Output {
    let file = home.join("s.ron");
    fs::write(&file, script).unwrap();
    command(home)
        .env("AIBUS_I_KNOW", "1")
        .args(["run", "--json", "--permission-mode", "bypass", "--script"])
        .arg(&file)
        .arg("@agent-scribe write made.txt")
        .output()
        .unwrap()
}

#[test]
fn a_worker_writes_in_its_worktree_and_the_project_tree_is_unchanged() {
    let home = xdg_temp();
    let proj = project(home.path());
    let out = delegate_to_scribe(
        home.path(),
        r#"[ToolCall(name: "write", input: {"path": "made.txt", "content": "hi\n"}), Stop(ToolUse), Text("wrote it"), Stop(EndTurn), Text("all done")]"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));

    assert!(
        !proj.join("made.txt").exists(),
        "the project tree must not see the file"
    );
    let status = Command::new("git")
        .arg("-C")
        .arg(&proj)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty());
    // The file is there, in a worktree that was kept because it has changes.
    let trees = kept(home.path());
    assert_eq!(trees.len(), 1, "{trees:?}");
    assert_eq!(
        fs::read_to_string(trees[0].join("made.txt")).unwrap(),
        "hi\n"
    );
    assert!(stderr(&out).contains("kept worktree"), "{}", stderr(&out));

    // `doctor` lists it.
    let doc = command(home.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&doc.stdout).contains(&*trees[0].to_string_lossy()));
}

#[test]
fn a_clean_worktree_is_removed_when_the_run_ends() {
    let home = xdg_temp();
    let proj = project(home.path());
    let out = delegate_to_scribe(
        home.path(),
        r#"[Text("nothing to write"), Stop(EndTurn), Text("all done")]"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(kept(home.path()).is_empty());
    // And git has forgotten it too.
    let list = Command::new("git")
        .arg("-C")
        .arg(&proj)
        .args(["worktree", "list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 1);
}

#[test]
fn outside_a_repository_the_isolated_run_fails_cleanly() {
    let home = xdg_temp();
    let proj = project(home.path());
    fs::remove_dir_all(proj.join(".git")).unwrap();
    let out = delegate_to_scribe(
        home.path(),
        r#"[Text("never runs"), Text("lead carries on")]"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(shown.contains("not inside a git repository"), "{shown}");
    assert!(kept(home.path()).is_empty());
}

#[test]
fn session_resume_continues_a_run_with_its_history() {
    let home = xdg_temp();
    let first = home.path().join("one.ron");
    fs::write(&first, r#"[Text("first answer")]"#).unwrap();
    let out = command(home.path())
        .args(["run", "--json", "--script"])
        .arg(&first)
        .arg("hello")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);
    let run = lines.iter().find(|l| l["type"] == "RunStarted").unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let session = lines.last().unwrap()["session"]
        .as_str()
        .unwrap()
        .to_owned();

    let second = home.path().join("two.ron");
    fs::write(
        &second,
        r#"[MatchPrompt(contains: "first answer", then: [Text("resumed ok"), Stop(EndTurn)]), Text("history lost")]"#,
    )
    .unwrap();
    let out = command(home.path())
        .args([
            "session", "resume", &session, "--run", &run, "--json", "--script",
        ])
        .arg(&second)
        .arg("and again")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);
    let done = lines.iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["summary"], "resumed ok");
    assert_eq!(done["run"], run.as_str());
    assert_eq!(lines.last().unwrap()["session"], session.as_str());

    // The saved conversation holds both rounds.
    let saved = home.path().join(format!(
        ".local/state/aibus/sessions/{session}/runs/{run}/conversation.jsonl"
    ));
    let text = fs::read_to_string(saved).unwrap();
    assert!(text.contains("hello") && text.contains("first answer") && text.contains("and again"));
    assert!(text.contains("\"kind\":\"resume\""));
}

#[test]
fn session_resume_needs_a_message_and_a_saved_run() {
    let home = xdg_temp();
    let out = command(home.path())
        .args([
            "session",
            "resume",
            "01JC0000000000000000000000",
            "--run",
            "x",
            "hi",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}
