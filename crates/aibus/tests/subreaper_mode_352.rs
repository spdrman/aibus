//! The hidden helper mode of the `aibus` binary (#352, D-352): it works when the host starts it
//! that way, and it is nothing a user can find or reach.
#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};

const MARKER: &str = "--aibus-internal-subreaper";

fn aibus() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aibus"));
    c.stdin(Stdio::null());
    c
}

#[test]
fn issue_352_the_binary_runs_a_program_under_the_helper_and_passes_its_exit_code_on() {
    let status = aibus()
        .args([MARKER, "/bin/sh", "-c", "exit 5"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(5));
}

#[test]
fn issue_352_the_mode_is_in_no_help_and_no_other_command_accepts_it() {
    let help = aibus().arg("--help").output().unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("subreaper"));
    for args in [&["runs", MARKER][..], &["run", MARKER][..], &[MARKER][..]] {
        let out = aibus().args(args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
    }
}
