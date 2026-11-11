//! Exit codes 0 to 12 (06 section 3), one case each, through the real binary.

mod common;

use std::fs;
use std::path::Path;

use common::{aibus, command, ndjson, stderr};
use aibus_testkit::xdg_temp;

fn user_config(home: &Path, text: &str) {
    let dir = home.join(".config/aibus");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), text).unwrap();
}

fn script(home: &Path, text: &str) -> String {
    let p = home.join("s.ron");
    fs::write(&p, text).unwrap();
    p.to_str().unwrap().to_owned()
}

fn code(out: &std::process::Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

#[test]
fn zero_is_a_completed_run_and_an_empty_session_list() {
    let home = xdg_temp();
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "hi"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(ndjson(&out).last().unwrap()["exit_code"], 0);

    // Nothing has run in this home: the list is empty and that is not an error.
    let fresh = xdg_temp();
    let out = aibus(fresh.path(), &["session", "list", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
}

#[test]
fn one_is_an_io_error() {
    let home = xdg_temp();
    let out = aibus(home.path(), &["run", "--script", "/no/such/script.ron", "hi"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
}

#[test]
fn two_is_usage() {
    let home = xdg_temp();
    for args in [
        vec!["run"],
        vec!["--permission-mode", "yolo", "doctor"],
        vec!["run", "--no-such-flag", "hi"],
        vec!["run", "--outcome", "great", "hi"],
    ] {
        let out = aibus(home.path(), &args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
    }
}

#[test]
fn three_is_a_bad_config() {
    let home = xdg_temp();
    user_config(home.path(), "[limits]\nmax_turns = \"many\"\n");
    let out = aibus(home.path(), &["run", "hi"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
}

#[test]
fn four_is_a_missing_credential_and_a_rejected_one() {
    let home = xdg_temp();
    let out = aibus(home.path(), &["run", "--model", "gpt-9", "hi"]);
    assert_eq!(code(&out), 4, "{}", stderr(&out));

    // The provider says 401 mid-run: the run fails with the account banner, and that is 4 too.
    let s = script(home.path(), r#"[Error((kind: Auth, retryable: false, message: "401"))]"#);
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "--script", &s, "hi"]);
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    let lines = ndjson(&out);
    assert_eq!(lines.last().unwrap()["exit_code"], 4);
    assert!(stderr(&out).contains("run aibus account test fake"), "{}", stderr(&out));
}

#[test]
fn five_is_a_provider_that_failed_after_retries() {
    let home = xdg_temp();
    let s = script(home.path(), r#"[Error((kind: Server, retryable: false, message: "down"))]"#);
    let out = aibus(home.path(), &["run", "--agent", "explore", "--script", &s, "hi"]);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
}

#[test]
fn six_is_a_prompt_a_hook_refused() {
    let home = xdg_temp();
    user_config(
        home.path(),
        "[hooks]\nuser_prompt_submit = [{ command = \"echo not today >&2; exit 2\" }]\n",
    );
    let out = aibus(home.path(), &["run", "hi"]);
    assert_eq!(code(&out), 6, "{}", stderr(&out));
    assert!(stderr(&out).contains("not today"), "{}", stderr(&out));
}

#[test]
fn seven_is_an_interrupt() {
    // SIGINT, SIGTERM and SIGHUP are exercised in run_cli.rs and nontty.rs; this is the code.
    let home = xdg_temp();
    let s = script(home.path(), "[Delay((secs: 60, nanos: 0)), Text(\"late\")]");
    let mut child = command(home.path())
        .args(["run", "--json", "--agent", "explore", "--script", &s, "hi"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut first = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.as_mut().unwrap()), &mut first).unwrap();
    assert!(first.contains("RunStarted"), "{first}");
    assert!(std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert_eq!(child.wait().unwrap().code(), Some(7));
}

#[test]
fn eight_is_a_budget_and_ten_is_everything_else_that_ended_early() {
    let home = xdg_temp();
    // 1,100 tokens against a 1,000 token budget, with a tool call still wanted.
    user_config(home.path(), "[limits]\nmax_tokens_total = 1000\n");
    let s = script(
        home.path(),
        r#"[Text("looking"), Usage(input: 600, output: 500), ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse), Text("never")]"#,
    );
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "--script", &s, "hi"]);
    assert_eq!(code(&out), 8, "{}", stderr(&out));
    let lines = ndjson(&out);
    let done = lines.iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["status"], "partial");
    assert_eq!(lines.last().unwrap()["exit_code"], 8);

    // One turn allowed and the model wants a second: partial, no error.
    user_config(home.path(), "[limits]\nmax_turns = 1\n");
    let s = script(
        home.path(),
        r#"[ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse), Text("never")]"#,
    );
    let out = aibus(home.path(), &["run", "--agent", "explore", "--script", &s, "hi"]);
    assert_eq!(code(&out), 10, "{}", stderr(&out));
}

#[test]
fn eleven_is_doctor_finding_a_problem_and_twelve_is_agent_validate() {
    let home = xdg_temp();
    let ok = aibus(home.path(), &["doctor"]);
    assert_eq!(code(&ok), 0, "{}{}", String::from_utf8_lossy(&ok.stdout), stderr(&ok));

    // A file where the data directory should be.
    fs::create_dir_all(home.path().join(".local/share")).unwrap();
    fs::write(home.path().join(".local/share/aibus"), "").unwrap();
    let bad = aibus(home.path(), &["doctor"]);
    assert_eq!(code(&bad), 11, "{}", String::from_utf8_lossy(&bad.stdout));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("FAIL"));

    let agents = home.path().join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("bad.md"), "---\nname: bad\n---\nbody\n").unwrap();
    let out = aibus(home.path(), &["agent", "validate"]);
    assert_eq!(code(&out), 12, "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn workflow_validate_exits_twelve_on_errors_and_zero_on_warnings() {
    let home = xdg_temp();
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    // A gate is a warning: the exit code stays 0.
    fs::write(dir.join("gated.toml"), "[[step]]\nagent = \"explore\"\nprompt = \"a\"\ngate = \"true\"\n").unwrap();
    let warned = aibus(home.path(), &["workflow", "validate"]);
    assert_eq!(code(&warned), 0, "{}", String::from_utf8_lossy(&warned.stdout));
    assert!(String::from_utf8_lossy(&warned.stdout).contains("warning"));

    fs::write(dir.join("bad.toml"), "[[step]]\nagent = \"ghost\"\nprompt = \"a\"\n").unwrap();
    let bad = aibus(home.path(), &["workflow", "validate"]);
    assert_eq!(code(&bad), 12, "{}", String::from_utf8_lossy(&bad.stdout));
    let one = aibus(home.path(), &["workflow", "validate", "gated"]);
    assert_eq!(code(&one), 0);
}

#[test]
fn mcp_run_status_exit_codes_follow_06_section_3() {
    use common::mcp_client::{McpClient, Protocol};
    use serde_json::json;
    // A chain that completes is 0, as `aibus run` would exit; the budget (8) and interrupt (7)
    // mappings are the child's own exit codes, carried through by `run_status`, and
    // `aibus-mcp/tests/run_tools.rs` plays them.
    let home = xdg_temp();
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ship.toml"), "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n").unwrap();
    let mut c = McpClient::start(home.path(), Protocol::V2026_07_28, &["--allow", "run"]);
    let started = c.call("workflow_run", json!({ "name": "ship", "scope": "project", "input": "look" })).unwrap();
    let run = started["structuredContent"]["run"].as_str().unwrap().to_owned();
    for _ in 0..120 {
        let r = c.call("run_status", json!({ "run": run, "wait_ms": 5000 })).unwrap();
        let body = &r["structuredContent"];
        // The exit code comes off the child's last event, a moment before the status settles.
        if !body["exit_code"].is_null() && !matches!(body["status"].as_str(), Some("starting" | "running")) {
            assert_eq!(body["exit_code"], 0, "{body}");
            assert_eq!(body["status"], "completed", "{body}");
            return;
        }
    }
    panic!("the run never finished");
}
