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
    let mut t = tap::Tap::open(root, S, &Redactor::new()).unwrap();
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

// ---- hooks in the user's settings: command hooks, a script, no token in settings ----

fn before_text() -> String {
    String::from_utf8(read("settings-before.json")).unwrap()
}

const CMD: &str = "/state/tap/aibus-tap-hook.sh";

#[test]
fn installing_adds_command_hooks_for_every_event_and_no_token() {
    let after = hooks::edit(&before_text(), Some(CMD))
        .unwrap()
        .expect("a change");
    let v: Value = serde_json::from_str(&after).unwrap();
    assert_eq!(v["model"], "synthetic");
    assert_eq!(
        v["hooks"]["Stop"][0]["hooks"][0]["command"],
        "/usr/local/bin/synthetic-stop.sh"
    );
    assert!(!hooks::EVENTS.is_empty());
    for e in hooks::EVENTS {
        let found = v["hooks"][*e]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .find(|h| h["type"] == "command")
            .unwrap_or_else(|| panic!("no command hook for {e}"));
        assert_eq!(found["command"], CMD);
        assert!(found.get("headers").is_none() && found.get("url").is_none());
    }
    assert!(
        !after.contains("Bearer") && !after.contains("http://"),
        "{after}"
    );
    // Twice is the same as once.
    assert_eq!(hooks::edit(&after, Some(CMD)).unwrap(), None);
}

#[test]
fn install_then_uninstall_gives_back_the_same_bytes_and_keeps_the_users_own_entries() {
    let tmp = tempfile::tempdir().unwrap();
    // Keys in the user's order, an empty event list of their own and a group with no hooks.
    let original = "{\n  \"model\": \"synthetic\",\n  \"hooks\": {\n    \"Stop\": [],\n    \"PostToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": []\n      }\n    ]\n  },\n  \"alpha\": 1\n}\n";
    let after = hooks::edit(original, Some(CMD)).unwrap().unwrap();
    let keys: Vec<_> = after.lines().filter(|l| l.starts_with("  \"")).collect();
    assert!(
        keys[0].contains("model") && keys[1].contains("hooks") && keys[2].contains("alpha"),
        "{after}"
    );
    assert!(after.contains("\"Stop\": []"), "{after}");
    let back = hooks::edit(&after, None).unwrap().unwrap();
    assert_eq!(back, original);
    drop(tmp);
}

#[test]
fn the_diff_shows_every_added_line_and_nothing_else() {
    let before = before_text();
    let after = hooks::edit(&before, Some(CMD)).unwrap().unwrap();
    let d = hooks::diff(&before, &after);
    assert!(d.contains("+") && d.contains(CMD), "{d}");
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
    let out = hooks::apply(&path, Some(CMD), &mut |d| {
        shown = d.to_owned();
        false
    })
    .unwrap();
    assert_eq!(out, hooks::Outcome::Declined);
    assert!(shown.contains(CMD));
    assert_eq!(fs::read(&path).unwrap(), read("settings-before.json"));

    let out = hooks::apply(&path, Some(CMD), &mut |_| true).unwrap();
    assert_eq!(out, hooks::Outcome::Changed);
    let now: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(now["hooks"]["PreToolUse"].is_array());
    let out = hooks::apply(&path, Some(CMD), &mut |_| panic!("asked")).unwrap();
    assert_eq!(out, hooks::Outcome::Unchanged);
    let out = hooks::apply(&path, None, &mut |_| true).unwrap();
    assert_eq!(out, hooks::Outcome::Changed);
    assert_eq!(fs::read(&path).unwrap(), read("settings-before.json"));
}

#[test]
fn install_refuses_settings_it_cannot_read_or_an_event_that_is_not_a_list() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(&path, "{ not json").unwrap();
    assert!(hooks::apply(&path, Some(CMD), &mut |_| true).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    let odd = "{\"hooks\": {\"PreToolUse\": {\"oops\": true}}}\n";
    fs::write(&path, odd).unwrap();
    let e = hooks::apply(&path, Some(CMD), &mut |_| true).unwrap_err();
    assert!(e.contains("PreToolUse"), "{e}");
    assert!(
        hooks::apply(&path, None, &mut |_| true).is_err(),
        "uninstall refuses too"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), odd);
}

#[test]
fn the_backup_has_the_settings_mode_and_is_refreshed_by_every_change() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(&path, read("settings-before.json")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    hooks::apply(&path, Some(CMD), &mut |_| true).unwrap();
    let backup = tmp.path().join("settings.json.aibus-before");
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&backup), 0o600);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::read(&backup).unwrap(), read("settings-before.json"));
    // The user edits the file, then installs again from another path: the backup follows.
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("synthetic", "edited");
    fs::write(&path, &edited).unwrap();
    hooks::apply(&path, None, &mut |_| true).unwrap();
    assert_eq!(fs::read_to_string(&backup).unwrap(), edited);
}

#[test]
fn install_files_write_a_0700_script_and_0600_files_and_keep_the_token_out_of_the_script() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let ep = endpoint();
    let script = hooks::install_files(tmp.path(), &ep).unwrap();
    assert!(script.ends_with(hooks::SCRIPT_NAME));
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&script), 0o700);
    let dir = script.parent().unwrap();
    assert_eq!(mode(&dir.join("hook.header")), 0o600);
    assert_eq!(mode(&dir.join("hook.url")), 0o600);
    assert!(fs::read_to_string(dir.join("hook.header"))
        .unwrap()
        .contains(&ep.token));
    assert!(!fs::read_to_string(&script).unwrap().contains(&ep.token));
    assert!(fs::read_to_string(dir.join("hook.url"))
        .unwrap()
        .contains("127.0.0.1:47831"));
}

#[test]
fn uninstall_rotates_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let ep = hooks::Endpoint::load_or_create(tmp.path()).unwrap();
    let script = hooks::install_files(tmp.path(), &ep).unwrap();
    let next = ep.rotate(tmp.path()).unwrap();
    assert_eq!(next.port, ep.port);
    assert_ne!(next.token, ep.token);
    assert_eq!(hooks::Endpoint::load_or_create(tmp.path()).unwrap(), next);
    let header = fs::read_to_string(script.parent().unwrap().join("hook.header")).unwrap();
    assert!(header.contains(&next.token) && !header.contains(&ep.token));
}

/// Runs the hook script with `input` on stdin; returns (stdout, exit ok, seconds).
fn run_script(script: &Path, input: &[u8]) -> (String, bool, f64) {
    use std::io::Write;
    let t = std::time::Instant::now();
    let mut c = std::process::Command::new(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let _ = c.stdin.take().unwrap().write_all(input);
    let o = c.wait_with_output().unwrap();
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        o.status.success(),
        t.elapsed().as_secs_f64(),
    )
}

fn script_for(port: u16) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let ep = hooks::Endpoint {
        port,
        token: "synthetic-token-0123456789".into(),
    };
    let script = hooks::install_files(tmp.path(), &ep).unwrap();
    (tmp, script)
}

#[test]
fn the_script_fails_open_at_once_when_nothing_listens() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (_t, script) = script_for(port);
    let (out, ok, secs) = run_script(&script, br#"{"session_id":"x"}"#);
    assert!(ok && out.is_empty(), "{out:?}");
    assert!(secs < 1.0, "{secs}");
}

#[test]
fn a_listener_that_accepts_and_never_answers_delays_the_session_by_the_scripts_timeout_only() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let keep = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(5));
        drop(s);
    });
    let (_t, script) = script_for(port);
    let (out, ok, secs) = run_script(&script, br#"{"session_id":"x"}"#);
    assert!(ok && out.is_empty(), "{out:?}");
    assert!(
        (0.5..2.5).contains(&secs),
        "the script gives up after about a second: {secs}"
    );
    drop(keep);
}

#[test]
fn a_reply_that_tries_to_decide_something_is_never_printed() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let body =
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#;
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    });
    let (_t, script) = script_for(port);
    let (out, ok, _) = run_script(&script, br#"{"session_id":"x"}"#);
    assert!(
        ok && out.is_empty(),
        "the reply must not reach Claude Code: {out:?}"
    );
    t.join().unwrap();
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
        assert!(
            tap::Tap::open(&root, bad, &Redactor::new()).is_err(),
            "{bad:?}"
        );
    }
    assert!(!tmp.path().join("x").exists());
    let mut t = tap::Tap::open(&root, S, &Redactor::new()).unwrap();
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
    show::tap(&e, &lister, &Redactor::new(), "refactor parser").unwrap()
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
    let text = show::tap(&e, &lister, &Redactor::new(), "3333").unwrap();
    assert!(text.contains("unavailable"), "{text}");
    assert!(text.contains("hooks install"), "{text}");
}

#[test]
fn the_list_text_marks_a_tapped_session() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    drop(ingest_all(&e.state.join("taps").join("claude")));
    let text = show::list(&e, &lister, &Redactor::new()).unwrap();
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
    assert!(
        transcript::TESTED.is_empty(),
        "no real transcript was ever parsed"
    );
    let tail = transcript::tail(&state, &cfg, &t, Some("2.1.304"), &Redactor::new());
    assert_eq!(tail.status, transcript::Status::UntestedVersion);
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

// ---- review fixes (#356) ----

#[test]
fn the_screen_does_not_claim_a_version_was_tested_without_a_real_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    drop(ingest_all(&e.taps()));
    transcript::set_allowed(&e.state, true).unwrap();
    let t = e
        .config
        .join("projects/-work-synthetic-b")
        .join(format!("{S}.jsonl"));
    fs::create_dir_all(t.parent().unwrap()).unwrap();
    fs::write(&t, read("transcript.jsonl")).unwrap();
    let text = show::tap(&e, &lister, &Redactor::new(), "refactor parser").unwrap();
    assert!(!text.contains("version tested"), "{text}");
    assert!(
        text.contains("format not verified against a real transcript"),
        "{text}"
    );
}

#[test]
fn a_transcript_path_that_is_not_this_sessions_file_is_not_remembered() {
    let tmp = tempfile::tempdir().unwrap();
    let mut t = tap::Tap::open(&tmp.path().join("taps"), S, &Redactor::new()).unwrap();
    let forged = format!(
        r#"{{"session_id":"{S}","transcript_path":"/cfg/projects/-p/33333333-3333-4333-8333-333333333333.jsonl","cwd":"/x","hook_event_name":"SessionStart"}}"#
    );
    t.ingest(forged.as_bytes(), SystemTime::now()).unwrap();
    assert_eq!(tap::transcript_path(t.dir()), None);
    let fine = format!(
        r#"{{"session_id":"{S}","transcript_path":"/cfg/projects/-p/{S}.jsonl","cwd":"/x","hook_event_name":"SessionStart"}}"#
    );
    t.ingest(fine.as_bytes(), SystemTime::now()).unwrap();
    assert!(tap::transcript_path(t.dir()).is_some());
}

#[test]
fn the_tap_meta_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let t = ingest_all(&tmp.path().join("taps"));
    let m = fs::metadata(t.dir().join("tap-meta.json")).unwrap();
    assert_eq!(m.permissions().mode() & 0o777, 0o600);
}

#[test]
fn a_registered_secret_is_redacted_in_the_list_and_the_tap_text() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let secret = "hunter2-synthetic-registered-secret";
    let r = Redactor::new();
    r.register(&aibus_core::AccountName("acct".into()), secret);
    let named = |_: ()| -> Result<Vec<u8>, String> {
        Ok(format!(
            r#"[{{"sessionId":"{S}","name":"job {secret}","cwd":"/w/{secret}","kind":"interactive","status":"busy","startedAt":1700000000000}}]"#
        )
        .into_bytes())
    };
    let l = show::list(&e, &|| named(()), &r).unwrap();
    assert!(!l.contains(secret) && l.contains("redacted"), "{l}");
    let t = show::tap(&e, &|| named(()), &r, "job").unwrap_or_else(|_| String::new());
    let t = if t.is_empty() {
        show::tap(&e, &|| named(()), &r, S).unwrap()
    } else {
        t
    };
    assert!(!t.contains(secret), "{t}");
    fs::create_dir_all(e.config.join("tasks").join(S)).unwrap();
    fs::write(
        e.config.join("tasks").join(S).join("1.json"),
        json!({"id":"1","subject":format!("use {secret}"),"status":"pending"}).to_string(),
    )
    .unwrap();
    let t = show::tap(&e, &|| named(()), &r, S).unwrap();
    assert!(!t.contains(secret) && t.contains("task 1"), "{t}");
}

#[test]
fn the_normaliser_puts_no_message_text_or_tool_data_in_any_event_whatever_the_store_policy() {
    let mut n = aibus_supervisor::Normalizer::new(aibus_trace::AttemptRef {
        workflow_run_id: uuid::Uuid::from_u128(1),
        attempt_id: uuid::Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: "x".into(),
    });
    let mut all = String::new();
    for line in read("hooks.jsonl")
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
    {
        for e in n.hook_line(line, SystemTime::now()).events {
            all.push_str(&serde_json::to_string(&e).unwrap());
        }
    }
    assert!(all.contains("tool_started"));
    for secret in [
        "SYNTHETIC-PROMPT-TEXT",
        "SYNTHETIC-SECRET-PATH",
        "SYNTHETIC-TOOL-OUTPUT",
        "SYNTHETIC-SUBAGENT-PROMPT",
    ] {
        assert!(!all.contains(secret), "{secret} is in an event");
    }
}

#[test]
fn a_task_file_that_is_a_link_is_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("cfg");
    fs::create_dir_all(cfg.join("tasks").join(S)).unwrap();
    let real = tmp.path().join("real.json");
    fs::write(
        &real,
        json!({"id":"9","subject":"via a link","status":"x"}).to_string(),
    )
    .unwrap();
    std::os::unix::fs::symlink(&real, cfg.join("tasks").join(S).join("1.json")).unwrap();
    assert!(intent::read(&cfg, S, None, &Redactor::new()).is_empty());
}
