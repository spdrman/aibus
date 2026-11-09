//! Plain mode and the non-TTY rules (06 sections 2 and 5): piped stdio, colour, signals.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{aibus, command, stderr};
use serde_json::Value;
use aibus_testkit::TempDir;

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn plain_mode_streams_the_text_and_keeps_tool_notices_on_stderr() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["run", "--agent", "explore", "map this repo"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Rust workspace"), "{text}");
    assert!(!text.contains("RunStarted") && !text.starts_with('{'), "{text}");
    assert!(text.ends_with('\n'));
    let err = stderr(&out);
    assert!(err.contains("aibus: tool ls ."), "{err}");
    assert!(err.contains("completed, 2 turns, 400 in"), "{err}");
    assert!(!text.contains('\u{1b}') && !err.contains('\u{1b}'));
}

#[test]
fn the_prompt_comes_from_stdin_in_plain_mode_too() {
    for args in [vec!["run", "--agent", "explore"], vec!["run", "--agent", "explore", "-"]] {
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
        assert!(stdout(&out).contains("Rust workspace"));
    }
}

#[test]
fn no_subcommand_with_stdout_piped_behaves_as_run_and_says_so() {
    let home = TempDir::new().unwrap();
    let mut child = command(home.path())
        .args(["--agent", "explore"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"map this repo\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("stdout is not a terminal, running as `aibus run`"), "{}", stderr(&out));
    assert!(stdout(&out).contains("Rust workspace"));

    // A prompt on the command line works the same, and --json still gives events.
    let out = aibus(home.path(), &["--json", "--agent", "explore", "map this repo"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).lines().next().unwrap().contains("RunStarted"));
}

#[test]
fn escape_sequences_in_the_model_text_never_reach_a_pipe() {
    let home = TempDir::new().unwrap();
    let script = home.path().join("ansi.ron");
    fs::write(&script, r#"[Text("\u{1b}[31mred\u{1b}[0m and \u{1b}]0;title\u{7}plain"), Stop(EndTurn)]"#).unwrap();
    for no_color in [None, Some("1")] {
        let mut c = command(home.path());
        c.args(["run", "--agent", "explore", "--script"]).arg(&script).arg("hi");
        if let Some(v) = no_color {
            c.env("NO_COLOR", v);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(stdout(&out).trim_end(), "red and plain", "NO_COLOR={no_color:?}");
        assert!(!stderr(&out).contains('\u{1b}'));
    }
}

#[test]
fn a_default_mode_prompt_is_refused_with_no_tty_in_plain_mode() {
    let home = TempDir::new().unwrap();
    let script = home.path().join("bash.ron");
    fs::write(&script, r#"[ToolCall(name: "bash", input: {"command": "echo hi > out.txt"}), Stop(ToolUse), Text("done")]"#).unwrap();
    let out = command(home.path())
        .args(["run", "--agent", "general", "--script"])
        .arg(&script)
        .arg("hi")
        .output()
        .unwrap();
    assert!(stderr(&out).contains("permission denied: no tty"), "{}", stderr(&out));
    assert!(!home.path().join("proj/out.txt").exists());
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
fn sighup_is_a_graceful_stop_that_exits_7_well_inside_ten_seconds() {
    let home = TempDir::new().unwrap();
    let began = Instant::now();
    let (mut child, mut out) = hanging_run(home.path());
    signal(&child, "-HUP");
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(7), "{rest}");
    let last: Value = serde_json::from_str(rest.lines().last().unwrap()).unwrap();
    assert_eq!((last["type"].as_str(), last["exit_code"].as_i64()), (Some("SessionEnd"), Some(7)));
    assert!(began.elapsed() < Duration::from_secs(9));
    // The session was closed properly: its outcome is on disk.
    let id = last["session"].as_str().unwrap();
    let json: Value = serde_json::from_str(
        &fs::read_to_string(home.path().join(".local/state/aibus/sessions").join(id).join("session.json")).unwrap(),
    )
    .unwrap();
    assert!(["cancelled", "interrupted"].contains(&json["outcome"].as_str().unwrap()), "{json}");
}

#[test]
fn sigwinch_becomes_a_resize_message() {
    let home = TempDir::new().unwrap();
    let (mut child, mut out) = hanging_run(home.path());
    signal(&child, "-WINCH");
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["type"], "Resize", "{line}");
    // No terminal here, so the fallback size.
    assert_eq!((v["cols"].as_u64(), v["rows"].as_u64()), (Some(80), Some(24)));
    signal(&child, "-INT");
    child.wait().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn term_dumb_refuses_the_tui_with_exit_2() {
    // util-linux `script` gives the binary a pseudo terminal on stdout and passes its exit code on.
    let has_script = std::process::Command::new("script").arg("--version").output().is_ok();
    if !has_script {
        eprintln!("skipped: no script(1) here");
        return;
    }
    let home = TempDir::new().unwrap();
    let bin = env!("CARGO_BIN_EXE_aibus");
    let mut child = std::process::Command::new("script")
        .env("TERM", "dumb")
        .env("HOME", home.path())
        .current_dir(home.path())
        .args(["-qec", bin, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep the pipe open until the binary is done, or script ends the session early.
    let _stdin = child.stdin.take();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}{}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("TERM=dumb"), "{}", stdout(&out));
}
