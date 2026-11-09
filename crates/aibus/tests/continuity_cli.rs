//! C1.4 from the command line: the ledger, handoff and memory commands, and the continuity loop
//! of a session end to end on the scripted fake (the M5 exit).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::schema::assert_valid;
use common::{aibus, command, ndjson, stderr};
use serde_json::{json, Value};
use aibus_testkit::TempDir;

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json_out(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

fn ok(home: &Path, args: &[&str]) -> Output {
    let out = aibus(home, args);
    assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
    out
}

fn proj(home: &Path) -> PathBuf {
    home.join("proj")
}

fn handoffs(home: &Path, topic: &str) -> Vec<PathBuf> {
    let dir = proj(home).join("thoughts/shared/handoffs").join(topic);
    let mut files: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    files
}

// ---- ledger ---------------------------------------------------------------------------------

#[test]
fn a_ledger_is_started_changed_and_shown() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let out = aibus(h, &["ledger", "show"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("ledger new general"), "{}", stderr(&out));

    let made = stdout(&ok(h, &["ledger", "new", "auth"]));
    let file = proj(h).join("thoughts/ledgers/CONTINUITY_auth.md");
    assert_eq!(made.trim(), file.to_string_lossy());
    let text = fs::read_to_string(&file).unwrap();
    for s in ["## Goal", "## Completed", "## In Progress", "## Blockers", "## Decisions", "Updated: "] {
        assert!(text.contains(s), "{s}: {text}");
    }
    assert_eq!(aibus(h, &["ledger", "new", "auth"]).status.code(), Some(2), "not overwritten");
    // A topic is one path segment: nothing that climbs or hides.
    assert_eq!(aibus(h, &["ledger", "new", "../escape"]).status.code(), Some(2));
    assert_eq!(aibus(h, &["ledger", "new", ".hidden"]).status.code(), Some(2));
    assert_eq!(fs::read_dir(proj(h).join("thoughts/ledgers")).unwrap().count(), 1);

    ok(h, &["--topic", "auth", "ledger", "update", "goal", "Move auth to middleware"]);
    ok(h, &["--topic", "auth", "ledger", "update", "in_progress", "write the test"]);
    ok(h, &["--topic", "auth", "ledger", "update", "In Progress", "run the migration", "--append"]);
    let shown = json_out(&ok(h, &["--topic", "auth", "ledger", "show", "--json"]));
    assert_eq!(shown["topic"], "auth");
    assert_eq!(shown["sections"]["Goal"], "Move auth to middleware");
    assert_eq!(shown["sections"]["In Progress"], "- write the test\n- run the migration");
    assert!(stdout(&ok(h, &["--topic", "auth", "ledger", "show"])).contains("## In Progress\n- write the test\n"));

    let bad = aibus(h, &["--topic", "auth", "ledger", "update", "notes", "x"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("Goal, Completed"), "{}", stderr(&bad));
    assert_eq!(aibus(h, &["--topic", "a b", "ledger", "show"]).status.code(), Some(2));
}

#[test]
fn the_most_recently_modified_ledger_is_the_topic_unless_one_is_given() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    ok(h, &["ledger", "new", "first"]);
    ok(h, &["ledger", "new", "second"]);
    assert_eq!(json_out(&ok(h, &["ledger", "show", "--json"]))["topic"], "second");
    ok(h, &["--topic", "first", "ledger", "update", "goal", "touch it"]);
    assert_eq!(json_out(&ok(h, &["ledger", "show", "--json"]))["topic"], "first");
    assert_eq!(json_out(&ok(h, &["--topic", "second", "ledger", "show", "--json"]))["topic"], "second");

    // `continuity.topic = "branch"` names it after the git branch instead.
    fs::create_dir_all(proj(h).join(".aibus")).unwrap();
    fs::write(proj(h).join(".aibus/settings.toml"), "[continuity]\ntopic = \"branch\"\n").unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(proj(h))
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "feature/login"]);
    git(&["commit", "-q", "--allow-empty", "-m", "first"]);
    ok(h, &["ledger", "update", "goal", "on a branch"]);
    assert!(proj(h).join("thoughts/ledgers/CONTINUITY_feature-login.md").exists());
}

// ---- handoff --------------------------------------------------------------------------------

#[test]
fn handoffs_are_created_listed_and_shown() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let none = aibus(h, &["handoff", "show"]);
    assert_eq!(none.status.code(), Some(2), "{}", stderr(&none));
    let no_goal = aibus(h, &["--topic", "auth", "handoff", "create"]);
    assert_eq!(no_goal.status.code(), Some(2));
    assert!(stderr(&no_goal).contains("no goal"), "{}", stderr(&no_goal));
    assert!(handoffs(h, "auth").is_empty());

    ok(h, &["--topic", "auth", "ledger", "update", "goal", "Move auth to middleware"]);
    ok(h, &["--topic", "auth", "ledger", "update", "in progress", "run the migration\nadd a test"]);
    let made = stdout(&ok(h, &["--topic", "auth", "handoff", "create", "--outcome", "partial-plus"]));
    let path = PathBuf::from(made.trim());
    assert_eq!(path.parent().unwrap(), proj(h).join("thoughts/shared/handoffs/auth"));
    assert!(path.file_name().unwrap().to_string_lossy().ends_with("_move-auth-to-middleware.yaml"));
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\nsession: auth\n"), "{text}");
    assert!(text.contains("outcome: PARTIAL_PLUS\n---\ngoal: Move auth to middleware\nnow: run the migration\n"), "{text}");

    // `AIBUS_OUTCOME` answers when there is no flag; `--goal` and `--now` replace the ledger's.
    let second = command(h)
        .env("AIBUS_OUTCOME", "succeeded")
        .args(["--topic", "other", "handoff", "create", "--goal", "Another goal", "--now", "Nothing"])
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    let other = fs::read_to_string(stdout(&second).trim()).unwrap();
    assert!(other.contains("status: complete\noutcome: SUCCEEDED\n---\ngoal: Another goal\nnow: Nothing\n"), "{other}");
    assert_eq!(aibus(h, &["--topic", "auth", "handoff", "create", "--outcome", "great"]).status.code(), Some(2));

    // Every topic, newest first, as an object (`handoff` schema); one topic with --topic.
    let all = json_out(&ok(h, &["handoff", "list", "--json"]));
    assert_valid("handoff", &all);
    let listed = all["handoffs"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|e| e["topic"] == "auth" && e["outcome"] == "PARTIAL_PLUS" && e["goal"] == "Move auth to middleware"));
    assert!(listed.iter().any(|e| e["topic"] == "other" && e["status"] == "complete"));
    let one = json_out(&ok(h, &["--topic", "auth", "handoff", "list", "--json"]));
    assert_eq!(one["handoffs"].as_array().unwrap().len(), 1);
    assert!(stdout(&ok(h, &["handoff", "list"])).lines().count() == 2);

    // `show` takes a path, or the newest.
    let shown = json_out(&ok(h, &["handoff", "show", path.to_str().unwrap(), "--json"]));
    assert_valid("handoff", &shown);
    assert_eq!(shown["goal"], "Move auth to middleware");
    assert_eq!(shown["next"], json!(["add a test"]));
    assert_eq!(stdout(&ok(h, &["handoff", "show", path.to_str().unwrap()])), text);
    assert_eq!(json_out(&ok(h, &["--topic", "auth", "handoff", "show", "--json"]))["session"], "auth");
    let missing = aibus(h, &["handoff", "show", "nowhere.yaml"]);
    assert_eq!(missing.status.code(), Some(2));
}

// ---- memory ---------------------------------------------------------------------------------

#[test]
fn memory_is_added_indexed_and_recalled() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let user = h.join(".local/share/aibus/agent-memory/lead/LEARNINGS.md");
    ok(h, &["memory", "add", "The cache key is the file hash", "--type", "fact"]);
    let text = fs::read_to_string(&user).unwrap();
    assert!(text.contains("- [fact, high] The cache key is the file hash\n"), "{text}");
    ok(h, &["memory", "add", "Do not grep for templated errors", "--type", "failed_approach", "--scope", "project"]);
    assert!(proj(h).join(".aibus/agent-memory/lead/LEARNINGS.md").exists());
    assert_eq!(aibus(h, &["memory", "add", "x", "--type", "gossip"]).status.code(), Some(2));
    assert_eq!(aibus(h, &["memory", "add", "x", "--scope", "world"]).status.code(), Some(2));
    assert_eq!(aibus(h, &["memory", "add", "  "]).status.code(), Some(2));

    let hits = json_out(&ok(h, &["memory", "recall", "cache hash", "--json"]));
    assert_valid("memory-recall", &hits);
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["path"], user.to_string_lossy().as_ref());
    assert!(hits[0]["snippet"].as_str().unwrap().contains("[cache]"), "{}", hits[0]);
    let both = json_out(&ok(h, &["memory", "recall", "grep OR cache", "--json"]));
    assert_eq!(both.as_array().unwrap().len(), 2);
    let one = json_out(&ok(h, &["memory", "recall", "grep cache", "-k", "1", "--json"]));
    assert_eq!(one.as_array().unwrap().len(), 1);
    assert!(stdout(&ok(h, &["memory", "recall", "cache"])).contains("LEARNINGS.md"));

    // The index is a cache: gone, `memory index` builds it again, and a second run reads nothing.
    fs::remove_file(h.join(".local/share/aibus/index.sqlite")).unwrap();
    let built = json_out(&ok(h, &["memory", "index", "--json"]));
    assert_eq!((built["added"].as_u64(), built["unchanged"].as_u64()), (Some(2), Some(0)));
    let again = json_out(&ok(h, &["memory", "index", "--json"]));
    assert_eq!((again["added"].as_u64(), again["unchanged"].as_u64()), (Some(0), Some(2)));
    assert!(stdout(&ok(h, &["memory", "index"])).starts_with("indexed: 0 added"));
}

#[test]
fn the_local_scope_is_gitignored_when_it_is_first_written() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    ok(h, &["memory", "add", "private note", "--scope", "local"]);
    assert!(proj(h).join(".aibus/agent-memory-local/lead/LEARNINGS.md").exists());
    assert_eq!(fs::read_to_string(proj(h).join(".gitignore")).unwrap(), ".aibus/agent-memory-local/\n");
    ok(h, &["memory", "add", "another", "--scope", "local"]);
    assert_eq!(fs::read_to_string(proj(h).join(".gitignore")).unwrap().matches("agent-memory-local").count(), 1);
}

// ---- a session end to end -------------------------------------------------------------------

const LEARNINGS_REPLY: &str = r#"[{"type":"working_solution","confidence":"high","content":"Run the failing test alone first to see the real error"},{"type":"fact","confidence":"medium","content":"Refresh tokens rotate on every use"},{"type":"gossip","confidence":"high","content":"dropped"}]"#;

/// Writes a script file in the home dir and returns its path.
fn script(home: &Path, name: &str, text: &str) -> PathBuf {
    let file = home.join(name);
    fs::write(&file, text).unwrap();
    file
}

fn run_json(home: &Path, script: &Path, extra: &[&str], prompt: &str) -> Output {
    command(home)
        .args(["run", "--json"])
        .args(extra)
        .arg("--script")
        .arg(script)
        .arg(prompt)
        .output()
        .unwrap()
}

fn first_session(home: &Path) -> Output {
    let s = format!(
        r#"[ToolCall(name: "ledger_update", input: {{"section": "Goal", "text": "Move auth to middleware"}}),
            ToolCall(name: "ledger_update", input: {{"section": "In Progress", "text": "run the migration\nadd a test"}}),
            Stop(ToolUse),
            Text("The ledger is up to date."), Stop(EndTurn),
            Text({LEARNINGS_REPLY:?})]"#
    );
    let file = script(home, "one.ron", &s);
    run_json(home, &file, &["--topic", "auth", "--outcome", "PARTIAL_PLUS"], "start the auth work")
}

#[test]
fn a_session_ends_with_a_handoff_a_learning_and_the_next_one_sees_both() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let out = first_session(h);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);

    // The status bar's data: `model:ledger` holds the goal and the first open item.
    let end = lines.last().unwrap();
    assert_eq!(end["type"], "SessionEnd");
    let session = end["session"].as_str().unwrap().to_owned();
    let shown = json_out(&ok(h, &["session", "show", &session, "--json"]));
    assert_eq!(shown["ledger"]["goal"], "Move auth to middleware");
    assert_eq!(shown["ledger"]["in_progress"], "run the migration");

    // The handoff: the Continuous-Claude schema, the outcome in the front matter, and named in
    // `SessionEnd` relative to the project.
    let files = handoffs(h, "auth");
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(end["handoff"], files[0].strip_prefix(proj(h)).unwrap().to_string_lossy().as_ref());
    let text = fs::read_to_string(&files[0]).unwrap();
    assert!(text.starts_with("---\nsession: auth\n"), "{text}");
    for want in [
        "status: complete\n",
        "outcome: PARTIAL_PLUS\n---\n",
        "goal: Move auth to middleware\n",
        "now: run the migration\n",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    let parsed = json_out(&ok(h, &["--topic", "auth", "handoff", "show", "--json"]));
    assert_eq!(parsed["next"], json!(["add a test"]));
    assert_eq!(parsed["decisions"], json!([]));

    // `HandoffWritten` went through the graph, and the run event list is still valid.
    for l in &lines {
        assert_valid("run-event", l);
    }

    // The learnings: two valid entries, the invalid one dropped, appended and indexed.
    let learnings = fs::read_to_string(h.join(".local/share/aibus/agent-memory/lead/LEARNINGS.md")).unwrap();
    assert!(learnings.contains(&format!("session {session}")), "{learnings}");
    assert!(learnings.contains("- [working_solution, high] Run the failing test alone first"), "{learnings}");
    assert!(learnings.contains("- [fact, medium] Refresh tokens rotate on every use"), "{learnings}");
    assert!(!learnings.contains("dropped"), "{learnings}");
    let found = json_out(&ok(h, &["memory", "recall", "failing test", "--json"]));
    assert!(found[0]["path"].as_str().unwrap().ends_with("agent-memory/lead/LEARNINGS.md"), "{found}");
    assert!(found[0]["snippet"].as_str().unwrap().contains("[failing]"), "{found}");

    // The next session's lead sees the handoff line, the whole file and the ledger.
    let two = script(
        h,
        "two.ron",
        r#"[MatchPrompt(contains: "[handoff] goal: Move auth to middleware now: run the migration", then: [
                MatchPrompt(contains: "outcome: PARTIAL_PLUS", then: [
                    MatchPrompt(contains: "[ledger] auth", then: [Text("saw all three"), Stop(EndTurn)])])]),
            Text("[]")]"#,
    );
    let next = run_json(h, &two, &["--topic", "auth"], "carry on");
    assert!(next.status.success(), "{}", stderr(&next));
    let finished = ndjson(&next);
    let summary = finished.iter().find(|l| l["type"] == "RunFinished").unwrap()["summary"].as_str().unwrap().to_owned();
    assert_eq!(summary, "saw all three");
    // Nothing changed in the ledger, so there is no second handoff.
    assert_eq!(handoffs(h, "auth").len(), 1);
    assert_eq!(finished.last().unwrap()["handoff"], Value::Null);
}

#[test]
fn another_topic_does_not_see_the_handoff_and_a_worker_never_sees_the_ledger() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    assert!(first_session(h).status.success());

    // A different topic: no handoff of its own, so nothing is shown.
    let quiet = script(h, "quiet.ron", r#"[MatchPrompt(contains: "[handoff]", then: [Text("leaked"), Stop(EndTurn)]), Text("clean"), Stop(EndTurn), Text("[]")]"#);
    let out = run_json(h, &quiet, &["--topic", "billing"], "other work");
    let done = ndjson(&out);
    assert_eq!(done.iter().find(|l| l["type"] == "RunFinished").unwrap()["summary"], "clean");

    // The delegated worker's first request is the first one the script sees.
    let worker = script(
        h,
        "worker.ron",
        r#"[MatchPrompt(contains: "[ledger]", then: [Text("LEAK"), Stop(EndTurn)]),
            MatchPrompt(contains: "[handoff]", then: [Text("LEAK"), Stop(EndTurn)]),
            Text("worker result"), Stop(EndTurn), Text("lead done"), Stop(EndTurn), Text("[]")]"#,
    );
    let out = run_json(h, &worker, &["--topic", "auth"], "@agent-explore look around");
    assert!(out.status.success(), "{}", stderr(&out));
    let summaries: Vec<String> = ndjson(&out)
        .iter()
        .filter(|l| l["type"] == "RunFinished")
        .map(|l| l["summary"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(summaries, ["worker result", "lead done"]);
}

#[test]
fn the_end_of_a_session_respects_its_switches() {
    // No ledger change: no handoff. `extract_on_end = false`: no learnings.
    let home = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(proj(h).join(".aibus")).unwrap();
    fs::write(proj(h).join(".aibus/settings.toml"), "[memory]\nextract_on_end = false\n").unwrap();
    let s = script(h, "s.ron", &format!("[Text(\"plain answer\"), Stop(EndTurn), Text({LEARNINGS_REPLY:?})]"));
    let out = run_json(h, &s, &["--topic", "auth"], "just talk");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(ndjson(&out).last().unwrap()["handoff"], Value::Null);
    assert!(handoffs(h, "auth").is_empty());
    assert!(!h.join(".local/share/aibus/agent-memory/lead/LEARNINGS.md").exists());

    // `handoff_on_end = false` keeps the handoff away even though the ledger changed.
    fs::write(proj(h).join(".aibus/settings.toml"), "[continuity]\nhandoff_on_end = false\n[memory]\nextract_on_end = false\n").unwrap();
    let s = script(
        h,
        "t.ron",
        r#"[ToolCall(name: "ledger_update", input: {"section": "Goal", "text": "G"}), Stop(ToolUse), Text("done"), Stop(EndTurn)]"#,
    );
    let out = run_json(h, &s, &["--topic", "auth"], "update the ledger");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(handoffs(h, "auth").is_empty());
    assert!(proj(h).join("thoughts/ledgers/CONTINUITY_auth.md").exists());
}

#[test]
fn a_run_that_did_not_complete_extracts_nothing() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    // The provider fails: the lead's run does not complete, so nothing is asked of the model.
    let s = script(h, "fail.ron", r#"[Error((kind: Server, retryable: false, message: "down"))]"#);
    let out = run_json(h, &s, &[], "this fails");
    assert!(!out.status.success());
    assert!(!h.join(".local/share/aibus/agent-memory/lead/LEARNINGS.md").exists());
}
