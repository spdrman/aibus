//! `aibus run` end to end on the fake provider.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use aibus_core::replay::replay_hash;
use common::{aibus, command, ndjson, stderr};
use tempfile::TempDir;

const FAKE_KEY: &str = "fake-key-DO-NOT-LOG-7f3a9c";

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn explore_prints_the_msg_sequence_and_records_a_replayable_fixture() {
    let home = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join("proj")).unwrap();
    fs::write(home.path().join("proj/README.md"), "hello").unwrap();
    let rec = home.path().join("rec");
    let out = aibus(
        home.path(),
        &["run", "--json", "--agent", "explore", "--record", rec.to_str().unwrap(), "map this repo"],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let lines = ndjson(&out);
    let mut names: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    names.dedup();
    assert_eq!(
        names,
        [
            "RunStarted",
            "Stream",
            "UsageUpdate",
            "ToolCallRequested",
            "ToolCallFinished",
            "Stream",
            "UsageUpdate",
            "RunFinished",
            "SessionEnd"
        ]
    );
    // The run task executes its own tool call; nothing else may run it a second time.
    let count = |n: &str| lines.iter().filter(|l| l["type"] == n).count();
    assert_eq!((count("ToolCallRequested"), count("ToolCallFinished")), (1, 1));

    // Field names from 06 section 2.
    let started = &lines[0];
    assert_eq!(started["agent"], "explore");
    assert_eq!(started["model"], "fake-small");
    assert_eq!(started["account"], "fake");
    assert!(started["run"].as_str().unwrap().len() >= 26);
    assert!(started["t"].as_str().unwrap().ends_with('Z'));
    let req = lines.iter().find(|l| l["type"] == "ToolCallRequested").unwrap();
    assert_eq!(req["call"]["name"], "ls");
    assert_eq!(req["call"]["input"]["path"], ".");
    let fin = lines.iter().find(|l| l["type"] == "ToolCallFinished").unwrap();
    assert_eq!(fin["call_id"], req["call"]["id"]);
    assert_eq!(fin["ok"], true);
    assert!(fin["bytes"].as_u64().unwrap() > 0);
    assert!(fin["ms"].is_u64());
    assert!(lines.iter().any(|l| l["type"] == "Stream" && l["text"].as_str().unwrap().contains("Rust workspace")));
    let done = lines.iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["turns"], 2);
    assert_eq!(done["usage"]["input_tokens"], 400);
    assert!(done["summary"].as_str().unwrap().contains("Rust workspace"));
    let end = lines.last().unwrap();
    assert_eq!(end["exit_code"], 0);
    assert!(end["session"].as_str().unwrap().len() >= 26);

    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &stderr(&out);
    assert!(!text.contains(FAKE_KEY));

    let jsonl = fs::read_to_string(rec.join("msgs.jsonl")).unwrap();
    let hash = fs::read_to_string(rec.join("snapshot.hash")).unwrap();
    assert_eq!(replay_hash(&jsonl).unwrap(), hash.trim());
    assert!(rec.join("script.ron").is_file());
    assert_eq!(mode(&rec), 0o700);
    assert_eq!(mode(&rec.join("msgs.jsonl")), 0o600);
}

#[test]
fn bad_usage_exits_2() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    assert_eq!(aibus(h, &["run"]).status.code(), Some(2));
    assert_eq!(aibus(h, &["run", "--json", "--agent", "nope", "hi"]).status.code(), Some(3), "an unknown agent is a config error");
    // A model no account serves is exit 4 (04 section 4.1, step 7), not a usage error.
    let nobody = aibus(h, &["run", "--json", "--model", "gpt-9", "hi"]);
    assert_eq!(nobody.status.code(), Some(4));
    assert!(stderr(&nobody).contains("no account serves model gpt-9"), "{}", stderr(&nobody));
    assert_eq!(aibus(h, &["--permission-mode", "yolo", "run", "--json", "hi"]).status.code(), Some(2));
    let bypass = aibus(h, &["run", "--json", "--permission-mode", "bypass", "hi"]);
    assert_eq!(bypass.status.code(), Some(2));
    assert!(stderr(&bypass).contains("AIBUS_I_KNOW"));
}

#[test]
fn a_failing_provider_exits_5_and_says_so_on_the_last_line() {
    let home = TempDir::new().unwrap();
    let script = home.path().join("fail.ron");
    fs::write(&script, r#"[Error((kind: Server, retryable: false, message: "down"))]"#).unwrap();
    let out = aibus(
        home.path(),
        &["run", "--json", "--agent", "explore", "--script", script.to_str().unwrap(), "hi"],
    );
    assert_eq!(out.status.code(), Some(5));
    let lines = ndjson(&out);
    let done = lines.iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["status"], "failed");
    assert_eq!(lines.last().unwrap()["exit_code"], 5);
}

fn user_config(home: &Path, text: &str) {
    let dir = home.join(".config/aibus");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), text).unwrap();
}

#[test]
fn configured_hooks_run_for_the_session_and_a_block_reaches_the_tool_result() {
    let home = TempDir::new().unwrap();
    user_config(
        home.path(),
        r#"[hooks]
session_start = [{ command = "echo started-by-hook" }]
pre_tool_use = [{ matcher = "ls", command = "echo no ls today >&2; exit 2" }]
session_end = [{ command = "touch ../ended" }]
"#,
    );
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "map this repo"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let lines = ndjson(&out);
    let fin = lines.iter().find(|l| l["type"] == "ToolCallFinished").unwrap();
    assert_eq!(fin["ok"], false, "the hook blocked the call");
    assert!(lines.iter().any(|l| l["type"] == "HookFired"), "{lines:?}");
    assert!(home.path().join("ended").exists(), "session_end ran");
    // Every run of a hook leaves a log under logs/hooks/<session>.
    let session = lines.last().unwrap()["session"].as_str().unwrap();
    let logs = home.path().join(".local/state/aibus/logs/hooks").join(session);
    let mut names: Vec<String> = fs::read_dir(logs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let events: Vec<&str> = names.iter().map(|n| n.split('-').next().unwrap()).collect();
    assert!(
        ["session_start", "pre_tool_use", "session_end"].iter().all(|e| events.contains(e)),
        "{names:?}"
    );
}

#[test]
fn no_hooks_warns_and_nothing_runs() {
    let home = TempDir::new().unwrap();
    user_config(home.path(), "[hooks]\nsession_start = [{ command = \"touch ../hooked\" }]\n");
    let out = aibus(home.path(), &["run", "--json", "--no-hooks", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("--no-hooks: no hooks will run"), "{}", stderr(&out));
    assert!(!home.path().join("hooked").exists());

    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(home.path().join("hooked").exists());
}

#[test]
fn pass_env_keeps_a_secret_looking_variable_in_bash_and_without_it_the_variable_is_gone() {
    let script = r#"[ToolCall(name: "bash", input: {"command": "printf %s \"$MY_TOKEN\" > passed.txt"}), Stop(ToolUse), Text("done")]"#;
    for (config, want) in [("[hooks]\npass_env = [\"MY_TOKEN\"]\n", "hunter2-hunter2"), ("", "")] {
        let home = TempDir::new().unwrap();
        user_config(home.path(), config);
        let script_path = home.path().join("bash.ron");
        fs::write(&script_path, script).unwrap();
        let out = command(home.path())
            .env("AIBUS_I_KNOW", "1")
            .env("MY_TOKEN", "hunter2-hunter2")
            .args(["run", "--json", "--agent", "general", "--permission-mode", "bypass", "--script"])
            .arg(&script_path)
            .arg("hi")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(fs::read_to_string(home.path().join("proj/passed.txt")).unwrap(), want);
    }
}

#[test]
fn the_prompt_can_come_from_stdin() {
    for args in [vec!["run", "--json", "--agent", "explore"], vec!["run", "--json", "--agent", "explore", "-"]] {
        let home = TempDir::new().unwrap();
        let mut child = command(home.path())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"summarise README.md\n").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert_eq!(ndjson(&out).last().unwrap()["exit_code"], 0);
    }
}

#[test]
fn sessions_land_under_the_xdg_state_dir_with_private_modes() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = ndjson(&out).last().unwrap()["session"].as_str().unwrap().to_owned();

    let session = home.path().join(".local/state/aibus/sessions").join(&id);
    assert_eq!(mode(&session), 0o700);
    assert_eq!(mode(&session.join("runs")), 0o700);
    assert_eq!(mode(&session.join("session.json")), 0o600);
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(session.join("session.json")).unwrap()).unwrap();
    assert_eq!(json["id"], id.as_str());
    assert_eq!(json["lead"], "explore");
    assert_eq!(json["outcome"], "completed");
    assert_eq!(json["schema_version"], 1);
    // The state dir itself and its parents we created are private too.
    assert_eq!(mode(&home.path().join(".local/state/aibus")), 0o700);
}

#[test]
fn every_xdg_variable_moves_its_directory() {
    let home = TempDir::new().unwrap();
    let x = |n: &str| home.path().join("x").join(n);
    let out = command(home.path())
        .env("XDG_STATE_HOME", x("state"))
        .env("XDG_CONFIG_HOME", x("config"))
        .env("XDG_DATA_HOME", x("data"))
        .env("XDG_CACHE_HOME", x("cache"))
        .args(["-vv", "run", "--json", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fs::read_dir(x("state").join("aibus/sessions")).unwrap().count(), 1);
    assert!(!home.path().join(".local").exists());
    assert!(x("state").join("aibus/logs/aibus.log").is_file());
}

#[test]
fn the_log_file_is_private_and_holds_no_credential() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["-vv", "run", "--json", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let log = home.path().join(".local/state/aibus/logs/aibus.log");
    assert_eq!(mode(&log), 0o600);
    assert_eq!(mode(log.parent().unwrap()), 0o700);
    let text = fs::read_to_string(&log).unwrap();
    assert!(!text.is_empty(), "-vv should log something");
    assert!(!text.contains(FAKE_KEY), "{text}");
    // Never to the terminal.
    assert!(!stderr(&out).contains("DEBUG"));
}

#[test]
fn user_scope_memory_comes_from_the_xdg_data_dir() {
    let home = TempDir::new().unwrap();
    let data = home.path().join("d");
    let config = home.path().join("c");
    fs::create_dir_all(config.join("aibus/agents")).unwrap();
    fs::write(
        config.join("aibus/agents/mem.md"),
        "---\nname: mem\ndescription: Reads its memory.\nmemory: user\n---\nAnswer briefly.\n",
    )
    .unwrap();
    fs::create_dir_all(data.join("aibus/agent-memory/mem")).unwrap();
    fs::write(data.join("aibus/agent-memory/mem/MEMORY.md"), "remember-marker-42").unwrap();
    let script = home.path().join("s.ron");
    fs::write(
        &script,
        r#"[MatchPrompt(contains: "remember-marker-42", then: [Text("found it"), Stop(EndTurn)]), Text("missing")]"#,
    )
    .unwrap();
    let out = command(home.path())
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CONFIG_HOME", &config)
        .args(["run", "--json", "--agent", "mem", "--script", script.to_str().unwrap(), "go"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let done = ndjson(&out).into_iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["summary"], "found it");
}

#[test]
fn config_aliases_pick_the_model_and_aibus_config_points_at_the_file() {
    let home = TempDir::new().unwrap();
    let cfg = home.path().join("elsewhere.toml");
    fs::write(&cfg, "[defaults]\nmodel = \"tiny\"\n[models]\ntiny = \"fake-small\"\n").unwrap();
    let out = command(home.path())
        .env("AIBUS_CONFIG", &cfg)
        .args(["run", "--json", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(ndjson(&out)[0]["model"], "fake-small");

    // `--config` wins over the variable, and a model nobody serves is exit 4.
    let other = home.path().join("other.toml");
    fs::write(&other, "[defaults]\nmodel = \"nope\"\n").unwrap();
    let out = command(home.path())
        .env("AIBUS_CONFIG", &cfg)
        .args(["--config", other.to_str().unwrap(), "run", "--json", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
}

#[test]
fn a_bad_config_exits_3_with_the_line_and_unknown_keys_only_warn() {
    let home = TempDir::new().unwrap();
    let cfg = home.path().join("c.toml");
    fs::write(&cfg, "[limits]\nmax_turns = \"many\"\n").unwrap();
    let out = command(home.path())
        .env("AIBUS_CONFIG", &cfg)
        .args(["run", "--json", "hi"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("c.toml:2:"), "{}", stderr(&out));
    assert!(out.stdout.is_empty());

    fs::write(&cfg, "[limits]\nmax_turn = 5\n").unwrap();
    let out = command(home.path())
        .env("AIBUS_CONFIG", &cfg)
        .args(["run", "--json", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(stderr(&out).contains("c.toml:2: unknown key limits.max_turn"), "{}", stderr(&out));
}

/// Starts a run that sits in a 60 s provider delay and waits for its first event.
fn hanging_run(home: &Path) -> (std::process::Child, BufReader<std::process::ChildStdout>) {
    let script = home.join("slow.ron");
    fs::write(&script, "[Delay((secs: 60, nanos: 0)), Text(\"late\")]").unwrap();
    let mut child = command(home)
        .args(["run", "--json", "--agent", "explore", "--script", script.to_str().unwrap(), "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    assert!(first.contains("RunStarted"), "{first}");
    (child, out)
}

fn signal(child: &std::process::Child, sig: &str) {
    let ok = std::process::Command::new("kill")
        .args([sig, &child.id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

#[test]
fn sigint_cancels_the_run_and_exits_7() {
    let home = TempDir::new().unwrap();
    let began = Instant::now();
    let (mut child, mut out) = hanging_run(home.path());
    signal(&child, "-INT");
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(7), "{rest}");
    let last: serde_json::Value = serde_json::from_str(rest.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "SessionEnd");
    assert_eq!(last["exit_code"], 7);
    assert!(began.elapsed() < Duration::from_secs(9), "took the whole grace period");
}

#[test]
fn a_second_sigint_within_two_seconds_forces_the_quit() {
    let home = TempDir::new().unwrap();
    let (mut child, mut out) = hanging_run(home.path());
    signal(&child, "-INT");
    signal(&child, "-INT");
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(7));
}

#[test]
fn sigterm_is_a_graceful_stop_too() {
    let home = TempDir::new().unwrap();
    let (mut child, mut out) = hanging_run(home.path());
    signal(&child, "-TERM");
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(7));
}
