//! Exit codes 0 to 12 (06 section 3), one case each, through the real binary.

mod common;

use std::fs;
use std::path::Path;

use common::{aibus, command, ndjson, stderr};
use tempfile::TempDir;

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
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["run", "--json", "--agent", "explore", "hi"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(ndjson(&out).last().unwrap()["exit_code"], 0);

    // Nothing has run in this home: the list is empty and that is not an error.
    let fresh = TempDir::new().unwrap();
    let out = aibus(fresh.path(), &["session", "list", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
}

#[test]
fn one_is_an_io_error() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["run", "--script", "/no/such/script.ron", "hi"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
}

#[test]
fn two_is_usage() {
    let home = TempDir::new().unwrap();
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
    let home = TempDir::new().unwrap();
    user_config(home.path(), "[limits]\nmax_turns = \"many\"\n");
    let out = aibus(home.path(), &["run", "hi"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
}

#[test]
fn four_is_a_missing_credential_and_a_rejected_one() {
    let home = TempDir::new().unwrap();
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
    let home = TempDir::new().unwrap();
    let s = script(home.path(), r#"[Error((kind: Server, retryable: false, message: "down"))]"#);
    let out = aibus(home.path(), &["run", "--agent", "explore", "--script", &s, "hi"]);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
}

#[test]
fn six_is_a_prompt_a_hook_refused() {
    let home = TempDir::new().unwrap();
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
    let home = TempDir::new().unwrap();
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
    let home = TempDir::new().unwrap();
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
    let home = TempDir::new().unwrap();
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
