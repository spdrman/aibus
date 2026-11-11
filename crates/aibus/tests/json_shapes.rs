//! Every `--json` output of 06 section 4 that exists, validated against `docs/schemas/cli/`.
//! `handoff` and `memory recall` are checked in `continuity_cli.rs`, next to the commands'
//! other behaviour.

mod common;

use std::fs;

use common::schema::{assert_valid, check};
use common::{aibus, command, ndjson, stderr};
use serde_json::{json, Value};
use aibus_testkit::xdg_temp;

fn out_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn the_checker_catches_what_the_schemas_ask_for() {
    assert!(check("doctor", &json!({"checks": [{"name": "a", "ok": true, "detail": "x"}]})).is_empty());
    assert!(!check("doctor", &json!({"checks": []})).is_empty());
    assert!(!check("doctor", &json!({"checks": [{"name": "a", "ok": "yes", "detail": "x"}]})).is_empty());
    assert!(!check("session-list", &json!([{"id": "x"}])).is_empty());
    let finished = json!({"type": "RunFinished", "t": "2026-11-08T00:00:00.000Z"});
    assert!(!check("run-event", &finished).is_empty(), "missing fields must fail");
    // An event of a type the schema does not list only needs `type` and `t`.
    assert!(check("run-event", &json!({"type": "HookFired", "t": "2026-11-08T00:00:00.000Z"})).is_empty());
    assert!(!check("run-event", &json!({"type": "HookFired", "t": "yesterday"})).is_empty());
    assert!(!check("account-test", &json!({"name": "a", "ok": false})).is_empty());
}

#[test]
fn every_event_of_a_run_validates_and_the_listed_ones_carry_their_fields() {
    let home = xdg_temp();
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "map this repo"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);
    for l in &lines {
        assert_valid("run-event", l);
    }
    for kind in ["RunStarted", "Stream", "ToolCallRequested", "ToolCallFinished", "RunFinished", "SessionEnd"] {
        assert!(lines.iter().any(|l| l["type"] == kind), "no {kind}");
    }
}

#[test]
fn the_events_the_fake_run_does_not_produce_validate_too() {
    // The shapes of 06 section 2 that need a router, an advisor or a permission prompt.
    for e in [
        json!({"type":"RouterAnswered","t":"2026-11-07T23:48:05.012Z","fork":1315,"kind":"which_file","choice":"src/auth.ts","p":0.94,"verdict":"sharp"}),
        json!({"type":"AdvisorSpoke","t":"2026-11-07T23:48:05.012Z","checkpoint":"before_plan","advice":"plan skips auth","tokens_read":122000}),
        json!({"type":"PermissionNeeded","t":"2026-11-07T23:48:05.012Z","id":"01JC0000000000000000000000","run":"01JC0000000000000000000001","tool":"bash","input":{"command":"cargo test"}}),
        json!({"type":"PermissionDecided","t":"2026-11-07T23:48:05.012Z","id":"01JC0000000000000000000000","decision":"denied","reason":"no tty"}),
    ] {
        assert_valid("run-event", &e);
    }
}

#[test]
fn a_tool_input_over_result_max_bytes_is_not_inlined() {
    let home = xdg_temp();
    fs::create_dir_all(home.path().join(".config/aibus")).unwrap();
    fs::write(home.path().join(".config/aibus/config.toml"), "[limits]\nresult_max_bytes = 256\n").unwrap();
    let big = "x".repeat(2000);
    let script = home.path().join("big.ron");
    fs::write(
        &script,
        format!(r#"[ToolCall(name: "ls", input: {{"path": "{big}"}}), Stop(ToolUse), Text("done")]"#),
    )
    .unwrap();
    let out = command(home.path())
        .args(["run", "--json", "--agent", "explore", "--script"])
        .arg(&script)
        .arg("go")
        .output()
        .unwrap();
    let lines = ndjson(&out);
    let req = lines.iter().find(|l| l["type"] == "ToolCallRequested").unwrap();
    assert_eq!(req["call"]["input"]["truncated"], true);
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&big));
    for l in &lines {
        assert!(l.to_string().len() < 1500, "{l}");
    }
}

#[test]
fn agent_commands_validate() {
    let home = xdg_temp();
    let list = aibus(home.path(), &["agent", "list", "--json"]);
    assert_valid("agent-list", &out_json(&list));
    let show = aibus(home.path(), &["agent", "show", "explore", "--json"]);
    assert_valid("agent-show", &out_json(&show));
    let validate = aibus(home.path(), &["agent", "validate", "--json"]);
    assert_valid("agent-validate", &out_json(&validate));

    let agents = home.path().join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("bad.md"), "---\nname: bad\n---\n").unwrap();
    let validate = aibus(home.path(), &["agent", "validate", "--json"]);
    assert_eq!(validate.status.code(), Some(12));
    assert_valid("agent-validate", &out_json(&validate));
}

#[test]
fn workflow_validate_and_show_match_their_schemas() {
    let home = xdg_temp();
    let init = aibus(home.path(), &["init"]);
    assert!(init.status.success(), "{}", stderr(&init));
    let show = aibus(home.path(), &["workflow", "show", "fix", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let v = out_json(&show);
    assert_valid("workflow-show", &v);
    assert_eq!(v["steps"][1]["agent"], "plan");
    assert_eq!(v["steps"][3]["on_fail"], "retry:2");

    // The four examples carry a gate warning or two, and no errors.
    let ok = aibus(home.path(), &["workflow", "validate", "--json"]);
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));
    let v = out_json(&ok);
    assert_valid("workflow-validate", &v);
    assert_eq!(v["ok"], true);
    assert!(!v["warnings"].as_array().unwrap().is_empty());

    let dir = home.path().join("proj/.aibus/workflows");
    fs::write(dir.join("bad.toml"), "[[step]]\nagent = \"ghost\"\nprompt = \"x\"\n").unwrap();
    let bad = aibus(home.path(), &["workflow", "validate", "bad", "--json"]);
    assert_eq!(bad.status.code(), Some(12));
    let v = out_json(&bad);
    assert_valid("workflow-validate", &v);
    assert_eq!(v["errors"][0]["step"], 1);
    assert!(v["errors"][0]["message"].as_str().unwrap().contains("ghost"));
}

#[test]
fn account_commands_validate() {
    let home = xdg_temp();
    let key = "sk-cli-0123456789abcdef";
    let with_key = |args: &[&str]| command(home.path()).env("WORK_KEY", key).args(args).output().unwrap();
    let add = with_key(&["account", "add", "work", "--provider", "fake", "--env", "WORK_KEY", "--label", "Work"]);
    assert!(add.status.success(), "{}", stderr(&add));
    let list = with_key(&["account", "list", "--json"]);
    assert_valid("account-list", &out_json(&list));
    let test = with_key(&["account", "test", "work", "--json"]);
    assert_valid("account-test", &out_json(&test));
    assert_eq!(out_json(&test)["ok"], true);
    // Without the key: the failure shape, with the error's kind.
    let failed = aibus(home.path(), &["account", "test", "work", "--json"]);
    assert_eq!(failed.status.code(), Some(4));
    let v = out_json(&failed);
    assert_valid("account-test", &v);
    assert_eq!(v["error"]["kind"], "auth");
    assert!(!String::from_utf8_lossy(&list.stdout).contains(key));
}

#[test]
fn session_commands_validate_after_a_run() {
    let home = xdg_temp();
    let run = aibus(home.path(), &["run", "--json", "--agent", "explore", "--topic", "mapping", "hi"]);
    assert!(run.status.success(), "{}", stderr(&run));
    let id = ndjson(&run).last().unwrap()["session"].as_str().unwrap().to_owned();

    let list = aibus(home.path(), &["session", "list", "--json"]);
    let rows = out_json(&list);
    assert_valid("session-list", &rows);
    assert_eq!(rows[0]["id"], id.as_str());
    assert_eq!(rows[0]["topic"], "mapping");
    assert_eq!(rows[0]["runs"], 1);
    assert_eq!(rows[0]["outcome"], "completed");
    assert!(rows[0]["cost_usd"].as_f64().unwrap() >= 0.0);

    let names = aibus(home.path(), &["session", "list", "--names"]);
    assert_eq!(String::from_utf8_lossy(&names.stdout).trim(), id);

    let show = aibus(home.path(), &["session", "show", &id, "--json"]);
    let shown = out_json(&show);
    assert_valid("session-show", &shown);
    assert_eq!(shown["runs"][0]["agent"], "explore");

    let log = aibus(home.path(), &["session", "log", &id, "--json"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(text.lines().count() > 3, "{text}");
    for line in text.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        assert_valid("session-log-line", &v);
    }
    assert!(text.contains("\"intent\":\"RunStarted\""));

    // A name that is not a session id never becomes a path.
    let bad = aibus(home.path(), &["session", "show", "../../etc", "--json"]);
    assert_eq!(bad.status.code(), Some(2));
    let missing = aibus(home.path(), &["session", "show", "01JC0000000000000000000000"]);
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn doctor_and_config_show_validate() {
    let home = xdg_temp();
    let doctor = aibus(home.path(), &["doctor", "--json"]);
    assert_eq!(doctor.status.code(), Some(0), "{}", String::from_utf8_lossy(&doctor.stdout));
    let v = out_json(&doctor);
    assert_valid("doctor", &v);
    let names: Vec<&str> = v["checks"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    for want in ["config", "credential store", "shell sandbox", "worktrees", "index", "xdg dirs"] {
        assert!(names.contains(&want), "{names:?}");
    }
    let by = |n: &str| v["checks"].as_array().unwrap().iter().find(|c| c["name"] == n).unwrap().clone();
    assert_eq!(by("shell sandbox")["detail"], "none");
    // No keychain in a container: the spec's wording; on a desktop it names the keyring.
    let store = by("credential store")["detail"].as_str().unwrap().to_owned();
    assert!(store == "unavailable, env/file only" || store.starts_with("keyring ("), "{store}");

    let config = aibus(home.path(), &["config", "show", "--json"]);
    assert_valid("config-show", &out_json(&config));
}
