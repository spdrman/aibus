//! `workflow_run`, `run_status` and `run_cancel` (#104), against a stand-in for `aibus run` that
//! plays a scripted chain (`common::fake_aibus`). The real binary is exercised in
//! `crates/aibus/tests/mcp_server.rs`.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aibus_core::Redactor;
use aibus_mcp::{Caps, RunConfig, Server};
use aibus_testkit::Xdg;
use serde_json::{json, Value};

const FLOW: &str = "description = \"Plan, then build\"\n\n\
    [[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n\n\
    [[step]]\nagent = \"general\"\nprompt = \"Build it: {{prev.summary}}\"\n";

struct Rig {
    server: Server,
    home: Xdg,
    fake: PathBuf,
    rt: tokio::runtime::Runtime,
}

fn rig_with(tune: impl FnOnce(&mut RunConfig)) -> Rig {
    let (server, home) = common::server(Caps::new(false, false, true, vec!["bash(cargo *)".into()]).unwrap(), Redactor::new());
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ship.toml"), FLOW).unwrap();
    let fake_dir = home.path().join("fake");
    fs::create_dir_all(&fake_dir).unwrap();
    let fake = common::fake_aibus(&fake_dir);
    let mut cfg = RunConfig::new(fake.clone(), 20.0);
    tune(&mut cfg);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    Rig { server: server.with_runs(cfg), home, fake, rt }
}

fn rig() -> Rig {
    rig_with(|_| {})
}

impl Rig {
    fn call(&self, tool: &str, args: Value, now: Instant) -> Value {
        let r = self.rt.block_on(self.server.call_async(None, None, tool, args.as_object().cloned(), now));
        serde_json::to_value(r.unwrap()).unwrap()
    }

    fn start(&self, input: &str) -> Value {
        self.call("workflow_run", json!({ "name": "ship", "scope": "project", "input": input }), Instant::now())
    }

    fn status(&self, run: &str, wait_ms: u64) -> Value {
        let r = self.call("run_status", json!({ "run": run, "wait_ms": wait_ms }), Instant::now());
        assert_ne!(r["isError"], true, "{r}");
        r["structuredContent"].clone()
    }

    /// Polls with a wait until `done` says so; returns the last body and every status seen.
    fn until(&self, run: &str, done: impl Fn(&Value) -> bool) -> (Value, Vec<String>) {
        let mut seen = vec!["starting".to_owned()];
        for _ in 0..100 {
            let body = self.status(run, 2000);
            let status = body["status"].as_str().unwrap().to_owned();
            if seen.last() != Some(&status) {
                seen.push(status);
            }
            if done(&body) {
                return (body, seen);
            }
        }
        panic!("the run never got there: {seen:?}");
    }

    fn fake_dir(&self) -> PathBuf {
        self.fake.parent().unwrap().to_owned()
    }
}

fn is_error(r: &Value) -> bool {
    r["isError"] == true
}

fn message(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or_default().to_owned()
}

fn run_id(started: &Value) -> String {
    assert_ne!(started["isError"], true, "{started}");
    started["structuredContent"]["run"].as_str().unwrap().to_owned()
}

#[test]
fn run_tools_need_the_flag_and_the_session_starts_lazily() {
    // Without the flag: not listed, and a call is -32602.
    let (server, _home) = common::server(Caps::default(), Redactor::new());
    let listed: Vec<_> = server.tools().iter().map(|t| t.name.to_string()).collect();
    assert!(!listed.iter().any(|n| n == "workflow_run" || n.starts_with("run_")), "{listed:?}");
    for tool in ["workflow_run", "run_status", "run_cancel"] {
        let e = server.call(None, tool, json!({ "run": "x" }).as_object().cloned(), Instant::now()).unwrap_err();
        assert_eq!(e.code.0, -32602, "{tool}");
    }

    // With the flag, a read-only scenario starts nothing: no child, so no session directory.
    let rig = rig();
    rig.call("workflow_list", json!({}), Instant::now());
    let listed: Vec<_> = rig.server.tools().iter().map(|t| t.name.to_string()).collect();
    assert_eq!(listed[listed.len() - 3..], ["workflow_run", "run_status", "run_cancel"]);
    assert!(!rig.fake_dir().join("argv.txt").exists());
    assert!(!rig.fake_dir().join("session").exists());
    // A status for a handle nobody gave out is an error that says so.
    let r = rig.call("run_status", json!({ "run": "run_nope" }), Instant::now());
    assert!(is_error(&r) && message(&r).contains("no run"), "{r}");
    assert!(!rig.fake_dir().join("argv.txt").exists());
}

#[test]
fn a_scripted_chain_goes_from_starting_to_completed_through_status() {
    let rig = rig();
    let started = rig.start("ok");
    assert_eq!(started["structuredContent"]["status"], "starting");
    let run = run_id(&started);

    let (done, seen) = rig.until(&run, |b| !b["exit_code"].is_null());
    assert_eq!(seen, ["starting", "running", "completed"], "{seen:?}");
    assert_eq!(done["exit_code"], 0);
    assert_eq!(done["session"], "SESSION1");
    let attempts: Vec<(i64, &str, &str)> = done["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["step"].as_i64().unwrap(), a["agent"].as_str().unwrap(), a["status"].as_str().unwrap()))
        .collect();
    assert_eq!(attempts, [(1, "plan", "completed"), (2, "general", "completed")]);
    assert_eq!(done["usage"]["input_tokens"], 10);
    assert_eq!(done["usage"]["cost_usd"], 0.05);
    // The tail is the report, wrapped as data.
    let tail = done["tail"].as_str().unwrap();
    assert!(tail.starts_with("The following is the run's report."), "{tail}");
    assert!(tail.contains("all done"), "{tail}");
    // The session the child made is there.
    assert!(rig.fake_dir().join("session/commits.jsonl.zst").exists());

    // What the child was started with: dont-ask, the pre-approval, the workflow, the input after
    // `--`, and never bypass.
    let argv = fs::read_to_string(rig.fake_dir().join("argv.txt")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    let at = |flag: &str| argv.iter().position(|a| *a == flag).unwrap_or_else(|| panic!("{flag} in {argv:?}"));
    assert_eq!(argv[at("--permission-mode") + 1], "dont-ask");
    assert_eq!(argv[at("--allow") + 1], "bash(cargo *)");
    assert_eq!(argv[at("--workflow") + 1], "ship");
    assert_eq!(&argv[argv.len() - 2..], ["--", "ok"]);
    assert!(!argv.iter().any(|a| a.contains("bypass")), "{argv:?}");
}

#[test]
fn wait_ms_is_capped_and_returns_early_on_change() {
    // The cap: a wait of a minute on a run that is not changing returns when the cap is spent.
    let rig = rig_with(|c| c.max_wait = Duration::from_millis(300));
    let run = run_id(&rig.start("step"));
    let began = Instant::now();
    let body = rig.status(&run, 60_000);
    assert!(began.elapsed() < Duration::from_secs(10), "waited {:?}", began.elapsed());
    // The first wait ended on the child's first event or on the cap; either way a further wait
    // on a quiet run is bounded by the cap too.
    let began = Instant::now();
    let _ = rig.status(&run, 60_000);
    let _ = rig.status(&run, 60_000);
    assert!(began.elapsed() < Duration::from_secs(10), "waited {:?}", began.elapsed());
    assert!(matches!(body["status"].as_str(), Some("starting" | "running")), "{body}");
    rig.call("run_cancel", json!({ "run": run }), Instant::now());

    // Early return: with a long cap, a wait that spans the child's first event comes back as
    // soon as it arrives, long before the wait is spent.
    let rig = rig_with(|c| c.max_wait = Duration::from_secs(25));
    let run = run_id(&rig.start("step"));
    let began = Instant::now();
    let body = rig.status(&run, 20_000);
    assert_eq!(body["status"], "running", "{body}");
    assert!(began.elapsed() < Duration::from_secs(10), "waited {:?}", began.elapsed());
    rig.call("run_cancel", json!({ "run": run }), Instant::now());
}

#[test]
fn a_tool_that_would_ask_is_denied_not_queued() {
    let rig = rig();
    let run = run_id(&rig.start("ask"));
    let (done, seen) = rig.until(&run, |b| !b["exit_code"].is_null());
    // There is no state in which the run waits for an answer: nothing here can give one.
    assert!(!seen.iter().any(|s| s.contains("waiting")), "{seen:?}");
    assert_eq!(done["status"], "partial");
    assert!(done["tail"].as_str().unwrap().contains("bash was denied"), "{done}");
    // And the run was started so that nothing can ask: dont-ask, stdin closed.
    let argv = fs::read_to_string(rig.fake_dir().join("argv.txt")).unwrap();
    assert!(argv.contains("dont-ask") && !argv.contains("bypass"), "{argv}");
}

#[test]
fn the_run_count_and_concurrency_caps_hold() {
    // Concurrency: the default is one at a time.
    let rig = rig();
    let first = run_id(&rig.start("slow"));
    rig.until(&first, |b| b["status"] == "running");
    let second = rig.start("ok");
    assert!(is_error(&second) && message(&second).contains("most at once"), "{second}");
    rig.call("run_cancel", json!({ "run": first }), Instant::now());

    // The count: with 3, the fourth is an error. Each call is a minute apart for the rate limit.
    let rig = rig_with(|c| {
        c.max_runs = 3;
        c.max_concurrent = 3;
    });
    let t0 = Instant::now();
    for i in 0..3u64 {
        let started = rig.call(
            "workflow_run",
            json!({ "name": "ship", "scope": "project", "input": "ok" }),
            t0 + Duration::from_secs(60 * i),
        );
        let run = run_id(&started);
        rig.until(&run, |b| !b["exit_code"].is_null());
    }
    let fourth = rig.call(
        "workflow_run",
        json!({ "name": "ship", "scope": "project", "input": "ok" }),
        t0 + Duration::from_secs(300),
    );
    assert!(is_error(&fourth) && message(&fourth).contains("--max-runs"), "{fourth}");
}

#[test]
fn the_sixth_start_in_a_minute_is_a_rate_limit_error() {
    let rig = rig_with(|c| {
        c.max_runs = 100;
        c.max_concurrent = 100;
    });
    let t0 = Instant::now();
    let start = |now| rig.call("workflow_run", json!({ "name": "ship", "scope": "project", "input": "ok" }), now);
    for _ in 0..5 {
        assert!(!is_error(&start(t0)));
    }
    let limited = start(t0);
    assert!(is_error(&limited) && message(&limited).contains("rate limit"), "{limited}");
}

#[test]
fn the_process_cost_cap_ends_the_run_with_exit_eight() {
    let rig = rig_with(|c| c.max_cost_usd = 1.0);
    let run = run_id(&rig.start("costly"));
    let (done, _) = rig.until(&run, |b| !b["exit_code"].is_null());
    assert_eq!(done["exit_code"], 8, "{done}");
    assert_eq!(done["status"], "partial");
    assert!(done["tail"].as_str().unwrap().contains("budget exceeded"), "{done}");
    // The child was given what was left of the cap, which was all of it.
    let env = fs::read_to_string(rig.fake_dir().join("env.txt")).unwrap();
    assert!(env.lines().any(|l| l == "AIBUS_MAX_COST_USD=1"), "{env}");
    // Its cost spent the cap, so no new run starts.
    let next = rig.call(
        "workflow_run",
        json!({ "name": "ship", "scope": "project", "input": "ok" }),
        Instant::now() + Duration::from_secs(120),
    );
    assert!(is_error(&next) && message(&next).contains("cost cap"), "{next}");
}

#[test]
fn cancel_is_idempotent_and_ends_a_running_chain() {
    let rig = rig();
    let run = run_id(&rig.start("slow"));
    rig.until(&run, |b| b["status"] == "running");
    let cancelled = rig.call("run_cancel", json!({ "run": run }), Instant::now());
    assert_ne!(cancelled["isError"], true, "{cancelled}");
    let body = &cancelled["structuredContent"];
    assert_eq!((body["status"].as_str(), body["exit_code"].as_i64()), (Some("cancelled"), Some(7)), "{body}");
    // Again on the finished run: the same answer.
    let again = rig.call("run_cancel", json!({ "run": run }), Instant::now());
    assert_eq!(again["structuredContent"]["status"], "cancelled");
    assert_eq!(again["structuredContent"]["exit_code"], 7);
    // An unknown handle is an error that says so.
    let unknown = rig.call("run_cancel", json!({ "run": "run_nope" }), Instant::now());
    assert!(is_error(&unknown) && message(&unknown).contains("no run"), "{unknown}");
}

#[test]
fn closing_the_server_cancels_live_runs_and_waits_for_them() {
    let rig = rig();
    let run = run_id(&rig.start("slow"));
    rig.until(&run, |b| b["status"] == "running");
    let Rig { server, home, fake, rt } = rig;
    let began = Instant::now();
    drop(server);
    // The child got its interrupt, wrote its end, and was waited for, inside the budget.
    assert!(began.elapsed() < Duration::from_secs(12), "{:?}", began.elapsed());
    assert!(fake.parent().unwrap().join("session/commits.jsonl.zst").exists());
    drop((home, rt));
}

#[test]
fn a_workflow_that_does_not_validate_or_is_hidden_starts_nothing() {
    let rig = rig();
    let call = |args: Value| rig.call("workflow_run", args, Instant::now());
    // Names that are not names, and a workflow that is not there.
    for name in ["../ship", "--cwd", "Ship", ""] {
        let r = call(json!({ "name": name, "scope": "project", "input": "ok" }));
        assert!(is_error(&r), "{name:?}: {r}");
    }
    let r = call(json!({ "name": "nothing", "scope": "project", "input": "ok" }));
    assert!(is_error(&r) && message(&r).contains("no workflow"), "{r}");
    // Empty input.
    let r = call(json!({ "name": "ship", "scope": "project", "input": "  " }));
    assert!(is_error(&r), "{r}");
    // A run resolves the name project first, so asking for the user scope is refused when the
    // project one is what would start.
    let r = call(json!({ "name": "ship", "scope": "user", "input": "ok" }));
    assert!(is_error(&r) && message(&r).contains("not in the user scope"), "{r}");
    // A workflow naming an agent nobody has.
    let broken = rig.home.path().join("proj/.aibus/workflows/broken.toml");
    fs::write(&broken, "[[step]]\nagent = \"nobody-by-this-name\"\nprompt = \"x\"\n").unwrap();
    let r = call(json!({ "name": "broken", "scope": "project", "input": "ok" }));
    assert!(is_error(&r) && message(&r).contains("does not validate"), "{r}");
    assert!(!rig.fake_dir().join("argv.txt").exists(), "no run was started");
}
