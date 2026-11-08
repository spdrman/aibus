mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use aibus_core::state::reduce::{reduce, Env};
use aibus_core::state::value::{as_i64, as_list, as_str, get, map, s};
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};
use common::*;
use tokio::sync::mpsc;

fn wired() -> (A, Rc<RefCell<Vec<Effect>>>) {
    let mut a = app();
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    a.effects = Effects::new(move |e| sink.borrow_mut().push(e));
    (a, log)
}

fn spawns(log: &Rc<RefCell<Vec<Effect>>>) -> usize {
    log.borrow()
        .iter()
        .filter(|e| matches!(e, Effect::Spawn(_)))
        .count()
}

#[test]
fn one_commit_per_msg() {
    for case in cases() {
        let mut a = app();
        (case.setup)(&mut a);
        let seen = Rc::new(Cell::new(0));
        let c = seen.clone();
        a.graph
            .subscribe_commits(Box::new(move |_| c.set(c.get() + 1)));
        let before = a.graph.now();
        a.step(case.msg.clone()).unwrap();
        assert_eq!(seen.get(), 1, "{}", case.msg.name());
        assert_eq!(a.graph.now(), before + 1, "{}", case.msg.name());
    }
}

#[test]
fn a_repeated_identical_msg_still_commits_each_time() {
    let mut a = app();
    let t = a.graph.now();
    for _ in 0..5 {
        a.step(Msg::Tick).unwrap();
    }
    assert_eq!(a.graph.now(), t + 5);
}

#[test]
fn a_replayed_commit_spawns_nothing_twice() {
    let (mut a, log) = wired();
    a.step(delegate()).unwrap();
    assert_eq!(spawns(&log), 1);
    let commit = a.graph.commit_log().last().unwrap().clone();
    a.effects.after_commit(&a.graph, &commit);
    a.effects.after_commit(&a.graph, &commit);
    a.step(Msg::Resize { cols: 80, rows: 24 }).unwrap();
    assert_eq!(spawns(&log), 1);
    // The run starts: it leaves the listing, nothing new appears.
    a.step(started(first_delegated())).unwrap();
    assert_eq!(spawns(&log), 1);
}

#[test]
fn a_retry_starts_the_run_again() {
    let (mut a, log) = wired();
    a.step(delegate()).unwrap();
    a.step(started(first_delegated())).unwrap();
    a.step(Msg::RunFailed {
        run: first_delegated(),
        error: RunError {
            kind: RunErrorKind::Provider,
            message: String::new(),
            retryable: true,
        },
    })
    .unwrap();
    a.step(Msg::Command(Command::Retry {
        run: first_delegated(),
    }))
    .unwrap();
    assert_eq!(spawns(&log), 2);
}

#[test]
fn hydrate_plus_one_after_commit_restarts_queued_work() {
    let (mut a, log) = wired();
    a.step(delegate()).unwrap();
    a.step(Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: "second".into(),
        background: false,
        isolation: Isolation::None,
        parent: None,
        run: None,
        model: None,
        account: None,
    }))
    .unwrap();
    assert_eq!(spawns(&log), 2);
    let snap = a.graph.snapshot();

    let (mut b, blog) = wired();
    b.hydrate(snap.clone()).unwrap();
    assert_eq!(spawns(&blog), 2, "both queued runs start again");
    let commit = b.graph.commit_log().last().unwrap().clone();
    b.effects.after_commit(&b.graph, &commit);
    assert_eq!(spawns(&blog), 2);
    assert_eq!(b.graph.snapshot().inputs, snap.inputs);
    assert_eq!(
        b.graph.commit_log().last().unwrap().originated_at,
        Some(snap.now)
    );
}

#[test]
fn the_seven_effect_nodes_each_start_their_effect() {
    let (mut a, log) = wired();
    running(&mut a);
    // 1 runnable
    a.step(delegate()).unwrap();
    // 2 tools:pending (approved only)
    a.step(Msg::ToolCallRequested {
        run: R(),
        call: tool_call(true),
    })
    .unwrap();
    assert!(
        !log.borrow().iter().any(|e| matches!(e, Effect::Tool(_))),
        "needs permission first"
    );
    a.step(Msg::PermissionNeeded {
        request: permission_request(),
    })
    .unwrap();
    a.step(Msg::PermissionDecided {
        id: perm(),
        decision: Decision::Allow,
        reason: None,
    })
    .unwrap();
    // 3 hooks:due, 4 router:queue
    poke(
        &mut a,
        "model:hooks:due",
        Value::List(
            vec![map([
                ("id", s("h1")),
                ("event", s("stop")),
                ("run", Value::Null),
            ])]
            .into(),
        ),
    );
    poke(
        &mut a,
        "model:router:queue",
        Value::List(vec![map([("fork", Value::I64(3))])].into()),
    );
    a.step(Msg::Tick).unwrap();
    // 5 checkpoint:pending
    a.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::BeforePlan,
        detail: String::new(),
    })
    .unwrap();
    // 7 cancel
    a.step(Msg::Command(Command::Cancel { run: R() })).unwrap();
    // 6 persist:due after 64 messages
    for _ in 0..64 {
        a.step(Msg::Tick).unwrap();
    }
    let got = log.borrow();
    let count = |f: fn(&Effect) -> bool| got.iter().filter(|e| f(e)).count();
    assert_eq!(count(|e| matches!(e, Effect::Spawn(_))), 1);
    assert_eq!(
        count(|e| matches!(e, Effect::Tool(t) if t.name == "bash")),
        1
    );
    assert_eq!(count(|e| matches!(e, Effect::Hook(_))), 1);
    assert_eq!(count(|e| matches!(e, Effect::Router(_))), 1);
    assert_eq!(
        count(|e| matches!(e, Effect::Advise { checkpoint } if checkpoint == "before_plan")),
        1
    );
    assert_eq!(count(|e| matches!(e, Effect::Cancel(r) if *r == R())), 1);
    assert_eq!(count(|e| matches!(e, Effect::Persist)), 1);
}

#[test]
fn a_denied_tool_never_runs() {
    let (mut a, log) = wired();
    with_perm(&mut a);
    a.step(Msg::PermissionDecided {
        id: perm(),
        decision: Decision::Deny,
        reason: None,
    })
    .unwrap();
    assert!(!log.borrow().iter().any(|e| matches!(e, Effect::Tool(_))));
    assert!(as_list(a.graph.read_key("model:tools:pending").unwrap()).is_empty());
}

#[test]
fn persist_is_due_after_64_msgs_or_30_seconds_and_cleared_by_persisted() {
    let clock = Rc::new(Cell::new(NOW));
    let c = clock.clone();
    let mut a = App::new(MiniGraph::new(), move || c.get());
    let due = |a: &A| a.graph.read_key("model:persist:due") == Some(&Value::Bool(true));
    assert!(!due(&a));
    for _ in 0..62 {
        a.step(Msg::Tick).unwrap();
    }
    assert!(!due(&a));
    a.step(Msg::Tick).unwrap();
    a.step(Msg::Tick).unwrap();
    assert!(due(&a), "64 messages");
    a.step(Msg::Persisted).unwrap();
    assert!(!due(&a));
    a.step(Msg::Tick).unwrap();
    assert!(!due(&a));
    clock.set(NOW + 31_000);
    a.step(Msg::Tick).unwrap();
    assert!(due(&a), "30 s with something to save");
}

#[test]
fn counts_runnable_and_limits() {
    let (mut a, log) = wired();
    for _ in 0..10 {
        a.step(delegate()).unwrap();
    }
    // Only 8 may run at once.
    assert_eq!(spawns(&log), 8);
    let counts = a.graph.read_key("model:orchestrator:counts").unwrap();
    assert_eq!(as_i64(get(counts, "queued").unwrap()), Some(10));
    let runnable = a.graph.read_key("model:orchestrator:runnable").unwrap();
    assert_eq!(as_list(runnable).len(), 8);

    // A child waits for a live parent.
    let (mut a, log) = wired();
    a.step(delegate()).unwrap();
    let parent = first_delegated();
    a.step(Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: "child".into(),
        background: false,
        isolation: Isolation::None,
        parent: Some(parent),
        run: None,
        model: None,
        account: None,
    }))
    .unwrap();
    assert_eq!(
        spawns(&log),
        1,
        "the child waits while the parent is only queued"
    );
    a.step(started(parent)).unwrap();
    assert_eq!(spawns(&log), 2, "the parent is alive, the child starts");
    let counts = a.graph.read_key("model:orchestrator:counts").unwrap();
    assert_eq!(as_i64(get(counts, "running").unwrap()), Some(1));

    // Depth: a child of a child of a child of a child is past max_spawn_depth (3).
    let mut a = app();
    a.step(started(R())).unwrap();
    let mut parent = R();
    for i in 0..5u128 {
        let child = rid(10 + i);
        a.step(Msg::RunStarted {
            run: child,
            agent: "worker".parse().unwrap(),
            role: Role::Worker,
            parent: Some(parent),
            model: ModelId("m".into()),
            account: AccountName("acct".into()),
        })
        .unwrap();
        parent = child;
    }
    a.step(Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: "too deep".into(),
        background: false,
        isolation: Isolation::None,
        parent: Some(parent),
        run: None,
        model: None,
        account: None,
    }))
    .unwrap();
    let runnable = a.graph.read_key("model:orchestrator:runnable").unwrap();
    assert!(
        as_list(runnable).is_empty(),
        "depth limit holds the run back"
    );
}

#[test]
fn checkpoint_pending_follows_the_advisor() {
    let mut a = app();
    assert_eq!(
        a.graph.read_key("model:checkpoint:pending"),
        Some(&Value::Null)
    );
    a.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::SameErrorX2,
        detail: String::new(),
    })
    .unwrap();
    assert_eq!(
        as_str(a.graph.read_key("model:checkpoint:pending").unwrap()),
        Some("same_error_x2")
    );
    a.step(Msg::AdvisorSpoke {
        checkpoint: Checkpoint::SameErrorX2,
        advice: "a".into(),
        tokens_read: 10,
    })
    .unwrap();
    assert_eq!(
        a.graph.read_key("model:checkpoint:pending"),
        Some(&Value::Null)
    );
}

#[test]
fn tick_records_dropped_ticks_in_the_graph() {
    let a = app();
    let w = reduce(
        &a.graph,
        &a.store,
        &Msg::Tick,
        Env {
            now_ms: NOW,
            ticks_dropped: 5,
        },
    )
    .unwrap();
    let (_, v) = w.iter().find(|(k, _)| k == "eng:ticks_dropped").unwrap();
    assert_eq!(v, &Value::I64(5));
}

#[test]
fn animating_follows_the_runs() {
    let mut a = app();
    let on = |a: &A| a.graph.read_key("view:animating") == Some(&Value::Bool(true));
    assert!(!on(&a));
    a.step(started(R())).unwrap();
    assert!(on(&a));
    a.step(text_stream(R(), "x")).unwrap();
    assert!(on(&a));
    a.step(Msg::PermissionNeeded {
        request: permission_request(),
    })
    .unwrap();
    assert!(!on(&a), "waiting for the user is not an animation");
}

#[tokio::test(start_paused = true)]
async fn no_ticks_for_five_seconds_when_idle() {
    let (tx, mut rx) = mpsc::channel(4096);
    let ticker = Ticker::spawn(tx, 30);
    tokio::task::yield_now().await;
    for _ in 0..50 {
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
    }
    assert!(rx.try_recv().is_err(), "an idle app must not tick");
    assert_eq!(ticker.dropped(), 0);
}

#[tokio::test(start_paused = true)]
async fn ticks_flow_only_while_animating() {
    let (tx, mut rx) = mpsc::channel(4096);
    let ticker = Ticker::spawn(tx, 30);
    ticker.set_animating(true);
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(34)).await;
        tokio::task::yield_now().await;
    }
    let mut n = 0;
    while rx.try_recv().is_ok() {
        n += 1;
    }
    assert!(
        (25..=32).contains(&n),
        "about 30 ticks in a second, got {n}"
    );
    ticker.set_animating(false);
    tokio::task::yield_now().await;
    for _ in 0..20 {
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn a_full_channel_drops_ticks_and_counts_them() {
    let (tx, mut rx) = mpsc::channel(2);
    let ticker = Ticker::spawn(tx, 30);
    ticker.set_animating(true);
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(34)).await;
        tokio::task::yield_now().await;
    }
    assert!(ticker.dropped() >= 20, "dropped {}", ticker.dropped());
    let mut n = 0;
    while rx.try_recv().is_ok() {
        n += 1;
    }
    assert_eq!(n, 2);
}

#[tokio::test]
async fn the_loop_runs_until_shutdown_and_the_channel_is_bounded() {
    let mut a = app();
    let tx = a.sender();
    assert_eq!(tx.max_capacity(), MSG_CAPACITY);
    assert_eq!(MSG_CAPACITY, 4096);
    tx.send(Msg::Resize {
        cols: 100,
        rows: 30,
    })
    .await
    .unwrap();
    tx.send(Msg::Shutdown {
        reason: ShutdownReason::UserQuit,
    })
    .await
    .unwrap();
    tx.send(Msg::Resize { cols: 1, rows: 1 }).await.unwrap();
    a.run().await.unwrap();
    assert!(a.should_quit());
    let size = a.graph.read_key("ctrl:size").unwrap();
    assert_eq!(
        as_i64(get(size, "cols").unwrap()),
        Some(100),
        "nothing after Shutdown is applied"
    );
}
