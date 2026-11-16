//! #355 (phase 1 of #348): list and watch Claude Code sessions, read-only. Fixtures are in
//! `fixtures/claude-code/tap-355/`: the field names of `agents.json` were checked against
//! `claude agents --json` (2.1.304), every value is synthetic. The transcript fixture is an
//! assumption about an internal format, which is what the version gate is for.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::Redactor;
use aibus_tap::{agents, hooks, intent, show, tap, transcript, Env};
use aibus_trace::view::{self, Filter, Recorded};
use serde_json::{json, Value};

const S: &str = "22222222-2222-4222-8222-222222222222";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/claude-code/tap-355")
        .join(name)
}

fn read(name: &str) -> Vec<u8> {
    fs::read(fixture(name)).unwrap()
}

fn env(tmp: &Path) -> Env {
    Env {
        state: tmp.join("state"),
        config: tmp.join("cfg"),
    }
}

fn lister() -> Result<Vec<u8>, String> {
    Ok(read("agents.json"))
}

fn ingest_all(root: &Path) -> tap::Tap {
    let mut t = tap::Tap::open(root, S).unwrap();
    for line in read("hooks.jsonl").split(|b| *b == b'\n') {
        if !line.is_empty() {
            t.ingest(line, SystemTime::now()).unwrap();
        }
    }
    t
}

fn endpoint() -> hooks::Endpoint {
    hooks::Endpoint {
        port: 47831,
        token: "synthetic-token-0123456789".into(),
    }
}

// ---- the list ----

#[test]
fn the_list_reads_the_field_names_of_claude_agents_json() {
    let l = agents::parse_agents(&read("agents.json")).unwrap();
    assert_eq!(l.len(), 4);
    assert_eq!(l[0].job_id.as_deref(), Some("bg-0001"));
    assert_eq!(l[0].kind, "background");
    assert_eq!(l[0].state, "blocked");
    assert_eq!(l[1].session_id, S);
    assert_eq!(l[1].state, "busy");
    assert_eq!(l[1].pid, Some(4242));
    assert_eq!(l[1].started_ms, Some(1_700_000_100_000));
    // A background session that also has a status: `state` is the one that says how it is.
    assert_eq!(l[3].state, "done");
}

#[test]
fn the_list_refuses_what_is_not_an_array_of_sessions() {
    assert!(agents::parse_agents(b"{}").is_err());
    assert!(agents::parse_agents(b"not json").is_err());
    // An entry without a session id is not a session aibus can tap.
    let l = agents::parse_agents(br#"[{"cwd":"/x","kind":"interactive"}]"#).unwrap();
    assert!(l.is_empty());
}

#[test]
fn the_rendered_list_says_what_aibus_can_do_and_cleans_the_terminal() {
    let l = agents::parse_agents(&read("agents.json")).unwrap();
    let text = agents::render_list(&l, &[S.to_owned()], 1_700_000_400_000);
    assert!(text.contains("refactor parser"), "{text}");
    assert!(text.contains("watch"), "{text}");
    assert!(text.contains("busy") && text.contains("idle"), "{text}");
    assert!(text.contains("tapped"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains("send"), "phase 1 offers no control: {text}");
}

#[test]
fn a_session_is_found_by_prefix_job_id_or_name_and_an_ambiguous_one_is_refused() {
    let l = agents::parse_agents(&read("agents.json")).unwrap();
    assert_eq!(agents::resolve(&l, "2222").unwrap().session_id, S);
    assert_eq!(agents::resolve(&l, "bg-0001").unwrap().kind, "background");
    assert_eq!(
        agents::resolve(&l, "refactor parser").unwrap().session_id,
        S
    );
    assert!(agents::resolve(&l, "").is_err());
    assert!(agents::resolve(&l, "no-such").is_err());
    let two = agents::parse_agents(
        br#"[{"sessionId":"aaaa1","name":"a"},{"sessionId":"aaaa2","name":"b"}]"#,
    )
    .unwrap();
    assert!(agents::resolve(&two, "aaaa")
        .unwrap_err()
        .contains("more than one"));
}

#[test]
fn local_session_files_only_fill_a_missing_pid_and_never_read_a_key_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sessions");
    fs::create_dir(&dir).unwrap();
    let me = std::process::id();
    // The CLI listed this background session without a pid; the file knows one that is alive.
    let id = "11111111-1111-4111-8111-111111111111";
    fs::write(
        dir.join(format!("{me}.json")),
        json!({"pid": me, "sessionId": id, "cwd": "/elsewhere", "kind": "interactive", "status": "busy"}).to_string(),
    )
    .unwrap();
    // A key file next to it, and a file for a session the CLI did not list.
    fs::write(dir.join(format!("{me}.key")), "SYNTHETIC-KEY-MATERIAL").unwrap();
    fs::write(
        dir.join("1.json"),
        json!({"pid": 1, "sessionId": "ffffffff-ffff-4fff-8fff-ffffffffffff"}).to_string(),
    )
    .unwrap();
    let mut l = agents::parse_agents(&read("agents.json")).unwrap();
    let before = l.len();
    agents::merge_local(&mut l, &dir);
    assert_eq!(l.len(), before, "the files never add a session");
    assert_eq!(l[0].pid, Some(me));
    assert_eq!(
        l[0].cwd, "/work/synthetic-a",
        "a field the command has is not overwritten"
    );
    assert!(!format!("{l:?}").contains("SYNTHETIC-KEY-MATERIAL"));
}

#[test]
fn a_local_file_with_a_pid_that_is_not_alive_or_not_the_listed_one_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sessions");
    fs::create_dir(&dir).unwrap();
    // Session 4 is listed with pid 4444; a file claims another pid for it, and the pid of
    // session 1 is not alive (a pid that was reused would be the same shape).
    fs::write(
        dir.join("a.json"),
        json!({"pid": 4_000_000, "sessionId": "11111111-1111-4111-8111-111111111111"}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("b.json"),
        json!({"pid": std::process::id(), "sessionId": "44444444-4444-4444-8444-444444444444"})
            .to_string(),
    )
    .unwrap();
    let mut l = agents::parse_agents(&read("agents.json")).unwrap();
    agents::merge_local(&mut l, &dir);
    assert_eq!(l[0].pid, None);
    assert_eq!(l[3].pid, Some(4444));
}

// ---- hooks in the user's settings ----

#[test]
fn installing_adds_http_hooks_for_every_event_and_keeps_what_was_there() {
    let before: Value = serde_json::from_slice(&read("settings-before.json")).unwrap();
    let after = hooks::with_hooks(&before, &endpoint());
    assert_eq!(after["model"], "synthetic");
    assert_eq!(
        after["hooks"]["Stop"][0]["hooks"][0]["command"],
        "/usr/local/bin/synthetic-stop.sh"
    );
    assert!(!hooks::EVENTS.is_empty());
    for e in hooks::EVENTS {
        let found = after["hooks"][*e]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .find(|h| h["type"] == "http")
            .unwrap_or_else(|| panic!("no http hook for {e}"));
        assert!(found["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:47831/"));
        assert_eq!(
            found["headers"]["Authorization"],
            "Bearer synthetic-token-0123456789"
        );
    }
    // Twice is the same as once.
    assert_eq!(hooks::with_hooks(&after, &endpoint()), after);
    // Uninstalling gives back exactly what the user had.
    assert_eq!(hooks::without_hooks(&after), before);
}

#[test]
fn the_diff_shows_every_added_line_and_nothing_else() {
    let before: Value = serde_json::from_slice(&read("settings-before.json")).unwrap();
    let after = hooks::with_hooks(&before, &endpoint());
    let d = hooks::diff(&before, &after);
    assert!(
        d.contains("+") && d.contains("http://127.0.0.1:47831/"),
        "{d}"
    );
    assert!(
        d.contains("Bearer synthetic-token-0123456789"),
        "the diff is exact: {d}"
    );
    assert!(
        !d.lines().any(|l| l.starts_with('-')),
        "an install removes nothing: {d}"
    );
    assert_eq!(hooks::diff(&before, &before), "");
}

#[test]
fn install_asks_first_and_declining_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(&path, read("settings-before.json")).unwrap();
    let mut shown = String::new();
    let out = hooks::apply(&path, Some(&endpoint()), &mut |d| {
        shown = d.to_owned();
        false
    })
    .unwrap();
    assert_eq!(out, hooks::Outcome::Declined);
    assert!(shown.contains("127.0.0.1:47831"));
    assert_eq!(fs::read(&path).unwrap(), read("settings-before.json"));

    let out = hooks::apply(&path, Some(&endpoint()), &mut |_| true).unwrap();
    assert_eq!(out, hooks::Outcome::Changed);
    let now: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(now["hooks"]["PreToolUse"].is_array());

    // Already there: nothing to ask, nothing written.
    let out = hooks::apply(&path, Some(&endpoint()), &mut |_| panic!("asked")).unwrap();
    assert_eq!(out, hooks::Outcome::Unchanged);

    let out = hooks::apply(&path, None, &mut |_| true).unwrap();
    assert_eq!(out, hooks::Outcome::Changed);
    let back: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        back,
        serde_json::from_slice::<Value>(&read("settings-before.json")).unwrap()
    );
}

#[test]
fn install_refuses_settings_it_cannot_read_instead_of_replacing_them() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(&path, "{ not json").unwrap();
    assert!(hooks::apply(&path, Some(&endpoint()), &mut |_| true).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn the_endpoint_accepts_only_its_own_token() {
    let ep = endpoint();
    assert!(hooks::accepts(
        &ep,
        Some("Bearer synthetic-token-0123456789")
    ));
    assert!(!hooks::accepts(
        &ep,
        Some("Bearer synthetic-token-0123456780")
    ));
    assert!(!hooks::accepts(&ep, Some("synthetic-token-0123456789")));
    assert!(!hooks::accepts(&ep, None));
}

// ---- the tap run ----

#[test]
fn hook_lines_become_trace_events_and_no_text_reaches_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("taps");
    let t = ingest_all(&root);
    let dir = t.dir().to_owned();
    drop(t);
    let raw = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    for secret in [
        "SYNTHETIC-PROMPT-TEXT",
        "SYNTHETIC-SECRET-PATH",
        "SYNTHETIC-TOOL-OUTPUT",
        "SYNTHETIC-SUBAGENT-PROMPT",
    ] {
        assert!(!raw.contains(secret), "{secret} reached the store");
    }
    let rec = Recorded::load(&dir).unwrap();
    let kinds: Vec<_> = rec.events.iter().map(|e| format!("{:?}", e.kind)).collect();
    for k in [
        "AgentStarted",
        "ToolStarted",
        "ToolCompleted",
        "TaskDelegated",
        "AgentStopped",
    ] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing in {kinds:?}");
    }
    // The unknown hook event is counted, never guessed at.
    assert!(rec.events.iter().any(|e| {
        format!("{:?}", e.kind) == "TraceWarning" && e.data["reason"] == "unknown_variant"
    }));
}

#[test]
fn a_session_id_that_is_a_path_or_a_payload_for_another_session_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("taps");
    for bad in ["../x", "a/b", "", ".", "..", "a b", &"x".repeat(200)] {
        assert!(tap::run_dir(&root, bad).is_err(), "{bad:?}");
        assert!(tap::Tap::open(&root, bad).is_err(), "{bad:?}");
    }
    assert!(!tmp.path().join("x").exists());
    let mut t = tap::Tap::open(&root, S).unwrap();
    let other = br#"{"session_id":"33333333-3333-4333-8333-333333333333","hook_event_name":"SessionStart"}"#;
    assert!(t.ingest(other, SystemTime::now()).is_err());
    assert!(t.ingest(b"{not json", SystemTime::now()).is_err());
}

#[test]
fn the_transcript_path_a_hook_reported_is_remembered() {
    let tmp = tempfile::tempdir().unwrap();
    let t = ingest_all(&tmp.path().join("taps"));
    let p = tap::transcript_path(t.dir()).unwrap();
    assert!(p.ends_with(format!("{S}.jsonl")));
}

// ---- what is shown ----

fn shown(tmp: &Path) -> String {
    let e = env(tmp);
    let root = e.state.join("taps").join("claude");
    drop(ingest_all(&root));
    show::tap(&e, &lister, "refactor parser").unwrap()
}

#[test]
fn the_tap_text_is_the_runs_inspect_text_with_a_header_and_a_status_line() {
    let tmp = tempfile::tempdir().unwrap();
    let text = shown(tmp.path());
    let dir = tmp.path().join("state/taps/claude").join(S);
    let rec = Recorded::load(&dir).unwrap();
    let inspect = format!(
        "{}{}",
        view::graph(&rec, S),
        view::timeline(&rec, &Filter::default())
    );
    assert!(
        text.contains(&inspect),
        "same renderer as `aibus runs inspect`:\n{text}"
    );
    assert!(
        text.contains("refactor parser") && text.contains("version: unavailable"),
        "{text}"
    );
    assert!(
        text.contains("hooks") && text.contains("observed"),
        "{text}"
    );
    assert!(text.contains("watch only"), "{text}");
    assert!(text.contains("transcripts: off"), "{text}");
    assert!(!text.contains("SYNTHETIC-PROMPT-TEXT"));
}

#[test]
fn a_session_nobody_has_posted_for_says_unavailable_not_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let text = show::tap(&e, &lister, "3333").unwrap();
    assert!(text.contains("unavailable"), "{text}");
    assert!(text.contains("hooks install"), "{text}");
}

#[test]
fn the_list_text_marks_a_tapped_session() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    drop(ingest_all(&e.state.join("taps").join("claude")));
    let text = show::list(&e, &lister).unwrap();
    assert!(
        text.contains("tapped") && text.contains("refactor parser"),
        "{text}"
    );
}

// ---- intent ----

#[test]
fn plans_and_tasks_are_intent_redacted_and_cleaned() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("cfg");
    fs::create_dir_all(cfg.join("tasks").join(S)).unwrap();
    fs::write(
        cfg.join("tasks").join(S).join("1.json"),
        json!({"id": "1", "subject": "write parser \u{1b}[31mred", "status": "in_progress", "note": "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA"}).to_string(),
    )
    .unwrap();
    let lines = intent::read(&cfg, S, None, &Redactor::new());
    let text = lines.join("\n");
    assert!(
        text.contains("write parser") && text.contains("in_progress"),
        "{text}"
    );
    assert!(
        !text.contains('\u{1b}') && !text.contains("sk-ant-api03"),
        "{text:?}"
    );
}

#[test]
fn a_plan_the_cli_never_wrote_down_is_not_invented() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(intent::read(&tmp.path().join("cfg"), S, None, &Redactor::new()).is_empty());
}

// ---- transcripts: off unless the user said so ----

#[test]
fn transcripts_are_off_by_default_and_the_file_is_not_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let t = tmp.path().join("cfg/projects/p/t.jsonl");
    fs::create_dir_all(t.parent().unwrap()).unwrap();
    fs::write(&t, read("transcript.jsonl")).unwrap();
    assert!(!transcript::allowed(&state));
    let tail = transcript::tail(
        &state,
        &tmp.path().join("cfg"),
        &t,
        Some("2.1.304"),
        &Redactor::new(),
    );
    assert_eq!(tail.status, transcript::Status::Off);
    assert!(tail.messages.is_empty());
    assert_eq!(tail.bytes_read, 0);
}

#[test]
fn with_the_opt_in_a_tested_version_shows_redacted_messages_and_counts_unknown_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let cfg = tmp.path().join("cfg");
    let t = cfg.join("projects/p/t.jsonl");
    fs::create_dir_all(t.parent().unwrap()).unwrap();
    fs::write(&t, read("transcript.jsonl")).unwrap();
    transcript::set_allowed(&state, true).unwrap();
    assert!(transcript::allowed(&state));
    assert!(transcript::TESTED.contains(&"2.1.304"));
    let tail = transcript::tail(&state, &cfg, &t, Some("2.1.304"), &Redactor::new());
    assert_eq!(tail.status, transcript::Status::Tested);
    let text = tail.messages.join("\n");
    assert!(
        text.contains("SYNTHETIC-ASSISTANT-TEXT") && text.contains("SYNTHETIC-USER-TEXT"),
        "{text}"
    );
    assert!(!text.contains("sk-ant-api03"), "{text}");
    assert_eq!(tail.tool_calls, 1);
    assert_eq!(
        tail.unknown_variants, 2,
        "one unknown entry type and one unknown block type"
    );
    assert!(!text.contains("SYNTHETIC-UNKNOWN-PAYLOAD"));
    transcript::set_allowed(&state, false).unwrap();
    assert!(!transcript::allowed(&state));
}

#[test]
fn an_untested_version_is_labelled_and_a_big_file_is_only_tailed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let cfg = tmp.path().join("cfg");
    let t = cfg.join("projects/p/t.jsonl");
    fs::create_dir_all(t.parent().unwrap()).unwrap();
    let line = String::from_utf8(read("transcript.jsonl")).unwrap();
    fs::write(&t, line.repeat(2000)).unwrap();
    transcript::set_allowed(&state, true).unwrap();
    let tail = transcript::tail(&state, &cfg, &t, Some("9.9.9"), &Redactor::new());
    assert_eq!(tail.status, transcript::Status::UntestedVersion);
    assert!(
        tail.bytes_read > 0 && tail.bytes_read <= 1 << 20,
        "{}",
        tail.bytes_read
    );
    assert!(tail.messages.len() <= 20);
}

#[test]
fn a_transcript_outside_the_config_dir_or_a_link_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let cfg = tmp.path().join("cfg");
    fs::create_dir_all(cfg.join("projects")).unwrap();
    transcript::set_allowed(&state, true).unwrap();
    let outside = tmp.path().join("elsewhere.jsonl");
    fs::write(&outside, read("transcript.jsonl")).unwrap();
    let tail = transcript::tail(&state, &cfg, &outside, Some("2.1.304"), &Redactor::new());
    assert!(
        matches!(tail.status, transcript::Status::Unreadable(_)),
        "{:?}",
        tail.status
    );
    assert!(tail.messages.is_empty());
    let link = cfg.join("projects/link.jsonl");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let tail = transcript::tail(&state, &cfg, &link, Some("2.1.304"), &Redactor::new());
    assert!(matches!(tail.status, transcript::Status::Unreadable(_)));
}
