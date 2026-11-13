//! `aibus run --workflow` takes its input from the argument (what a person types) or from stdin
//! (`-`, what `aibus mcp` uses so the input is not on a command line, #246). Both reach the
//! workflow as the same text, and a large input only works on stdin: one argument is capped.

mod common;

use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

use aibus_testkit::xdg_temp;
use common::{command, ndjson, stderr};
use serde_json::{json, Value};

const CHAIN: &str = r#"[
    Text("the cause is in main.rs"), Stop(EndTurn),
    Text("a small change.\nRISK: LOW"), Stop(EndTurn),
    Text("patched"), Stop(EndTurn),
    Text("tests pass"), Stop(EndTurn),
    Text("the fix is in"), Stop(EndTurn),
]"#;

/// `aibus run --json --workflow fix`, the input as `arg` or, when `stdin` is given, on stdin
/// after `-`.
fn run_fix(arg: Option<&str>, stdin: Option<&[u8]>) -> Output {
    let home = xdg_temp();
    let h = home.path();
    let init = command(h).arg("init").output().unwrap();
    assert!(init.status.success(), "{}", stderr(&init));
    fs::write(h.join("s.ron"), CHAIN).unwrap();
    let mut cmd = command(h);
    cmd.args(["run", "--json", "--workflow", "fix", "--script"])
        .arg(h.join("s.ron"));
    cmd.arg(arg.unwrap_or("-"));
    let Some(bytes) = stdin else {
        return cmd.output().unwrap();
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let bytes = bytes.to_vec();
    let feeder = std::thread::spawn(move || pipe.write_all(&bytes));
    let out = child.wait_with_output().unwrap();
    let _ = feeder.join();
    out
}

/// What the lead handed to the `workflow` tool as its input.
fn workflow_input(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
    let lines = ndjson(out);
    let call = lines
        .iter()
        .find(|l| l["type"] == "ToolCallRequested")
        .unwrap();
    assert_eq!(call["call"]["name"], "workflow");
    call["call"]["input"]["input"].clone()
}

#[test]
fn the_workflow_input_is_the_same_from_the_argument_and_from_stdin() {
    let arg = workflow_input(&run_fix(Some("login fails silently"), None));
    let piped = workflow_input(&run_fix(None, Some(b"login fails silently\n")));
    assert_eq!(arg, json!("login fails silently"));
    assert_eq!(piped, arg);
}

#[test]
fn a_one_megabyte_input_works_on_stdin() {
    let big = "0123456789abcdef".repeat(65_536);
    assert_eq!(big.len(), 1 << 20);
    let got = workflow_input(&run_fix(None, Some(big.as_bytes())));
    assert_eq!(
        got.as_str().map(str::len),
        Some(big.len()),
        "the whole input arrives"
    );
}
