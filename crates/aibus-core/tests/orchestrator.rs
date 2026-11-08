//! B2.1: the limits behind `model:orchestrator:runnable`, `@agent-<name>`, the `delegate`
//! schema, and how the executor hands runs to the spawner. Graph level: nothing here runs a
//! provider (tests/delegation.rs does).

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use aibus_core::agent::discover::discover;
use aibus_core::config::Config;
use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_core::*;
use aibus_graph::Graph;
use common::*;
use tempfile::TempDir;

fn wired() -> (A, Rc<RefCell<Vec<Effect>>>) {
    let mut a = app();
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    a.effects = Effects::new(move |e| sink.borrow_mut().push(e));
    (a, log)
}

/// A delegate from the user (or from `parent`) with an id the test picked.
fn ask(a: &mut A, n: u128, parent: Option<RunId>, account: Option<&str>) -> RunId {
    let run = rid(100 + n);
    a.step(Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: format!("task {n}"),
        background: true,
        isolation: Isolation::None,
        parent,
        run: Some(run),
        model: None,
        account: account.map(|x| AccountName(x.into())),
    }))
    .unwrap();
    run
}

fn start_with(a: &mut A, run: RunId, parent: Option<RunId>, account: &str) {
    a.step(Msg::RunStarted {
        run,
        agent: "worker".parse().unwrap(),
        role: Role::Worker,
        parent,
        model: ModelId("m".into()),
        account: AccountName(account.into()),
    })
    .unwrap();
}

fn runnable(a: &A) -> Vec<String> {
    let v = a.graph.read_key("model:orchestrator:runnable").unwrap();
    as_list(v)
        .iter()
        .map(|r| as_str(get(r, "run").unwrap()).unwrap().to_owned())
        .collect()
}

fn status(a: &A, run: RunId) -> String {
    as_str(a.graph.read_key(&format!("model:run:{run}:status")).unwrap())
        .unwrap()
        .to_owned()
}

fn count(a: &A, k: &str) -> i64 {
    let c = a.graph.read_key("model:orchestrator:counts").unwrap();
    as_i64(get(c, k).unwrap()).unwrap()
}

fn limits(f: impl FnOnce(&mut Limits)) -> Limits {
    let mut l = Limits::default();
    f(&mut l);
    l
}

fn spawns(log: &Rc<RefCell<Vec<Effect>>>) -> usize {
    log.borrow()
        .iter()
        .filter(|e| matches!(e, Effect::Spawn(_)))
        .count()
}

// ---- the five checks of `runnable` -------------------------------------------------------

#[test]
fn defaults_are_3_8_4_and_500() {
    let l = Limits::default();
    assert_eq!(
        (l.max_spawn_depth, l.max_concurrent_runs, l.max_per_account, l.max_runs_per_session),
        (3, 8, 4, 500)
    );
    let a = app();
    let node = a.graph.read_key("model:limits").unwrap();
    let n = |k: &str| as_i64(get(node, k).unwrap()).unwrap();
    assert_eq!(
        (n("max_spawn_depth"), n("max_concurrent_runs"), n("max_per_account"), n("max_runs_per_session")),
        (3, 8, 4, 500)
    );
}

#[test]
fn check_1_the_parent_has_to_be_alive() {
    let mut a = app();
    let parent = ask(&mut a, 1, None, None);
    let child = ask(&mut a, 2, Some(parent), None);
    assert_eq!(runnable(&a), [parent.to_string()], "the parent is only queued");
    start_with(&mut a, parent, None, "acct");
    assert_eq!(runnable(&a), [child.to_string()], "the parent runs now");
}

#[test]
fn a_run_whose_parent_ended_is_cancelled_and_never_listed() {
    let mut a = app();
    a.set_limits(&limits(|l| l.max_concurrent_runs = 1)).unwrap();
    let parent = ask(&mut a, 1, None, None);
    start_with(&mut a, parent, None, "acct");
    // The only slot is taken, so the child waits in the queue ...
    let child = ask(&mut a, 2, Some(parent), None);
    let grandchild = ask(&mut a, 3, Some(child), None);
    assert!(runnable(&a).is_empty());
    assert_eq!(status(&a, child), "queued");
    // ... and the parent ends before it gets a slot.
    a.step(Msg::RunFinished {
        run: parent,
        result: RunResult {
            run_id: parent,
            agent: "worker".parse().unwrap(),
            status: RunStatus::Completed,
            turns: 1,
            usage: Default::default(),
            summary: String::new(),
            summary_file: None,
            duration: Default::default(),
            error: None,
        },
    })
    .unwrap();
    assert_eq!(status(&a, child), "cancelled");
    assert_eq!(status(&a, grandchild), "cancelled", "and the queue below it");
    assert!(runnable(&a).is_empty());

    // A delegate that names an ended parent is cancelled on arrival.
    let late = ask(&mut a, 4, Some(parent), None);
    assert_eq!(status(&a, late), "cancelled");
    assert!(runnable(&a).is_empty());
}

#[test]
fn cancelling_a_queued_parent_cancels_its_queued_children() {
    let mut a = app();
    let parent = ask(&mut a, 1, None, None);
    let child = ask(&mut a, 2, Some(parent), None);
    a.step(Msg::Command(Command::Cancel { run: parent })).unwrap();
    assert_eq!(status(&a, child), "cancelled");
    assert_eq!(count(&a, "failed"), 2);
}

#[test]
fn check_2_depth_is_limited_to_max_spawn_depth() {
    let mut a = app();
    start_with(&mut a, R(), None, "acct");
    let mut parent = R();
    let mut chain = vec![];
    for i in 1..=3u128 {
        let child = rid(10 + i);
        start_with(&mut a, child, Some(parent), "acct");
        chain.push(child);
        parent = child;
    }
    // chain[1] is at depth 2: a child of it is at depth 3, which is allowed ...
    let ok = ask(&mut a, 1, Some(chain[1]), None);
    assert_eq!(runnable(&a), [ok.to_string()]);
    // ... and a child of chain[2] (depth 3) would be at depth 4.
    let _too_deep = ask(&mut a, 2, Some(chain[2]), None);
    assert_eq!(runnable(&a), [ok.to_string()], "depth 4 is held back");
    // The same run is fine when the limit is raised.
    a.set_limits(&limits(|l| l.max_spawn_depth = 4)).unwrap();
    assert_eq!(runnable(&a).len(), 2);
}

#[test]
fn check_3_concurrency_is_limited_to_max_concurrent_runs() {
    let mut a = app();
    a.set_limits(&limits(|l| l.max_concurrent_runs = 3)).unwrap();
    start_with(&mut a, R(), None, "acct");
    let runs: Vec<RunId> = (1..=4).map(|i| ask(&mut a, i, None, None)).collect();
    assert_eq!(runnable(&a).len(), 2, "one slot is the lead's");
    start_with(&mut a, runs[0], None, "acct");
    assert_eq!(runnable(&a), [runs[1].to_string()]);
    start_with(&mut a, runs[1], None, "acct");
    assert!(runnable(&a).is_empty());
    assert_eq!(count(&a, "queued"), 2);
    assert_eq!(count(&a, "running"), 3);
}

#[test]
fn check_4_each_account_has_its_own_slots() {
    let mut a = app();
    let mut l = limits(|l| l.max_per_account = 2);
    l.per_account.insert("small".into(), 1);
    a.set_limits(&l).unwrap();
    let s1 = ask(&mut a, 1, None, Some("small"));
    let s2 = ask(&mut a, 2, None, Some("small"));
    let b1 = ask(&mut a, 3, None, Some("big"));
    let b2 = ask(&mut a, 4, None, Some("big"));
    let b3 = ask(&mut a, 5, None, Some("big"));
    // "small" has one slot (its own entry), "big" the default of two.
    let want: Vec<String> = [s1, b1, b2].iter().map(ToString::to_string).collect();
    assert_eq!(runnable(&a), want);
    start_with(&mut a, s1, None, "small");
    start_with(&mut a, b1, None, "big");
    start_with(&mut a, b2, None, "big");
    assert!(runnable(&a).is_empty(), "{s2} and {b3} wait for a slot");
    assert_eq!(status(&a, s2), "queued");
}

#[test]
fn check_5_the_session_total_is_limited() {
    // Queueing stops at the cap ...
    let mut a = app();
    a.set_limits(&limits(|l| l.max_runs_per_session = 3)).unwrap();
    for i in 1..=4 {
        ask(&mut a, i, None, None);
    }
    assert_eq!(count(&a, "queued"), 3, "the fourth is not queued");

    // ... and `runnable` holds back what would be one run too many (a snapshot taken under a
    // higher limit and resumed under a lower one).
    let mut a = app();
    let runs: Vec<RunId> = (1..=3).map(|i| ask(&mut a, i, None, None)).collect();
    start_with(&mut a, runs[0], None, "acct");
    a.set_limits(&limits(|l| l.max_runs_per_session = 2)).unwrap();
    assert_eq!(runnable(&a), [runs[1].to_string()]);
    a.set_limits(&limits(|l| l.max_runs_per_session = 1)).unwrap();
    assert!(runnable(&a).is_empty());
}

#[test]
fn the_ninth_run_stays_queued_and_runnable_excludes_it() {
    let (mut a, log) = wired();
    start_with(&mut a, R(), None, "acct");
    let kids: Vec<RunId> = (1..=8).map(|i| ask(&mut a, i, None, None)).collect();
    // The lead plus seven are the eight; the eighth child is the ninth run.
    assert_eq!(spawns(&log), 7);
    for k in &kids[..7] {
        start_with(&mut a, *k, None, "acct");
    }
    assert_eq!(count(&a, "running"), 8);
    assert_eq!(status(&a, kids[7]), "queued");
    assert!(runnable(&a).is_empty());
    assert_eq!(spawns(&log), 7, "nothing more was handed to the spawner");
    assert_eq!(count(&a, "queued"), 1);

    // A slot frees up and the ninth goes.
    a.step(Msg::RunFinished {
        run: kids[0],
        result: RunResult {
            run_id: kids[0],
            agent: "worker".parse().unwrap(),
            status: RunStatus::Completed,
            turns: 1,
            usage: Default::default(),
            summary: String::new(),
            summary_file: None,
            duration: Default::default(),
            error: None,
        },
    })
    .unwrap();
    assert_eq!(runnable(&a), [kids[7].to_string()]);
    assert_eq!(spawns(&log), 8);
}

#[test]
fn counts_has_running_queued_done_and_failed() {
    let mut a = app();
    let runs: Vec<RunId> = (1..=5).map(|i| ask(&mut a, i, None, None)).collect();
    for r in &runs[..4] {
        start_with(&mut a, *r, None, "acct");
    }
    let finish = |a: &mut A, run: RunId, status| {
        a.step(Msg::RunFinished {
            run,
            result: RunResult {
                run_id: run,
                agent: "worker".parse().unwrap(),
                status,
                turns: 1,
                usage: Default::default(),
                summary: String::new(),
                summary_file: None,
                duration: Default::default(),
                error: None,
            },
        })
        .unwrap()
    };
    finish(&mut a, runs[0], RunStatus::Completed);
    finish(&mut a, runs[1], RunStatus::Partial);
    finish(&mut a, runs[2], RunStatus::Failed);
    let got: Vec<i64> = ["running", "queued", "done", "failed"]
        .iter()
        .map(|k| count(&a, k))
        .collect();
    assert_eq!(got, [1, 1, 2, 1]);
}

// ---- limits from config and the environment ----------------------------------------------

#[test]
fn the_environment_overrides_depth_and_concurrency() {
    let cfg = Config::default();
    let none = |_: &str| None;
    assert_eq!(Limits::resolve(&cfg, none).unwrap(), Limits::default());

    let env = |k: &str| match k {
        "AIBUS_MAX_SPAWN_DEPTH" => Some("1".to_owned()),
        "AIBUS_MAX_CONCURRENT_RUNS" => Some("2".to_owned()),
        _ => None,
    };
    let l = Limits::resolve(&cfg, env).unwrap();
    assert_eq!((l.max_spawn_depth, l.max_concurrent_runs), (1, 2));
    assert_eq!(l.max_runs_per_session, 500, "the rest is untouched");

    let mut cfg = Config::default();
    cfg.limits.max_concurrent_runs = 5;
    cfg.limits.max_spawn_depth = 2;
    let l = Limits::resolve(&cfg, none).unwrap();
    assert_eq!((l.max_spawn_depth, l.max_concurrent_runs), (2, 5), "config");
    let l = Limits::resolve(&cfg, env).unwrap();
    assert_eq!((l.max_spawn_depth, l.max_concurrent_runs), (1, 2), "env over config");

    let bad = |_: &str| Some("lots".to_owned());
    let e = Limits::resolve(&cfg, bad).unwrap_err();
    assert!(e.to_string().contains("AIBUS_MAX_SPAWN_DEPTH"), "{e}");
    assert_eq!(e.exit_code(), 2);
}

#[test]
fn each_accounts_rate_limit_is_its_slot_count() {
    let mut cfg = Config::default();
    cfg.accounts.push(aibus_core::config::Account {
        name: "work".into(),
        ..Default::default()
    });
    cfg.accounts[0].rate.max_concurrent = 2;
    let l = Limits::resolve(&cfg, |_| None).unwrap();
    assert_eq!(l.per_account, BTreeMap::from([("work".to_owned(), 2)]));
}

// ---- @agent-<name> -----------------------------------------------------------------------

#[test]
fn a_mention_is_the_name_and_the_rest() {
    let m = |t: &str| parse_mention(t).map(|(a, r)| (a.to_string(), r.to_owned()));
    assert_eq!(
        m("@agent-code-reviewer look at src/auth.rs"),
        Some(("code-reviewer".into(), "look at src/auth.rs".into()))
    );
    assert_eq!(m("  @agent-explore\n  find it"), Some(("explore".into(), "find it".into())));
    assert_eq!(m("@agent-explore"), Some(("explore".into(), String::new())));
    assert_eq!(m("please @agent-explore this"), None, "only at the start");
    assert_eq!(m("@agent-Bad name"), None, "not a valid name");
    assert_eq!(m("@agent- nothing"), None);
    assert_eq!(m("@explore this"), None);
}

#[test]
fn a_mention_in_a_prompt_is_a_forced_background_delegate_of_the_lead() {
    let mut a = app();
    a.step(Msg::SubmitPrompt { text: "hello".into() }).unwrap();
    let lead = as_str(&as_list(a.graph.read_key("model:runs:order").unwrap())[0].clone())
        .unwrap()
        .parse::<RunId>()
        .unwrap();
    start_with(&mut a, lead, None, "acct");
    a.step(Msg::SubmitPrompt {
        text: "@agent-explore find the parser".into(),
    })
    .unwrap();
    let order = as_list(a.graph.read_key("model:runs:order").unwrap()).to_vec();
    assert_eq!(order.len(), 2);
    let child = as_str(&order[1]).unwrap();
    let meta = a.graph.read_key(&format!("model:run:{child}:meta")).unwrap();
    assert_eq!(as_str(get(meta, "agent").unwrap()), Some("explore"));
    assert_eq!(as_str(get(meta, "parent").unwrap()), Some(lead.to_string().as_str()));
    assert_eq!(get(meta, "background"), Some(&aibus_graph::Value::Bool(true)));
    assert_eq!(as_str(get(meta, "prompt").unwrap()), Some("find the parser"));
    assert_eq!(runnable(&a), [child.to_owned()], "the lead is alive, so it can start");
}

// ---- the schema ---------------------------------------------------------------------------

fn defs_with_reviewer() -> (TempDir, Vec<AgentDef>) {
    let dir = TempDir::new().unwrap();
    let agents = dir.path().join(".aibus/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews a diff for bugs.\nrole: worker\n---\nReview it.\n",
    )
    .unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let defs = found.agents.values().cloned().collect();
    (dir, defs)
}

#[test]
fn the_delegate_tool_matches_01_section_4_1() {
    let (_dir, defs) = defs_with_reviewer();
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let orch = Orchestrator::new(Limits::default(), tx);
    orch.set_roster(Roster::of(defs.iter()));
    let spec = Tool::spec(&Delegate::new(orch.clone()));

    assert_eq!(spec.name, "delegate");
    let s = &spec.schema;
    assert_eq!(s["required"], serde_json::json!(["agent", "prompt"]));
    let props = s["properties"].as_object().unwrap();
    let keys: Vec<&str> = props.keys().map(String::as_str).collect();
    for k in ["agent", "prompt", "background", "isolation", "model", "account"] {
        assert!(keys.contains(&k), "{k}");
    }
    assert_eq!(props["background"]["default"], serde_json::json!(true));
    assert_eq!(props["isolation"]["enum"], serde_json::json!(["none", "worktree"]));
    let agents: Vec<&str> = props["agent"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for a in ["explore", "plan", "general", "code-reviewer"] {
        assert!(agents.contains(&a), "{a} in {agents:?}");
    }
    assert!(!agents.contains(&"lead"), "workers only");
    assert!(!agents.contains(&"advisor"), "workers only");

    // The description carries the shared roster text: every non-built-in, none of the others.
    let roster = aibus_core::agent::roster::text(defs.iter());
    assert!(roster.contains("- code-reviewer: Reviews a diff for bugs."));
    assert!(spec.description.contains(&roster));
    assert!(!spec.description.contains("- explore:"));
}

// ---- the executor hands runs over ---------------------------------------------------------

#[test]
fn spawn_specs_carry_the_model_and_account_the_caller_asked_for() {
    let (mut a, log) = wired();
    a.step(Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: "p".into(),
        background: false,
        isolation: Isolation::Worktree,
        parent: None,
        run: Some(rid(5)),
        model: Some("opus".into()),
        account: Some(AccountName("work".into())),
    }))
    .unwrap();
    let log = log.borrow();
    let Some(Effect::Spawn(spec)) = log.iter().find(|e| matches!(e, Effect::Spawn(_))) else {
        panic!("no spawn");
    };
    assert_eq!(spec.run, rid(5), "the caller's id is kept");
    assert_eq!(spec.model.as_deref(), Some("opus"));
    assert_eq!(spec.account, Some(AccountName("work".into())));
    assert_eq!(spec.isolation, Isolation::Worktree);
}

#[test]
fn a_command_that_arrives_twice_queues_one_run() {
    let mut a = app();
    let run = ask(&mut a, 1, None, None);
    let again = ask(&mut a, 1, None, None);
    assert_eq!(run, again);
    assert_eq!(count(&a, "queued"), 1);
}

#[test]
fn background_moves_a_run_to_the_background_and_a_definition_can_force_it() {
    let mut a = app();
    a.step(Msg::AgentsReloaded {
        defs: {
            let (_d, mut defs) = defs_with_reviewer();
            defs.iter_mut()
                .find(|d| d.name.as_str() == "code-reviewer")
                .unwrap()
                .background = true;
            defs
        },
        warnings: vec![],
    })
    .unwrap();
    let meta_bg = |a: &A, run: RunId| {
        get(a.graph.read_key(&format!("model:run:{run}:meta")).unwrap(), "background").cloned()
    };
    // The definition says background: true, the caller says false.
    let forced = rid(7);
    a.step(Msg::Command(Command::Delegate {
        agent: "code-reviewer".parse().unwrap(),
        prompt: "p".into(),
        background: false,
        isolation: Isolation::None,
        parent: None,
        run: Some(forced),
        model: None,
        account: None,
    }))
    .unwrap();
    assert_eq!(meta_bg(&a, forced), Some(aibus_graph::Value::Bool(true)));

    let fg = ask_fg(&mut a, 8);
    assert_eq!(meta_bg(&a, fg), Some(aibus_graph::Value::Bool(false)));
    a.step(Msg::Command(Command::Background { run: fg })).unwrap();
    assert_eq!(meta_bg(&a, fg), Some(aibus_graph::Value::Bool(true)));
}

fn ask_fg(a: &mut A, n: u128) -> RunId {
    let run = rid(100 + n);
    a.step(Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: "p".into(),
        background: false,
        isolation: Isolation::None,
        parent: None,
        run: Some(run),
        model: None,
        account: None,
    }))
    .unwrap();
    run
}

#[test]
fn no_spawn_effect_is_lost_when_the_executor_has_no_spawner() {
    // Without an orchestrator the sink still gets `Spawn`, once, as before.
    let (mut a, log) = wired();
    let run = ask(&mut a, 1, None, None);
    a.step(Msg::Tick).unwrap();
    assert_eq!(spawns(&log), 1);
    start_with(&mut a, run, None, "acct");
    assert_eq!(status(&a, run), "starting");
}
