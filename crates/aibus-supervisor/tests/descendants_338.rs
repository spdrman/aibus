//! A supervised CLI leaves no process of its own running once the attempt is over (#338): not a
//! child that left the process group with `setsid`, not one that double-forked so its parent is
//! gone, and not one the CLI left behind when it exited by itself. The stand-in CLIs are shell
//! scripts that use perl for `setsid`, which both macOS and Linux ship.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aibus_core::Redactor;
use aibus_supervisor::{cli, supervise, Launch, Limits, Normalizer};
use aibus_trace::{AttemptStatus, PayloadPolicy, StoreLimits, TraceStore};
use tokio_util::sync::CancellationToken;

/// Starts `sleep 300` in a session of its own (`setsid`) and writes its pid to `$1`. With
/// `Some(wait)` it double-forks first: the parent lingers for `wait` seconds and exits, so the
/// child is adopted by init. Output goes to /dev/null so only the pid escape is tested.
fn detach(double: Option<f64>) -> String {
    let fork = match double {
        Some(wait) => format!("if (fork) {{ select(undef, undef, undef, {wait}); exit }}"),
        None => String::new(),
    };
    format!(
        "perl -e 'use POSIX; {fork} POSIX::setsid(); \
         open F, \">$ARGV[0]\"; print F $$; close F; \
         open STDIN, \"</dev/null\"; open STDOUT, \">/dev/null\"; open STDERR, \">/dev/null\"; \
         exec \"sleep\", \"300\"' "
    )
}

fn alive(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    // A zombie is dead, it is only waiting for whoever adopted it to collect it.
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

/// Runs `body` as the CLI and returns the pid the script wrote, once `supervise` has returned.
async fn run(
    body: &str,
    limits: Limits,
    cancel_after: Option<Duration>,
) -> (AttemptStatus, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root: PathBuf = tmp.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    let pidfile = root.join("escaped.pid");
    let exe = root.join("fake-cli");
    let body = body.replace("PIDFILE", &pidfile.display().to_string());
    fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let launch = Launch {
        exe,
        args: vec!["-p".into()],
        cwd: root.join("work"),
        env: cli::bounded_env(Vec::<(OsString, OsString)>::new(), &[], &[]).unwrap(),
        stdin: b"the prompt".to_vec(),
        limits,
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
    let pid = fs::read_to_string(&pidfile).expect("the escaped child never wrote its pid");
    (out.status, pid)
}

/// The kill is a signal, so allow a moment for the kernel before reading the state.
async fn dead_within(pid: &str, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !alive(pid)
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
async fn issue_338_a_setsid_child_does_not_outlive_the_wall_deadline() {
    let body = format!("{} PIDFILE &\nwhile :; do sleep 0.05; done", detach(None));
    let (status, pid) = run(&body, quick(), None).await;
    assert_eq!(status, AttemptStatus::Interrupted);
    assert!(
        dead_within(&pid, Duration::from_secs(3)).await,
        "the setsid child {pid} outlived the deadline"
    );
}

#[tokio::test]
async fn issue_338_a_double_forked_setsid_child_does_not_outlive_the_wall_deadline() {
    let body = format!(
        "{} PIDFILE &\nwhile :; do sleep 0.05; done",
        detach(Some(0.4))
    );
    let (_, pid) = run(&body, quick(), None).await;
    assert!(
        dead_within(&pid, Duration::from_secs(3)).await,
        "the double-forked child {pid} outlived the deadline"
    );
}

#[tokio::test]
async fn issue_338_a_deactivated_run_leaves_no_escaped_child() {
    let body = format!(
        "{} PIDFILE &\nwhile :; do sleep 0.05; done",
        detach(Some(0.4))
    );
    let limits = Limits {
        wall_time: Duration::from_secs(60),
        ..quick()
    };
    let (status, pid) = run(&body, limits, Some(Duration::from_millis(700))).await;
    assert_eq!(status, AttemptStatus::Cancelled);
    assert!(
        dead_within(&pid, Duration::from_secs(3)).await,
        "the escaped child {pid} outlived the cancel"
    );
}

#[tokio::test]
async fn issue_338_a_cli_that_ignores_term_still_loses_its_escaped_child() {
    let body = format!(
        "trap '' TERM\n{} PIDFILE &\nwhile :; do sleep 0.05; done",
        detach(Some(0.4))
    );
    let (_, pid) = run(&body, quick(), None).await;
    assert!(
        dead_within(&pid, Duration::from_secs(3)).await,
        "the escaped child {pid} outlived the kill"
    );
}

#[tokio::test]
async fn issue_338_a_child_left_behind_by_a_cli_that_exits_by_itself_is_ended_with_the_attempt() {
    // The CLI returns normally while its detached child is still running.
    let body = format!("{} PIDFILE &\nsleep 0.5\necho '{{}}'", detach(Some(0.4)));
    let limits = Limits {
        wall_time: Duration::from_secs(20),
        ..quick()
    };
    let (_, pid) = run(&body, limits, None).await;
    assert!(
        dead_within(&pid, Duration::from_secs(3)).await,
        "the child {pid} outlived the attempt that started it"
    );
}

#[tokio::test]
async fn issue_338_a_child_in_the_group_that_forks_a_setsid_grandchild_is_ended_too() {
    let body = format!(
        "sh -c '{} PIDFILE &\nwhile :; do sleep 0.05; done' &\nwhile :; do sleep 0.05; done",
        detach(Some(0.4)).replace('\'', "'\\''")
    );
    let (_, pid) = run(&body, quick(), None).await;
    assert!(
        dead_within(&pid, Duration::from_secs(3)).await,
        "the grandchild {pid} outlived the deadline"
    );
}
