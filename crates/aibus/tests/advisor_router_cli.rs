//! B2.3 headless: `aibus run --json` shows the advisor and the router at work (the M4 exit,
//! headless half). The built-in fake serves the lead, the advisor and the router from one script,
//! in the order they ask.

mod common;

use std::fs;

use common::schema::assert_valid;
use common::{command, ndjson, stderr};
use aibus_testkit::TempDir;

const SCRIPT: &str = r##"[
    // the lead: a plan that ends on an intention
    Text("# Plan\n1. Now I'll list the files."), Stop(EndTurn),
    // the advisor, at before_plan
    Text("plan skips auth"), Stop(EndTurn),
    // the router, at which_tool
    RouterAnswer(choice: "ls", p: 0.95), Stop(EndTurn),
    // the lead again, with the advice and the tool hint in front of it
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("done"),
]"##;

fn run(home: &TempDir, extra: &[&str]) -> std::process::Output {
    let script = home.path().join("script.ron");
    fs::write(&script, SCRIPT).unwrap();
    command(home.path())
        .args(["run", "--json", "--script"])
        .arg(&script)
        .args(extra)
        .arg("plan the work")
        .output()
        .unwrap()
}

#[test]
fn both_events_show_up_in_the_json_output() {
    let home = TempDir::new().unwrap();
    let out = run(&home, &["--advisor", "fake-large", "--router", "fake-small"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);
    for l in &lines {
        assert_valid("run-event", l);
    }
    let spoke = lines.iter().find(|l| l["type"] == "AdvisorSpoke").expect("AdvisorSpoke");
    assert_eq!(spoke["checkpoint"], "before_plan");
    assert_eq!(spoke["advice"], "plan skips auth");
    assert!(spoke["tokens_read"].as_u64().unwrap() > 0);
    let routed = lines.iter().find(|l| l["type"] == "RouterAnswered").expect("RouterAnswered");
    assert_eq!(routed["kind"], "which_tool");
    assert_eq!(routed["choice"], "ls");
    assert_eq!(routed["verdict"], "sharp");
    assert_eq!(routed["fork"], 1);
}

#[test]
fn off_flags_leave_them_out_with_a_note() {
    let home = TempDir::new().unwrap();
    // With both off the script's advisor and router steps go to the lead instead, so the run
    // may end differently; what matters is that neither event appears and the notes are said.
    let out = run(&home, &["--advisor", "off", "--router", "off"]);
    let lines = ndjson(&out);
    assert!(lines.iter().all(|l| l["type"] != "AdvisorSpoke" && l["type"] != "RouterAnswered"));
    let err = stderr(&out);
    assert!(err.contains("the advisor is off"), "{err}");
    assert!(err.contains("the router is off"), "{err}");
}
