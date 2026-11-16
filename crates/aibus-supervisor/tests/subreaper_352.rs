//! The Linux subreaper helper (#352, D-352): the CLI runs under a process that is a child
//! subreaper, so every orphaned descendant (a `setsid -f` daemon whose middle process exits at
//! once included) is handed to it and ends with the attempt. These tests run the helper directly
//! (`aibus-subreaper`, the stand-in for the hidden `aibus` mode) and through `supervise`.
#![cfg(target_os = "linux")]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aibus_core::Redactor;
use aibus_supervisor::reaper::MARKER;
use aibus_supervisor::{cli, supervise, Ending, Launch, Limits, Normalizer};
use aibus_trace::{PayloadPolicy, StoreLimits, TraceStore};
use tokio_util::sync::CancellationToken;

const HELPER: &str = env!("CARGO_BIN_EXE_aibus-subreaper");

/// Perl (shipped everywhere these tests run) double-forks, starts a new session, writes its pid to
/// `$ARGV[0]` and becomes `sleep 300`. The middle process exits at once: no linger, the case the
/// #338 sweep cannot see.
const DAEMON: &str = "perl -e 'use POSIX; exit if fork; POSIX::setsid(); \
    open F, \">$ARGV[0]\"; print F $$; close F; \
    open STDIN, \"</dev/null\"; open STDOUT, \">/dev/null\"; open STDERR, \">/dev/null\"; \
    exec \"sleep\", \"300\"'";

fn alive(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

fn gone(pid: &str) -> bool {
    String::from_utf8_lossy(
        &Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .is_empty()
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn pid_from(file: &Path) -> String {
    assert!(
        wait_for(Duration::from_secs(10), || fs::read_to_string(file)
            .is_ok_and(|s| !s.is_empty())),
        "{} never appeared",
        file.display()
    );
    fs::read_to_string(file).unwrap().trim().to_string()
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// The helper as the supervisor starts it: marker first, then the program and its arguments, an
/// empty environment, stdin from /dev/null (a terminal is refused).
fn helper(program: &str, args: &[&str]) -> Command {
    let mut c = Command::new(HELPER);
    c.arg(MARKER)
        .arg(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null());
    c
}

#[test]
fn issue_352_the_exit_code_of_the_cli_is_the_exit_code_of_the_helper() {
    let status = helper("/bin/sh", &["-c", "exit 7"]).status().unwrap();
    assert_eq!(status.code(), Some(7));
}

#[test]
fn issue_352_a_cli_killed_by_a_signal_makes_the_helper_end_by_the_same_signal() {
    for (name, n) in [("KILL", 9), ("TERM", 15), ("HUP", 1)] {
        let status = helper("/bin/sh", &["-c", &format!("kill -{name} $$")])
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(n), "kill -{name}");
    }
}

#[test]
fn issue_352_the_cli_gets_the_environment_the_helper_got_and_nothing_else() {
    let out = Command::new(HELPER)
        .args([MARKER, "/usr/bin/env"])
        .env_clear()
        .env("ONLY", "this")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ONLY=this\n");
}

#[test]
fn issue_352_stdin_and_stdout_pass_through_to_the_cli() {
    let mut c = helper("/bin/cat", &[]);
    c.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"the prompt")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.stdout, b"the prompt");
}

#[test]
fn issue_352_a_program_that_is_not_absolute_is_refused_and_not_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mark = tmp.path().join("ran");
    let status = helper("sh", &["-c", &format!("touch {}", mark.display())])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert!(!mark.exists());
    let status = Command::new(HELPER)
        .arg(MARKER)
        .env_clear()
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2), "no program is a usage error too");
}

#[test]
fn issue_352_a_daemon_that_double_forked_is_ended_when_the_cli_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("daemon.pid");
    // The CLI waits until the daemon is up, then exits by itself with the daemon still running.
    let cli = script(
        tmp.path(),
        "cli.sh",
        &format!(
            "{DAEMON} {pf}\nwhile [ ! -s {pf} ]; do sleep 0.05; done\nexit 0",
            pf = pidfile.display()
        ),
    );
    let status = helper(cli.to_str().unwrap(), &[]).status().unwrap();
    assert_eq!(status.code(), Some(0));
    let pid = pid_from(&pidfile);
    assert!(
        wait_for(Duration::from_secs(2), || gone(&pid)),
        "the daemon {pid} outlived the helper"
    );
}

#[test]
fn issue_352_orphans_that_exit_are_reaped_not_left_as_zombies() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("short.pid");
    // A short-lived orphan: its parent (the subshell) exits at once, it ends after 0.3 s, and the
    // CLI keeps running well beyond that.
    let cli = script(
        tmp.path(),
        "cli.sh",
        &format!(
            "( sleep 0.3 & echo $! > {pf} )\nsleep 3\nexit 0",
            pf = pidfile.display()
        ),
    );
    let mut child = helper(cli.to_str().unwrap(), &[]).spawn().unwrap();
    let pid = pid_from(&pidfile);
    assert!(
        wait_for(Duration::from_secs(2), || gone(&pid)),
        "the orphan {pid} is still in the table (a zombie nobody collects?)"
    );
    child.kill().ok();
    child.wait().ok();
}

#[test]
fn issue_352_the_helper_and_the_whole_tree_end_when_the_supervisor_dies() {
    let tmp = tempfile::tempdir().unwrap();
    let (daemon_pid, cli_pid, helper_pid) = (
        tmp.path().join("daemon.pid"),
        tmp.path().join("cli.pid"),
        tmp.path().join("helper.pid"),
    );
    let cli = script(
        tmp.path(),
        "cli.sh",
        &format!(
            "echo $$ > {cp}\n{DAEMON} {dp}\nwhile :; do sleep 0.05; done",
            cp = cli_pid.display(),
            dp = daemon_pid.display()
        ),
    );
    // The stand-in supervisor: starts the helper in the background and waits for it.
    let sup = script(
        tmp.path(),
        "supervisor.sh",
        &format!(
            "{HELPER} {MARKER} {cli} </dev/null &\necho $! > {hp}\nwait",
            cli = cli.display(),
            hp = helper_pid.display()
        ),
    );
    let mut supervisor = Command::new(&sup).env_clear().spawn().unwrap();
    let (d, c, h) = (
        pid_from(&daemon_pid),
        pid_from(&cli_pid),
        pid_from(&helper_pid),
    );
    assert!(alive(&d) && alive(&c) && alive(&h));
    supervisor.kill().unwrap(); // SIGKILL: nothing of the supervisor's own cleanup runs
    supervisor.wait().unwrap();
    for (what, pid) in [("helper", &h), ("cli", &c), ("daemon", &d)] {
        assert!(
            wait_for(Duration::from_secs(5), || gone(pid)),
            "the {what} {pid} outlived the supervisor"
        );
    }
}

/// Runs `supervise` with the helper and a CLI script (`DIR` is its scratch directory); returns the
/// outcome and that directory.
async fn supervised(
    body: &str,
    limits: Limits,
    cancel_after: Option<Duration>,
) -> (aibus_supervisor::Outcome, PathBuf) {
    // Kept (the caller reads the pid files); the container the tests run in is thrown away.
    let root = tempfile::tempdir().unwrap().keep().canonicalize().unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    let exe = script(
        &root,
        "fake-cli",
        &body.replace("DIR", &root.display().to_string()),
    );
    let launch = Launch {
        exe,
        args: vec!["-p".into()],
        cwd: root.join("work"),
        env: cli::bounded_env(Vec::<(OsString, OsString)>::new(), &[], &[]).unwrap(),
        stdin: b"the prompt".to_vec(),
        limits,
        helper: Some(PathBuf::from(HELPER)),
    };
    let (mut store, _) = TraceStore::open(
        &root.join("trace"),
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let mut norm = Normalizer::new(common::attempt());
    let cancel = CancellationToken::new();
    if let Some(after) = cancel_after {
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            c.cancel();
        });
    }
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        supervise(launch, &mut norm, &mut store, cancel),
    )
    .await
    .expect("supervise hung")
    .unwrap();
    (out, root)
}

fn quick() -> Limits {
    Limits {
        wall_time: Duration::from_millis(1500),
        grace: Duration::from_millis(300),
        drain_stall: Duration::from_millis(300),
        drain_max: Duration::from_secs(2),
        ..Limits::default()
    }
}

#[tokio::test]
async fn issue_352_the_exit_code_of_the_cli_reaches_supervise_through_the_helper() {
    let (out, _) = supervised("cat > /dev/null\nexit 3", quick(), None).await;
    assert_eq!(out.ending, Ending::Exited(3));
}

#[tokio::test]
async fn issue_352_a_signal_death_of_the_cli_reaches_supervise_through_the_helper() {
    let (out, _) = supervised("kill -SEGV $$", quick(), None).await;
    assert_eq!(out.ending, Ending::Signalled(11));
}

#[tokio::test]
async fn issue_352_the_deadline_ends_the_whole_tree_even_for_a_cli_that_ignores_term() {
    // A daemon with no linger, a setsid grandchild of a child, and a CLI that ignores TERM.
    let body = format!(
        "trap '' TERM\n{DAEMON} DIR/daemon.pid\n\
         sh -c '{DAEMON} DIR/grandchild.pid; while :; do sleep 0.05; done' &\n\
         echo $$ > DIR/cli.pid\nwhile :; do sleep 0.05; done",
        DAEMON = DAEMON.replace('\'', "'\\''")
    );
    let started = Instant::now();
    let (out, root) = supervised(&body, quick(), None).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the helper did not end the run in time"
    );
    assert_eq!(out.status, aibus_trace::AttemptStatus::Interrupted);
    for f in ["daemon.pid", "grandchild.pid", "cli.pid"] {
        let pid = pid_from(&root.join(f));
        assert!(
            wait_for(Duration::from_secs(3), || !alive(&pid)),
            "{f}: {pid} outlived the deadline"
        );
    }
}

#[tokio::test]
async fn issue_352_a_cancel_ends_the_whole_tree() {
    let body = format!("{DAEMON} DIR/daemon.pid\nwhile :; do sleep 0.05; done");
    let limits = Limits {
        wall_time: Duration::from_secs(60),
        ..quick()
    };
    let (out, root) = supervised(&body, limits, Some(Duration::from_millis(700))).await;
    assert_eq!(out.status, aibus_trace::AttemptStatus::Cancelled);
    let pid = pid_from(&root.join("daemon.pid"));
    assert!(
        wait_for(Duration::from_secs(3), || !alive(&pid)),
        "the daemon {pid} outlived the cancel"
    );
}

#[tokio::test]
async fn issue_352_a_run_that_ends_by_itself_leaves_no_daemon() {
    let body = format!(
        "{DAEMON} DIR/daemon.pid\nwhile [ ! -s DIR/daemon.pid ]; do sleep 0.05; done\necho '{{}}'"
    );
    let limits = Limits {
        wall_time: Duration::from_secs(20),
        ..quick()
    };
    let (_, root) = supervised(&body, limits, None).await;
    let pid = pid_from(&root.join("daemon.pid"));
    assert!(
        wait_for(Duration::from_secs(3), || !alive(&pid)),
        "the daemon {pid} outlived the attempt"
    );
}
