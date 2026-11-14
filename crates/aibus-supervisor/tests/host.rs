//! The process host against stand-in CLIs (small shell scripts launched by absolute path, the way
//! the real one is): exit versus the first `result`, cancel and kill, signals, the bounded
//! environment, the prompt on stdin, stderr, and hostile output (huge lines, invalid UTF-8, streams
//! that never end, too much output). No test here runs the real Claude Code CLI.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aibus_core::ids::AccountName;
use aibus_core::Redactor;
use aibus_supervisor::{cli, supervise, Ending, Launch, Limits, Normalizer};
use aibus_trace::{
    project, AttemptStatus, EventKind, PayloadPolicy, StoreLimits, TraceEvent, TraceStore,
};
use tokio_util::sync::CancellationToken;

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        Self { _tmp: tmp, root }
    }

    /// A stand-in CLI: `body` is a POSIX shell script.
    fn cli(&self, body: &str) -> PathBuf {
        let p = self.root.join("bin/fake-cli");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn launch(&self, exe: PathBuf, limits: Limits) -> Launch {
        Launch {
            exe,
            args: vec!["-p".into(), "--output-format".into(), "stream-json".into()],
            cwd: self.root.join("work"),
            env: cli::bounded_env(Vec::<(OsString, OsString)>::new(), &[], &[]).unwrap(),
            stdin: b"the prompt".to_vec(),
            limits,
        }
    }

    async fn run(
        &self,
        launch: Launch,
        policy: PayloadPolicy,
        redactor: Redactor,
        cancel: CancellationToken,
    ) -> (aibus_supervisor::Outcome, Vec<TraceEvent>) {
        let dir = self.root.join("trace");
        let (mut store, _) =
            TraceStore::open(&dir, redactor, policy, StoreLimits::default()).unwrap();
        let mut norm = Normalizer::new(common::attempt());
        let outcome = supervise(launch, &mut norm, &mut store, cancel)
            .await
            .unwrap();
        drop(store);
        (outcome, TraceStore::read(&dir).unwrap().0)
    }

    async fn plain(
        &self,
        body: &str,
        limits: Limits,
    ) -> (aibus_supervisor::Outcome, Vec<TraceEvent>) {
        let exe = self.cli(body);
        self.run(
            self.launch(exe, limits),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            CancellationToken::new(),
        )
        .await
    }

    fn trace_dir(&self) -> PathBuf {
        self.root.join("trace")
    }
}

fn fixture(name: &str) -> String {
    common::spike_dir()
        .join(format!("{name}.stdout.jsonl"))
        .display()
        .to_string()
}

fn last(events: &[TraceEvent]) -> &TraceEvent {
    events.last().unwrap()
}

fn grep_tree(dir: &Path, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            hits.extend(grep_tree(&p, needle));
        } else if String::from_utf8_lossy(&fs::read(&p).unwrap()).contains(needle) {
            hits.push(p.display().to_string());
        }
    }
    hits
}

#[tokio::test]
async fn a_stand_in_replaying_the_foreground_run_ends_completed_with_the_full_graph() {
    let rig = Rig::new();
    let (out, events) = rig
        .plain(
            &format!("cat '{}'", fixture("11-foreground")),
            Limits::default(),
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Exited(0), AttemptStatus::Completed)
    );
    assert_eq!(events.first().unwrap().kind, EventKind::AttemptStarted);
    assert_eq!(last(&events).kind, EventKind::AttemptCompleted);
    let p = project(&events);
    assert_eq!((p.agents.len(), p.edges.len(), p.tools.len()), (3, 2, 2));
}

#[tokio::test]
async fn the_attempt_ends_at_exit_not_at_the_first_result() {
    let rig = Rig::new();
    // Three `result`s, then the process lingers, then prints one more line and exits.
    let body = format!(
        "cat '{}'\nsleep 1\necho '{{\"type\":\"system\",\"subtype\":\"status\",\"session_id\":\"late\"}}'\nexit 0",
        fixture("07-deleg")
    );
    let started = Instant::now();
    let (out, events) = rig.plain(&body, Limits::default()).await;
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the host waited for the process"
    );
    assert_eq!(out.status, AttemptStatus::Completed);
    let end = events
        .iter()
        .position(|e| e.kind == EventKind::AttemptCompleted)
        .unwrap();
    assert_eq!(end, events.len() - 1);
    assert_eq!(last(&events).data["results_seen"], 3);
}

#[tokio::test]
async fn cancel_asks_politely_first_and_the_attempt_is_cancelled() {
    let rig = Rig::new();
    let marker = rig.root.join("got-term");
    let body = format!(
        "trap 'touch {}; exit 143' TERM\nhead -n 3 '{}'\nwhile :; do sleep 0.05; done",
        marker.display(),
        fixture("11-foreground")
    );
    let exe = rig.cli(&body);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.cancel();
    });
    let (out, events) = rig
        .run(
            rig.launch(exe, Limits::default()),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Cancelled, AttemptStatus::Cancelled)
    );
    assert!(marker.exists(), "SIGTERM came before any kill");
    assert_eq!(last(&events).kind, EventKind::AttemptFailed);
    assert_eq!(last(&events).data["status"], "cancelled");
}

#[tokio::test]
async fn a_cli_that_ignores_term_is_killed_with_its_whole_process_group() {
    let rig = Rig::new();
    let pidfile = rig.root.join("grandchild.pid");
    let body = format!(
        "trap '' TERM\nsleep 300 &\necho $! > {}\nwhile :; do sleep 0.05; done",
        pidfile.display()
    );
    let exe = rig.cli(&body);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        c.cancel();
    });
    let limits = Limits {
        grace: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let (out, _) = rig
        .run(
            rig.launch(exe, limits),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(out.status, AttemptStatus::Cancelled);
    let pid = fs::read_to_string(&pidfile).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the grandchild {pid} outlived the kill");
}

#[tokio::test]
async fn a_cli_killed_by_someone_else_is_interrupted_never_completed() {
    let rig = Rig::new();
    let (out, events) = rig
        .plain(
            &format!("cat '{}'\nkill -9 $$", fixture("10-killed")),
            Limits::default(),
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Signalled(9), AttemptStatus::Interrupted)
    );
    assert!(!events.iter().any(|e| e.kind == EventKind::AttemptCompleted));
    assert_eq!(last(&events).data["status"], "interrupted");
}

#[tokio::test]
async fn a_nonzero_exit_is_failed() {
    let rig = Rig::new();
    let (out, events) = rig
        .plain(
            &format!("cat '{}'\nexit 3", fixture("06-basic")),
            Limits::default(),
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Exited(3), AttemptStatus::Failed)
    );
    assert_eq!(last(&events).data["exit_code"], 3);
}

#[tokio::test]
async fn wall_time_interrupts_a_stream_that_never_ends() {
    let rig = Rig::new();
    let limits = Limits {
        wall_time: Duration::from_millis(800),
        grace: Duration::from_millis(200),
        lines_per_second: 5_000,
        ..Limits::default()
    };
    let started = Instant::now();
    let (out, events) = rig
        .plain(
            r#"yes '{"type":"system","subtype":"status","session_id":"s"}'"#,
            limits,
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        (out.ending, out.status),
        (Ending::WallTime, AttemptStatus::Interrupted)
    );
    assert!(
        events.len() < 50,
        "known, unprojected lines leave no event each: {}",
        events.len()
    );
}

#[tokio::test]
async fn output_past_the_byte_limit_stops_the_attempt() {
    let rig = Rig::new();
    let limits = Limits {
        max_stdout_bytes: 256 * 1024,
        lines_per_second: 1_000_000,
        ..Limits::default()
    };
    let (out, events) = rig
        .plain(
            r#"yes '{"type":"system","subtype":"status","session_id":"s"}'"#,
            limits,
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::OutputLimit, AttemptStatus::Interrupted)
    );
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "output_limit"));
}

#[tokio::test]
async fn a_huge_line_without_newline_is_a_gap_and_the_next_line_still_counts() {
    let rig = Rig::new();
    let limits = Limits {
        max_line_bytes: 64 * 1024,
        ..Limits::default()
    };
    let body = format!(
        "head -c 20000000 /dev/zero | tr '\\000' 'a'\nprintf '\\n'\ncat '{}'",
        fixture("06-basic")
    );
    let (out, events) = rig.plain(&body, limits).await;
    assert_eq!(out.status, AttemptStatus::Completed);
    let gap = events
        .iter()
        .find(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "line_too_long")
        .expect("a gap");
    assert_eq!(gap.data["bytes"], 20_000_000);
    assert!(
        fs::metadata(rig.trace_dir().join("events.jsonl"))
            .unwrap()
            .len()
            < 1_000_000,
        "the line itself is nowhere in the trace"
    );
}

#[tokio::test]
async fn invalid_utf8_and_broken_json_are_gaps_and_do_not_end_the_attempt() {
    let rig = Rig::new();
    let body = format!(
        "printf '\\377\\376{{\"type\":\"x\"}}\\n'\nprintf '{{\"type\":\\n'\ncat '{}'",
        fixture("06-basic")
    );
    let (out, events) = rig.plain(&body, Limits::default()).await;
    assert_eq!(out.status, AttemptStatus::Completed);
    let reasons: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::GapDetected)
        .map(|e| e.data["reason"].as_str().unwrap().to_owned())
        .collect();
    assert!(reasons.contains(&"invalid_utf8".to_string()), "{reasons:?}");
    assert!(
        reasons.contains(&"malformed_json".to_string()),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn the_child_environment_is_exactly_the_bounded_one() {
    let rig = Rig::new();
    let out_file = rig.root.join("env.txt");
    let exe = rig.cli(&format!("env > {}", out_file.display()));
    let parent: Vec<(OsString, OsString)> = [
        ("HOME", "/home/u"),
        ("CLAUDE_CONFIG_DIR", "/home/u/.claude-b"),
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        (
            "ANTHROPIC_API_KEY",
            "sk-ant-api03-PARENTKEYPARENTKEYPARENTKEY",
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let mut launch = rig.launch(exe, Limits::default());
    launch.env = cli::bounded_env(parent, &[], &[]).unwrap();
    rig.run(
        launch,
        PayloadPolicy::MetadataOnly,
        Redactor::new(),
        CancellationToken::new(),
    )
    .await;
    let env = fs::read_to_string(&out_file).unwrap();
    let names: Vec<&str> = env
        .lines()
        .filter_map(|l| l.split('=').next())
        .filter(|n| !["PWD", "SHLVL", "_", "OLDPWD"].contains(n))
        .collect();
    let mut names = names;
    names.sort();
    assert_eq!(names, ["CLAUDE_CONFIG_DIR", "HOME", "PATH"], "{env}");
    assert!(!env.contains("PARENTKEY") && !env.contains("CLAUDECODE"));
}

#[tokio::test]
async fn the_prompt_goes_to_stdin_and_never_to_argv() {
    let rig = Rig::new();
    let (stdin_file, argv_file) = (rig.root.join("stdin.txt"), rig.root.join("argv.txt"));
    let exe = rig.cli(&format!(
        "cat > {}\necho \"$@\" > {}",
        stdin_file.display(),
        argv_file.display()
    ));
    let mut launch = rig.launch(exe, Limits::default());
    launch.stdin = b"secret plan: rename everything".to_vec();
    rig.run(
        launch,
        PayloadPolicy::MetadataOnly,
        Redactor::new(),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(stdin_file).unwrap(),
        "secret plan: rename everything"
    );
    assert!(!fs::read_to_string(argv_file)
        .unwrap()
        .contains("secret plan"));
}

#[tokio::test]
async fn a_login_prompt_on_stderr_is_auth_required_and_its_text_is_never_stored() {
    let rig = Rig::new();
    let body = "echo 'Not logged in. Please run /login or paste your API key: ' >&2\necho 'warning: something mundane' >&2\nexit 1";
    let exe = rig.cli(body);
    // Even with payload retention on, the prompt's text stays out.
    let (out, events) = rig
        .run(
            rig.launch(exe, Limits::default()),
            PayloadPolicy::Retain { max_bytes: 1 << 20 },
            Redactor::new(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(out.status, AttemptStatus::Failed);
    assert!(events.iter().any(|e| e.kind == EventKind::AuthRequired));
    assert!(grep_tree(&rig.trace_dir(), "paste your API key").is_empty());
    assert!(grep_tree(&rig.trace_dir(), "Not logged in").is_empty());
    let warn = events
        .iter()
        .find(|e| e.kind == EventKind::TraceWarning && e.data["channel"] == "stderr")
        .expect("the mundane line is a warning");
    assert!(
        warn.payload_ref.is_some(),
        "and its text is a retained payload"
    );
}

#[tokio::test]
async fn an_auth_error_result_is_auth_required_without_its_text() {
    let rig = Rig::new();
    let body = r#"echo '{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login","session_id":"s","num_turns":0}'"#;
    let exe = rig.cli(body);
    let (_, events) = rig
        .run(
            rig.launch(exe, Limits::default()),
            PayloadPolicy::Retain { max_bytes: 1 << 20 },
            Redactor::new(),
            CancellationToken::new(),
        )
        .await;
    assert!(events.iter().any(|e| e.kind == EventKind::AuthRequired));
    assert!(grep_tree(&rig.trace_dir(), "Invalid API key").is_empty());
    assert_eq!(last(&events).data["status"], "failed");
}

#[tokio::test]
async fn a_relative_executable_or_a_missing_directory_is_refused_before_anything_runs() {
    let rig = Rig::new();
    let exe = rig.cli("touch ran");
    let dir = rig.trace_dir();
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let mut norm = Normalizer::new(common::attempt());
    let mut relative = rig.launch(PathBuf::from("bin/fake-cli"), Limits::default());
    relative.cwd = rig.root.clone();
    assert!(matches!(
        supervise(relative, &mut norm, &mut store, CancellationToken::new()).await,
        Err(aibus_supervisor::SupervisorError::Refused(_))
    ));
    let mut nowhere = rig.launch(exe, Limits::default());
    nowhere.cwd = rig.root.join("missing");
    assert!(matches!(
        supervise(nowhere, &mut norm, &mut store, CancellationToken::new()).await,
        Err(aibus_supervisor::SupervisorError::Refused(_))
    ));
    assert!(!rig.root.join("ran").exists() && !rig.root.join("work/ran").exists());
}

#[tokio::test]
async fn retained_payloads_from_a_live_run_are_redacted_on_disk() {
    let rig = Rig::new();
    let canary = "sk-ant-api03-LIVECANARYLIVECANARYLIVECANARY";
    let registered = "registered-live-canary-42";
    let line = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{canary} and {registered}"}}]}},"session_id":"s","parent_tool_use_id":null}}"#
    );
    let exe = rig.cli(&format!("echo '{line}'\ncat '{}'", fixture("06-basic")));
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), registered);
    rig.run(
        rig.launch(exe, Limits::default()),
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
        redactor,
        CancellationToken::new(),
    )
    .await;
    assert!(grep_tree(&rig.trace_dir(), canary).is_empty());
    assert!(grep_tree(&rig.trace_dir(), registered).is_empty());
}
