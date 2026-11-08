//! B1.2: the `bash` tool and the allow-list that guards it. Everything here starts real
//! processes, so it runs in the container only.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibus_core::tools::Bash;
use aibus_core::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn pat(s: &str) -> ToolPattern {
    s.parse().unwrap()
}

fn eff(allow: &[&str]) -> EffectiveTools {
    EffectiveTools {
        allow: allow.iter().map(|s| pat(s)).collect(),
        deny: vec![],
    }
}

fn cx(cwd: &Path) -> ToolCx {
    ToolCx {
        run: RunId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
    }
}

fn host_env() -> Vec<(String, String)> {
    ["PATH", "HOME", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect()
}

fn rig(dir: &Path, timeout: u32, pass_env: &[&str], extra_env: &[(&str, &str)]) -> (ToolExecutor, Arc<Bash>) {
    let mut env = host_env();
    env.extend(extra_env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    let bash = Arc::new(Bash::new(
        timeout,
        pass_env.iter().map(|s| (*s).to_owned()).collect(),
        env,
        dir.to_path_buf(),
        "sess-1".into(),
    ));
    (ToolExecutor::new(ToolRegistry::standard(bash.clone())), bash)
}

async fn run(
    ex: &ToolExecutor,
    e: &EffectiveTools,
    c: &ToolCx,
    input: Value,
) -> Result<ToolOutput, ToolError> {
    ex.call_gated(e, c, PermissionMode::Bypass, "bash", input, |_| async { Decision::Deny })
        .await
}

fn denied_with(r: &Result<ToolOutput, ToolError>, needle: &str) -> bool {
    matches!(r, Err(ToolError::Denied { reason }) if reason.contains(needle))
}

#[tokio::test]
async fn compound_commands_cannot_smuggle_a_denied_part_past_an_allow_glob() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let e = eff(&["bash(echo *)"]);
    // Each of these would create PWN if any part after the first reached the shell.
    let smuggled = [
        "echo a; touch PWN",
        "echo a;touch PWN",
        "echo a && touch PWN",
        "echo a || touch PWN",
        "echo a | touch PWN",
        "echo a |& touch PWN",
        "echo a & touch PWN",
        "echo a &\ntouch PWN",
        "echo a\ntouch PWN",
        "echo a\n\n  touch PWN",
        "echo a;;touch PWN",
        "echo $(touch PWN)",
        "echo \"$(touch PWN)\"",
        "echo `touch PWN`",
        "echo \"`touch PWN`\"",
        "echo $(echo $(touch PWN))",
        "(touch PWN)",
        "echo a; (touch PWN)",
        "(echo a) && (touch PWN)",
        "echo <(touch PWN)",
        "echo a >(touch PWN)",
        "echo \\>| touch PWN",
        "echo a 2>&1 | touch PWN",
        "echo a >| out; touch PWN",
        "touch PWN",
        "touch PWN # echo ok",
        "touch PWN\necho ok",
        "{ touch PWN; }",
        "echo ok && { touch PWN; }",
        "if true; then touch PWN; fi",
        "echo ok; ! touch PWN",
        "echo ok; time touch PWN",
        "echo ok; exec touch PWN",
        "echo ok; env touch PWN",
    ];
    for cmd in smuggled {
        let r = run(&ex, &e, &cx(dir.path()), json!({ "command": cmd })).await;
        assert!(denied_with(&r, "not in allow-list"), "{cmd:?}: {r:?}");
        assert!(!dir.path().join("PWN").exists(), "{cmd:?} ran its second part");
    }
}

#[tokio::test]
async fn ambiguous_commands_are_denied_even_for_a_bare_allow() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let e = eff(&["bash"]);
    let ambiguous = [
        "echo 'unterminated; touch PWN",
        "echo \"unterminated; touch PWN",
        "echo $(echo a; touch PWN",
        "echo `echo a; touch PWN",
        "echo ok)",
        "echo a\\",
        "cat <<EOF\ntouch PWN\nEOF",
        "echo $((1 + $(touch PWN)))",
        "echo $'\\x27'; touch PWN",
        "echo `echo \\`touch PWN\\``",
        "$CMD PWN",
        "\"$CMD\" PWN",
        "${CMD} PWN",
        "echo ok; $CMD PWN",
        "PATH=/tmp/evil echo hi",
        "LD_PRELOAD=/tmp/x.so echo hi",
        "FOO=$(touch PWN) echo hi",
        "echo a\0b",
        "echo\u{a0}a",
        "echo a\r\ntouch PWN",
    ];
    for cmd in ambiguous {
        let r = run(&ex, &e, &cx(dir.path()), json!({ "command": cmd })).await;
        assert!(denied_with(&r, "ambiguous command"), "{cmd:?}: {r:?}");
        assert!(!dir.path().join("PWN").exists(), "{cmd:?} ran");
    }
}

#[tokio::test]
async fn commands_made_only_of_allowed_parts_run() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let e = eff(&["bash(echo *)", "bash(cat *)"]);
    let table = [
        ("echo one", "one"),
        ("echo one; echo two", "one\ntwo"),
        ("echo one && echo two", "one\ntwo"),
        ("echo $(echo nested)", "nested"),
        ("echo \"a; touch PWN\"", "a; touch PWN"),
        ("echo 'x && touch PWN'", "x && touch PWN"),
        ("echo a\\;b", "a;b"),
        ("echo one 2>&1", "one"),
        ("echo one | cat -", "one"),
        ("echo a > f.txt; cat f.txt", "a"),
        ("echo a >| g.txt; cat g.txt", "a"),
        ("echo \"line1\nline2\"", "line1\nline2"),
        ("echo a \\\n b", "a b"),
    ];
    for (cmd, want) in table {
        let r = run(&ex, &e, &cx(dir.path()), json!({ "command": cmd })).await;
        assert_eq!(r.unwrap().content.trim_end(), want, "{cmd:?}");
    }
    assert!(!dir.path().join("PWN").exists());
}

#[tokio::test]
async fn a_deny_specifier_catches_any_part_and_the_destructive_list_comes_first() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let both = EffectiveTools {
        allow: vec![pat("bash")],
        deny: vec![pat("bash(git push *)")],
    };
    for cmd in [
        "echo ok; git push origin x",
        "echo ok && git   push origin x",
        "echo $(git push origin x)",
    ] {
        let r = run(&ex, &both, &cx(dir.path()), json!({ "command": cmd })).await;
        assert!(denied_with(&r, "denied by tool policy"), "{cmd:?}: {r:?}");
    }

    let destructive = [
        "rm -rf /",
        "echo hi; rm -rf /",
        "echo hi && sudo rm -rf ~",
        "rm -rf *",
        "echo $(rm -rf /)",
        "git push --force origin main",
        "git push -f origin master",
        "git reset --hard",
        ":(){ :|:& };:",
        "mkfs.ext4 /dev/sda1",
        "dd if=/dev/zero of=/dev/sda",
    ];
    for cmd in destructive {
        let r = run(&ex, &eff(&["bash"]), &cx(dir.path()), json!({ "command": cmd })).await;
        assert!(denied_with(&r, "shell deny pattern"), "{cmd:?}: {r:?}");
    }
    // The list is checked before the prompt too, so even a call the user would approve is refused.
    let r = ex
        .call_gated(
            &eff(&["bash"]),
            &cx(dir.path()),
            PermissionMode::Default,
            "bash",
            json!({"command": "rm -rf /"}),
            |_| async { panic!("the prompt must not be reached") },
        )
        .await;
    assert!(denied_with(&r, "rm-root"), "{r:?}");
}

#[tokio::test]
async fn config_deny_patterns_extend_the_list() {
    let dir = TempDir::new().unwrap();
    let bash = Arc::new(Bash::new(20, vec![], host_env(), dir.path().into(), "s".into()));
    let ex = ToolExecutor::with_gate(
        ToolRegistry::standard(bash),
        Gate {
            deny_patterns: vec!["no-echo-secret: echo secret*".into()],
            ..Gate::default()
        },
    )
    .unwrap();
    let r = run(&ex, &eff(&["bash"]), &cx(dir.path()), json!({"command": "echo secret1"})).await;
    assert!(denied_with(&r, "no-echo-secret"), "{r:?}");
    let ok = run(&ex, &eff(&["bash"]), &cx(dir.path()), json!({"command": "echo fine"})).await;
    assert_eq!(ok.unwrap().content.trim_end(), "fine");

    let bad = Gate {
        deny_patterns: vec!["oops: [".into()],
        ..Gate::default()
    };
    assert!(ToolExecutor::with_gate(ToolRegistry::read_only(), bad).is_err());
}

#[tokio::test]
async fn the_environment_is_scrubbed_and_the_aibus_variables_are_set() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(
        dir.path(),
        20,
        &["PASS_ME_KEY"],
        &[
            ("OPENAI_API_KEY", "val-openai"),
            ("GITHUB_TOKEN", "val-github"),
            ("DB_SECRET", "val-dbsecret"),
            ("DB_PASSWORD", "val-dbpass"),
            ("lower_api_key", "val-lower"),
            ("TOKEN", "val-token"),
            ("AIBUS_ACCOUNT_WORK_KEY", "val-acct-key"),
            ("AIBUS_ACCOUNT_WORK", "val-acct-bare"),
            ("PASS_ME_KEY", "kept"),
            ("KEEP_ME", "kept-too"),
            ("KEYBOARD", "kept-as-well"),
        ],
    );
    let c = cx(dir.path());
    let out = run(&ex, &eff(&["bash"]), &c, json!({"command": "env"})).await.unwrap().content;
    for gone in ["val-openai", "val-github", "val-dbsecret", "val-dbpass", "val-lower", "val-token", "val-acct-key", "val-acct-bare"] {
        assert!(!out.contains(gone), "{gone} leaked into the environment:\n{out}");
    }
    for kept in ["PASS_ME_KEY=kept", "KEEP_ME=kept-too", "KEYBOARD=kept-as-well"] {
        assert!(out.lines().any(|l| l == kept), "{kept} missing:\n{out}");
    }
    assert!(out.lines().any(|l| l == format!("AIBUS_PROJECT_DIR={}", dir.path().display())));
    assert!(out.lines().any(|l| l == "AIBUS_SESSION_ID=sess-1"));
    assert!(out.lines().any(|l| l == format!("AIBUS_RUN_ID={}", c.run)));
}

#[tokio::test]
async fn the_working_directory_is_the_runs_and_both_streams_are_captured() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let wt = dir.path().join("sub");
    let out = run(&ex, &eff(&["bash"]), &cx(&wt), json!({"command": "pwd; echo out; echo err 1>&2; exit 3"}))
        .await
        .unwrap()
        .content;
    let canon = wt.canonicalize().unwrap();
    assert!(out.starts_with(&format!("{}\n", canon.display())), "{out}");
    assert!(out.contains("out") && out.contains("err"), "{out}");
    assert!(out.ends_with("[exit status 3]"), "{out}");
}

#[tokio::test]
async fn the_timeout_arg_and_the_config_default_end_a_slow_command() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 1, &[], &[]);
    let e = eff(&["bash"]);
    let t0 = Instant::now();
    let r = run(&ex, &e, &cx(dir.path()), json!({"command": "sleep 30"})).await;
    assert!(matches!(r, Err(ToolError::Timeout)), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());

    let (ex, _) = rig(dir.path(), 60, &[], &[]);
    let t0 = Instant::now();
    let r = run(&ex, &e, &cx(dir.path()), json!({"command": "sleep 30", "timeout": 1})).await;
    assert!(matches!(r, Err(ToolError::Timeout)), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(5));

    let r = run(&ex, &e, &cx(dir.path()), json!({"command": "echo hi", "timeout": 0})).await;
    assert!(matches!(r, Err(ToolError::Invalid(_))), "{r:?}");
}

/// Running and not a zombie (an orphan nobody has reaped yet is already dead).
fn alive(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|s| s.rsplit(')').next().is_some_and(|rest| !rest.trim_start().starts_with('Z')))
}

#[tokio::test]
async fn cancelling_kills_the_process_group_including_grandchildren() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 120, &[], &[]);
    let c = cx(dir.path());
    let cancel = c.cancel.clone();
    // A grandchild (the inner sleep) that would outlive a kill of the shell alone.
    let cmd = "sh -c 'sleep 60 & echo $! > grandchild.pid; wait'";
    let e = eff(&["bash"]);
    let call = run(&ex, &e, &c, json!({ "command": cmd }));
    tokio::pin!(call);
    let pid_file = dir.path().join("grandchild.pid");
    tokio::select! {
        _ = &mut call => panic!("should still be running"),
        () = async { while !pid_file.exists() { tokio::time::sleep(Duration::from_millis(20)).await } } => {}
    }
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    assert!(alive(&pid), "the grandchild should be running before the cancel");
    cancel.cancel();
    let r = tokio::time::timeout(Duration::from_secs(2), call).await.expect("returned in time");
    assert!(matches!(r, Err(ToolError::Cancelled)), "{r:?}");
    let t0 = Instant::now();
    while alive(&pid) && t0.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid), "the grandchild survived the cancel");
}

#[tokio::test]
async fn dropping_the_call_kills_the_group_too() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 120, &[], &[]);
    let c = cx(dir.path());
    let e = eff(&["bash"]);
    let pid_file = dir.path().join("p.pid");
    let call = run(&ex, &e, &c, json!({"command": "sleep 60 & echo $! > p.pid; wait"}));
    // The executor drops the future when it loses the race with a cancel; dropping is all it does.
    let _ = tokio::time::timeout(Duration::from_millis(600), call).await;
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    let t0 = Instant::now();
    while alive(&pid) && t0.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid), "the background child survived a dropped call");
}

#[tokio::test]
async fn a_foreground_call_does_not_wait_for_a_backgrounded_child() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 120, &[], &[]);
    let t0 = Instant::now();
    let out = run(&ex, &eff(&["bash"]), &cx(dir.path()), json!({"command": "sleep 30 & echo started"}))
        .await
        .unwrap();
    assert_eq!(out.content.trim_end(), "started");
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
}

#[tokio::test]
async fn output_is_capped_to_its_tail() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let mut c = cx(dir.path());
    c.limits.bash_max_output_bytes = 1000;
    let out = run(&ex, &eff(&["bash"]), &c, json!({"command": "seq 1 100000"})).await.unwrap();
    assert!(out.truncated);
    assert!(out.content.len() <= 1000, "{}", out.content.len());
    assert!(out.content.starts_with("[... "), "{}", &out.content[..40]);
    assert!(out.content.trim_end().ends_with("100000"));
    assert_eq!(out.content.matches("bytes truncated").count(), 1);

    let small = run(&ex, &eff(&["bash"]), &c, json!({"command": "echo hi"})).await.unwrap();
    assert!(!small.truncated);
}

#[tokio::test]
async fn background_returns_a_handle_and_the_job_can_be_read_and_stopped() {
    let dir = TempDir::new().unwrap();
    let (ex, bash) = rig(dir.path(), 20, &[], &[]);
    let e = eff(&["bash"]);
    let started = run(&ex, &e, &cx(dir.path()), json!({"command": "echo bg-out; sleep 0.3", "background": true}))
        .await
        .unwrap();
    let handle = serde_json::from_str::<Value>(&started.content).unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(handle, "bg-1");
    let t0 = Instant::now();
    let done = loop {
        let v = bash.job(&handle).unwrap();
        if !v.running || t0.elapsed() > Duration::from_secs(5) {
            break v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(!done.running, "{done:?}");
    assert_eq!(done.exit_code, Some(0));
    assert!(done.output.contains("bg-out"));
    assert!(bash.job("bg-99").is_none());

    // A long job survives the call that started it and dies on stop_job.
    let pid_file = dir.path().join("long.pid");
    let started = run(&ex, &e, &cx(dir.path()), json!({"command": "sleep 60 & echo $! > long.pid; wait", "background": true}))
        .await
        .unwrap();
    let handle = serde_json::from_str::<Value>(&started.content).unwrap()["handle"].as_str().unwrap().to_owned();
    let t0 = Instant::now();
    while !pid_file.exists() && t0.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    assert!(alive(&pid));
    assert!(bash.job(&handle).unwrap().running);
    assert!(bash.stop_job(&handle));
    let t0 = Instant::now();
    while alive(&pid) && t0.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid));
    assert!(!bash.stop_job("nope"));
}

#[tokio::test]
async fn dropping_the_tool_stops_background_jobs() {
    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("d.pid");
    {
        let (ex, _bash) = rig(dir.path(), 20, &[], &[]);
        run(&ex, &eff(&["bash"]), &cx(dir.path()), json!({"command": "sleep 60 & echo $! > d.pid; wait", "background": true}))
            .await
            .unwrap();
        let t0 = Instant::now();
        while !pid_file.exists() && t0.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    let t0 = Instant::now();
    while alive(&pid) && t0.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid));
}

#[tokio::test]
async fn bad_input_is_invalid_not_a_panic() {
    let dir = TempDir::new().unwrap();
    let (ex, _) = rig(dir.path(), 20, &[], &[]);
    let e = eff(&["bash"]);
    for input in [json!({}), json!({"command": 5}), json!({"command": "   "}), json!({"command": "ls", "bogus": 1})] {
        let r = run(&ex, &e, &cx(dir.path()), input.clone()).await;
        assert!(matches!(r, Err(ToolError::Invalid(_))), "{input}: {r:?}");
    }
}
