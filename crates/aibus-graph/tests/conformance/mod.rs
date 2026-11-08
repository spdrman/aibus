//! Graph conformance suite (spec 10, section 8). Generic over the backend: `suite(MiniGraph::new)`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use aibus_graph::{
    CommitError, Compute, Graph, GraphTime, HydrateError, Node, NodeKey, RetentionResult,
    SimulateResult, SmolStr, Tracker, Value, COMMIT_HISTORY_CAP,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

fn k(s: &str) -> NodeKey {
    SmolStr::new(s)
}

fn int(v: &Value) -> i64 {
    match v {
        Value::I64(i) => *i,
        _ => 0,
    }
}

fn compute(f: impl Fn(&mut Tracker) -> Value + Send + 'static) -> Compute {
    Box::new(f)
}

fn bytes<G: Graph>(g: &G) -> Vec<u8> {
    serde_json::to_vec(&g.snapshot()).unwrap()
}

type Log = Rc<RefCell<Vec<String>>>;

fn recorder(log: &Log, tag: &'static str) -> aibus_graph::NodeObserver {
    let log = log.clone();
    Box::new(move |v, c| {
        log.borrow_mut()
            .push(format!("{tag}={} @{}", int(v), c.time))
    })
}

pub fn suite<G: Graph>(mk: fn() -> G) {
    determinism(mk);
    glitch_freedom(mk);
    atomic_rollback(mk);
    time(mk);
    subscriber_order(mk);
    dynamic_dependencies(mk);
    equality_cutoff(mk);
    retention(mk);
    time_travel(mk);
    layering(mk);
    explain_and_metadata(mk);
    simulate(mk);
    hydrate_round_trip(mk);
}

// ---------------------------------------------------------------- determinism

#[derive(Debug, Clone)]
struct Spec {
    inputs: usize,
    /// (kind, deps): kind 0 sums every dep; kind 1 reads deps[0] and then one of two others,
    /// so its dependency set changes with the data.
    derived: Vec<(u8, Vec<usize>)>,
    commits: Vec<Vec<(usize, i64)>>,
}

fn spec() -> impl Strategy<Value = Spec> {
    (1usize..=4, 1usize..=8).prop_flat_map(|(inputs, n)| {
        let derived: Vec<_> = (0..n)
            .map(|i| (0u8..2, vec(0..inputs + i, 1..=3)))
            .collect();
        let commits = vec(vec((0..inputs, -50i64..50), 1..=3), 1..=8);
        (Just(inputs), derived, commits).prop_map(|(inputs, derived, commits)| Spec {
            inputs,
            derived,
            commits,
        })
    })
}

fn oracle(spec: &Spec, ins: &[i64]) -> Vec<i64> {
    let mut v = ins.to_vec();
    for (kind, deps) in &spec.derived {
        let x = if *kind == 0 {
            deps.iter().fold(0i64, |a, d| a.wrapping_add(v[*d]))
        } else if v[deps[0]] % 2 == 0 {
            v[deps[1 % deps.len()]]
        } else {
            v[deps[2 % deps.len()]]
        };
        v.push(x);
    }
    v
}

fn build<G: Graph>(g: &mut G, spec: &Spec) -> Vec<Node> {
    let mut nodes: Vec<Node> = (0..spec.inputs)
        .map(|i| g.input(&k(&format!("model:in:{i}")), Value::I64(0)))
        .collect();
    for (i, (kind, deps)) in spec.derived.iter().enumerate() {
        let ns: Vec<Node> = deps.iter().map(|d| nodes[*d]).collect();
        let kind = *kind;
        let node = g.derived(
            &k(&format!("model:d:{i}")),
            compute(move |t| {
                Value::I64(if kind == 0 {
                    ns.iter().fold(0i64, |a, n| a.wrapping_add(int(t.get(*n))))
                } else if int(t.get(ns[0])) % 2 == 0 {
                    int(t.get(ns[1 % ns.len()]))
                } else {
                    int(t.get(ns[2 % ns.len()]))
                })
            }),
        );
        nodes.push(node);
    }
    nodes
}

fn replay<G: Graph>(mk: fn() -> G, spec: &Spec) -> Result<G, TestCaseError> {
    let mut g = mk();
    let nodes = build(&mut g, spec);
    let mut ins = vec![0i64; spec.inputs];
    for writes in &spec.commits {
        let merged: BTreeMap<usize, i64> = writes.iter().copied().collect();
        let before = g.now();
        let changed = merged.iter().any(|(i, v)| ins[*i] != *v);
        for (i, v) in &merged {
            ins[*i] = *v;
        }
        let w: Vec<_> = merged
            .iter()
            .map(|(i, v)| (nodes[*i], Value::I64(*v)))
            .collect();
        g.commit("step", &w)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(g.now(), before + u64::from(changed));
        for (n, want) in nodes.iter().zip(oracle(spec, &ins)) {
            prop_assert_eq!(g.read(*n), &Value::I64(want));
        }
    }
    Ok(g)
}

pub fn determinism<G: Graph>(mk: fn() -> G) {
    let mut runner = TestRunner::new(Config {
        cases: 1000,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&spec(), |spec| {
            let a = replay(mk, &spec)?;
            let b = replay(mk, &spec)?;
            prop_assert_eq!(bytes(&a), bytes(&b));
            let log = |g: &G| {
                g.commit_log()
                    .iter()
                    .map(|c| (c.time, c.intent.clone(), c.changed_nodes.clone()))
                    .collect::<Vec<_>>()
            };
            prop_assert_eq!(log(&a), log(&b));

            // A fresh graph hydrated from the final snapshot agrees on every value.
            let mut c = mk();
            let nodes = build(&mut c, &spec);
            c.hydrate(a.snapshot())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            for (n, want) in nodes.iter().zip(oracle(
                &spec,
                &c.snapshot().inputs.values().map(int).collect::<Vec<_>>(),
            )) {
                prop_assert_eq!(c.read(*n), &Value::I64(want));
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("determinism: {e}"));
}

// ---------------------------------------------------------------- glitch freedom

pub fn glitch_freedom<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(0));
    let a = g.derived(
        &k("model:a"),
        compute(move |t| Value::I64(int(t.get(x)) + 1)),
    );
    let b = g.derived(
        &k("model:b"),
        compute(move |t| Value::I64(int(t.get(x)) * 2)),
    );
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let d = g.derived(
        &k("model:d"),
        compute(move |t| {
            counted.fetch_add(1, Ordering::SeqCst);
            Value::List(vec![t.get(a).clone(), t.get(b).clone()].into())
        }),
    );
    let seen: Rc<RefCell<Vec<Value>>> = Rc::default();
    let sink = seen.clone();
    g.subscribe(d, Box::new(move |v, _| sink.borrow_mut().push(v.clone())));
    let base = runs.load(Ordering::SeqCst);
    for i in 1..=20i64 {
        g.commit("tick", &[(x, Value::I64(i))]).unwrap();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            base + i as usize,
            "d settles once per commit"
        );
    }
    let seen = seen.borrow();
    assert_eq!(seen.len(), 20, "one firing per commit");
    for v in seen.iter() {
        let Value::List(pair) = v else {
            panic!("not a pair")
        };
        assert_eq!(
            int(&pair[1]),
            2 * (int(&pair[0]) - 1),
            "a and b disagree: {pair:?}"
        );
    }
}

// ---------------------------------------------------------------- atomicity

pub fn atomic_rollback<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(1));
    let y = g.input(&k("model:y"), Value::I64(1));
    let echo = g.derived(
        &k("model:echo"),
        compute(move |t| Value::I64(int(t.get(y)) * 10)),
    );
    let bad = g.derived(
        &k("model:bad"),
        compute(move |t| match int(t.get(x)) {
            13 => t.fail("thirteen"),
            n => Value::I64(n),
        }),
    );
    let fired = Rc::new(RefCell::new(0));
    for n in [x, y, echo, bad] {
        let f = fired.clone();
        g.subscribe(n, Box::new(move |_, _| *f.borrow_mut() += 1));
    }
    let f = fired.clone();
    g.subscribe_commits(Box::new(move |_| *f.borrow_mut() += 1));
    g.commit("ok", &[(x, Value::I64(2))]).unwrap();
    let fired_before = *fired.borrow();

    let snap = bytes(&g);
    let (now, log_len, versions) = (
        g.now(),
        g.commit_log().len(),
        [x, y, echo, bad].map(|n| g.version(n)),
    );

    // y is written and echo recomputes fine before bad fails, so a half-applied commit would show.
    let err = g
        .commit("boom", &[(y, Value::I64(7)), (x, Value::I64(13))])
        .unwrap_err();
    assert!(
        matches!(err, CommitError::Compute { ref message, .. } if message == "thirteen"),
        "{err:?}"
    );
    assert_eq!(
        bytes(&g),
        snap,
        "snapshot is byte-identical after a failed commit"
    );
    assert_eq!(*fired.borrow(), fired_before, "no subscriber fires");
    assert_eq!((g.now(), g.commit_log().len()), (now, log_len));
    assert_eq!([x, y, echo, bad].map(|n| g.version(n)), versions);
    assert_eq!(g.read(echo), &Value::I64(10));

    // Hydrate is atomic too.
    let mut poisoned = g.snapshot();
    poisoned.inputs.insert(k("model:x"), Value::I64(13));
    assert!(matches!(
        g.hydrate(poisoned),
        Err(HydrateError::Commit(CommitError::Compute { .. }))
    ));
    assert_eq!(bytes(&g), snap);
    assert_eq!(*fired.borrow(), fired_before);

    // Bad writes are rejected before anything runs.
    assert!(matches!(
        g.commit("w", &[(echo, Value::Null)]),
        Err(CommitError::NotAnInput(_))
    ));
    assert!(matches!(
        g.commit("w", &[(x, Value::Null), (x, Value::Null)]),
        Err(CommitError::DuplicateWrite(_))
    ));
    assert_eq!(bytes(&g), snap);
}

// ---------------------------------------------------------------- time

pub fn time<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(0));
    assert_eq!(g.now(), 0);
    for i in 1..=5u64 {
        let c = g.commit("step", &[(x, Value::I64(i as i64))]).unwrap();
        assert_eq!((g.now(), c.time), (i, i), "one commit, one new time");
        assert_eq!(c.originated_at, None);
    }
    let same = g.commit("noop", &[(x, Value::I64(5))]).unwrap();
    assert_eq!(
        (g.now(), same.time, g.commit_log().len()),
        (5, 5, 5),
        "a commit that changes nothing creates no time"
    );
    assert!(g.commit("empty", &[]).unwrap().changed_nodes.is_empty());
    assert_eq!(g.now(), 5);

    let snap = g.snapshot();
    assert_eq!(snap.now, 5);
    let h = g.hydrate(snap.clone()).unwrap();
    assert_eq!(
        (g.now(), h.time, h.originated_at),
        (6, 6, Some(5)),
        "hydrate advances time even when nothing differs"
    );
    assert_eq!(g.commit_log().last(), Some(&h));

    let mut other = mk();
    let ox = other.input(&k("model:x"), Value::I64(0));
    let h = other.hydrate(snap).unwrap();
    assert_eq!(
        (other.now(), h.originated_at, h.changed_nodes),
        (1, Some(5), vec![ox])
    );
    assert_eq!(other.read(ox), &Value::I64(5));
    assert!(matches!(
        other.hydrate(aibus_graph::Snapshot {
            now: 0,
            inputs: [(k("nope"), Value::Null)].into_iter().collect()
        }),
        Err(HydrateError::UnknownKey(_))
    ));
}

// ---------------------------------------------------------------- subscriber order

pub fn subscriber_order<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let a = g.input(&k("model:a"), Value::I64(0));
    let b = g.input(&k("model:b"), Value::I64(0));
    let c = g.derived(
        &k("model:c"),
        compute(move |t| Value::I64(int(t.get(a)) + int(t.get(b)))),
    );
    let log: Log = Rc::default();
    g.subscribe(a, recorder(&log, "a1"));
    g.subscribe(b, recorder(&log, "b1"));
    let a2 = g.subscribe(a, recorder(&log, "a2"));
    g.subscribe(c, recorder(&log, "c1"));
    let l = log.clone();
    g.subscribe_commits(Box::new(move |c| {
        l.borrow_mut().push(format!("commit @{}", c.time))
    }));

    // changed_nodes is the writes in order (b, a), then the derived.
    let commit = g
        .commit("both", &[(b, Value::I64(1)), (a, Value::I64(1))])
        .unwrap();
    assert_eq!(commit.changed_nodes, vec![b, a, c]);
    assert_eq!(
        *log.borrow(),
        ["b1=1 @1", "a1=1 @1", "a2=1 @1", "c1=2 @1", "commit @1"]
    );

    log.borrow_mut().clear();
    a2.unsubscribe();
    g.commit("a only", &[(a, Value::I64(5))]).unwrap();
    assert_eq!(*log.borrow(), ["a1=5 @2", "c1=6 @2", "commit @2"]);
}

// ---------------------------------------------------------------- dynamic dependencies

pub fn dynamic_dependencies<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let flag = g.input(&k("model:flag"), Value::Bool(true));
    let a = g.input(&k("model:a"), Value::I64(1));
    let b = g.input(&k("model:b"), Value::I64(2));
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let d = g.derived(
        &k("model:d"),
        compute(move |t| {
            counted.fetch_add(1, Ordering::SeqCst);
            let pick = if *t.get(flag) == Value::Bool(true) {
                a
            } else {
                b
            };
            t.get(pick).clone()
        }),
    );
    let runs = || runs.load(Ordering::SeqCst);
    assert_eq!(g.dependencies(d), vec![flag, a]);
    let start = runs();

    g.commit("b", &[(b, Value::I64(20))]).unwrap();
    assert_eq!(runs(), start, "b is not a dependency yet");
    g.commit("flip", &[(flag, Value::Bool(false))]).unwrap();
    assert_eq!((runs(), g.read(d)), (start + 1, &Value::I64(20)));
    assert_eq!(g.dependencies(d), vec![flag, b]);
    assert!(g.dependents(a).is_empty(), "the stale edge is dropped");
    assert_eq!(g.dependents(b), vec![d]);

    g.commit("a", &[(a, Value::I64(10))]).unwrap();
    assert_eq!(runs(), start + 1, "a no longer triggers a recompute");
    g.commit("b", &[(b, Value::I64(30))]).unwrap();
    assert_eq!((runs(), g.read(d)), (start + 2, &Value::I64(30)));

    // A dependency picked up on this recompute that is itself recomputing settles first.
    let slow = g.derived(
        &k("model:slow"),
        compute(move |t| Value::I64(int(t.get(b)) + 1)),
    );
    let late = g.derived(
        &k("model:late"),
        compute(move |t| {
            if *t.get(flag) == Value::Bool(true) {
                t.get(slow).clone()
            } else {
                t.get(a).clone()
            }
        }),
    );
    g.commit("both", &[(flag, Value::Bool(true)), (b, Value::I64(99))])
        .unwrap();
    assert_eq!(g.read(late), &Value::I64(100));
}

// ---------------------------------------------------------------- equality cutoff

pub fn equality_cutoff<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(1));
    let parity_runs = Arc::new(AtomicUsize::new(0));
    let down_runs = Arc::new(AtomicUsize::new(0));
    let (p, d) = (parity_runs.clone(), down_runs.clone());
    let parity = g.derived(
        &k("model:parity"),
        compute(move |t| {
            p.fetch_add(1, Ordering::SeqCst);
            Value::I64(int(t.get(x)) % 2)
        }),
    );
    let down = g.derived(
        &k("model:down"),
        compute(move |t| {
            d.fetch_add(1, Ordering::SeqCst);
            Value::I64(int(t.get(parity)) + 10)
        }),
    );
    let log: Log = Rc::default();
    g.subscribe(parity, recorder(&log, "parity"));
    g.subscribe(down, recorder(&log, "down"));
    let (p0, d0) = (
        parity_runs.load(Ordering::SeqCst),
        down_runs.load(Ordering::SeqCst),
    );

    let c = g.commit("odd to odd", &[(x, Value::I64(3))]).unwrap();
    assert_eq!(
        parity_runs.load(Ordering::SeqCst),
        p0 + 1,
        "the node next to the change recomputes"
    );
    assert_eq!(
        down_runs.load(Ordering::SeqCst),
        d0,
        "its dependent does not: the value did not change"
    );
    assert_eq!(c.changed_nodes, vec![x]);
    assert!(log.borrow().is_empty());
    assert_eq!(g.version(parity), 0);

    g.commit("odd to even", &[(x, Value::I64(4))]).unwrap();
    assert_eq!(
        (
            parity_runs.load(Ordering::SeqCst),
            down_runs.load(Ordering::SeqCst)
        ),
        (p0 + 2, d0 + 1)
    );
    assert_eq!(g.version(down), 1);
}

// ---------------------------------------------------------------- retention

pub fn retention<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(0));
    let twice = g.derived(
        &k("model:twice"),
        compute(move |t| Value::I64(int(t.get(x)) * 2)),
    );
    let total = COMMIT_HISTORY_CAP as u64 + 100;
    for i in 1..=total {
        g.commit("step", &[(x, Value::I64(i as i64))]).unwrap();
    }
    let oldest: GraphTime = total - COMMIT_HISTORY_CAP as u64;
    assert_eq!(g.commit_log().len(), COMMIT_HISTORY_CAP);
    assert_eq!(g.commit_log()[0].time, oldest + 1);
    assert_eq!(
        g.read_at(x, 0),
        RetentionResult::Evicted {
            oldest_retained: oldest
        }
    );
    assert_eq!(
        g.read_at(x, oldest - 1),
        RetentionResult::Evicted {
            oldest_retained: oldest
        }
    );
    assert!(
        matches!(g.snapshot_at(3), RetentionResult::Evicted { oldest_retained } if oldest_retained == oldest)
    );
    assert_eq!(
        g.read_at(x, total + 1),
        RetentionResult::Future { now: total }
    );
    for t in (oldest..=total)
        .step_by(61)
        .chain([oldest, oldest + 1, total])
    {
        assert_eq!(
            g.read_at(x, t),
            RetentionResult::Found(Value::I64(t as i64)),
            "x at {t}"
        );
        assert_eq!(
            g.read_at(twice, t),
            RetentionResult::Found(Value::I64(2 * t as i64)),
            "twice at {t}"
        );
    }
    let RetentionResult::Found(snap) = g.snapshot_at(oldest) else {
        panic!("oldest is retained")
    };
    assert_eq!(
        (snap.now, snap.inputs[&k("model:x")].clone()),
        (oldest, Value::I64(oldest as i64))
    );
}

/// Before anything is evicted, history reads reach all the way back to time 0.
pub fn time_travel<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(0));
    let y = g.input(&k("model:y"), Value::I64(0));
    let sum = g.derived(
        &k("model:sum"),
        compute(move |t| Value::I64(int(t.get(x)) + int(t.get(y)))),
    );
    g.commit("one", &[(x, Value::I64(1))]).unwrap();
    g.commit("two", &[(y, Value::I64(10))]).unwrap();
    g.commit("three", &[(x, Value::I64(2))]).unwrap();
    let at = |n, t| match g.read_at(n, t) {
        RetentionResult::Found(Value::I64(v)) => v,
        other => panic!("{other:?}"),
    };
    assert_eq!([0, 1, 2, 3].map(|t| at(x, t)), [0, 1, 1, 2]);
    assert_eq!([0, 1, 2, 3].map(|t| at(sum, t)), [0, 1, 11, 12]);
    let RetentionResult::Found(s) = g.snapshot_at(2) else {
        panic!()
    };
    assert_eq!(
        (
            s.now,
            s.inputs[&k("model:x")].clone(),
            s.inputs[&k("model:y")].clone()
        ),
        (2, Value::I64(1), Value::I64(10))
    );
}

// ---------------------------------------------------------------- layering

/// Every `(model key, ctrl key)` pair where a `ctrl:` node is a transitive dependency of a
/// `model:` node among `keys`. Empty means the layering rule holds.
pub fn layering_violations<G: Graph>(g: &G, keys: &[&str]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for key in keys.iter().filter(|k| k.starts_with("model:")) {
        let mut todo = vec![g.node(key).expect("registered")];
        let mut seen = Vec::new();
        while let Some(n) = todo.pop() {
            if seen.contains(&n) {
                continue;
            }
            seen.push(n);
            let ex = g.explain(n);
            if ex.key.starts_with("ctrl:") {
                found.push((key.to_string(), ex.key.to_string()));
            }
            todo.extend(g.dependencies(n));
        }
    }
    found
}

pub fn layering<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let sel = g.input(&k("ctrl:selected"), Value::I64(0));
    let runs = g.input(&k("model:run:1:turns"), Value::I64(0));
    let tick = g.input(&k("eng:tick"), Value::I64(0));
    g.derived(
        &k("model:counts"),
        compute(move |t| Value::I64(int(t.get(runs)) + int(t.get(tick)))),
    );
    g.derived(
        &k("view:card"),
        compute(move |t| {
            Value::List(
                vec![
                    t.get(sel).clone(),
                    t.get_key("model:counts").cloned().unwrap(),
                ]
                .into(),
            )
        }),
    );
    let keys = [
        "ctrl:selected",
        "model:run:1:turns",
        "eng:tick",
        "model:counts",
        "view:card",
    ];
    assert_eq!(layering_violations(&g, &keys), vec![]);

    // The walk has to be able to fail: a model node that reads a ctrl node is a violation.
    g.derived(
        &k("model:leaky"),
        compute(move |t| Value::I64(int(t.get(sel)))),
    );
    g.derived(
        &k("model:via"),
        compute(|t| t.get_key("model:leaky").cloned().unwrap()),
    );
    let keys = ["model:counts", "model:leaky", "model:via"];
    assert_eq!(
        layering_violations(&g, &keys),
        vec![
            ("model:leaky".to_string(), "ctrl:selected".to_string()),
            ("model:via".to_string(), "ctrl:selected".to_string())
        ]
    );
}

// ---------------------------------------------------------------- explain, metadata, simulate, hydrate

pub fn explain_and_metadata<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let a = g.input(&k("model:a"), Value::I64(1));
    let b = g.input(&k("model:b"), Value::I64(1));
    let unused = g.input(&k("model:unused"), Value::I64(0));
    let mid = g.derived(
        &k("model:mid"),
        compute(move |t| Value::I64(int(t.get(a)) + int(t.get(b)))),
    );
    let top = g.derived(
        &k("view:top"),
        compute(move |t| Value::I64(int(t.get(mid)) * 2)),
    );
    let dirty = g.commit_metadata_derived(
        &k("view:dirty"),
        compute(|t| match t.commit() {
            Some(c) => Value::Str(format!("{}:{}", c.intent, c.changed_nodes.len()).into()),
            None => Value::Null,
        }),
    );
    assert_eq!(g.read(dirty), &Value::Null);
    g.commit("first", &[(a, Value::I64(5))]).unwrap();
    let e = g.explain(top);
    assert_eq!(
        (e.key.as_str(), e.value, e.changed_at, e.intent.as_str()),
        ("view:top", Value::I64(12), 1, "first")
    );
    assert_eq!((e.dependencies, e.inputs), (vec![mid], vec![a, b]));
    assert!(!g.explain(top).inputs.contains(&unused));
    assert_eq!(
        g.read(dirty),
        &Value::Str("first:3".into()),
        "a, mid and top changed"
    );
    assert!(
        g.commit_log()[0].changed_nodes.contains(&dirty),
        "the metadata node's own change is recorded"
    );

    g.commit("second", &[(unused, Value::I64(1))]).unwrap();
    assert_eq!(g.read(dirty), &Value::Str("second:1".into()));
    g.commit("same", &[(unused, Value::I64(1))]).unwrap();
    assert_eq!(
        g.read(dirty),
        &Value::Str("second:1".into()),
        "no commit, no recompute"
    );

    let h = g.hydrate(g.snapshot()).unwrap();
    assert_eq!(g.read(dirty), &Value::Str("hydrate:0".into()));
    assert_eq!(h.originated_at, Some(2));
}

pub fn simulate<G: Graph>(mk: fn() -> G) {
    let mut g = mk();
    let x = g.input(&k("model:x"), Value::I64(1));
    let twice = g.derived(
        &k("model:twice"),
        compute(move |t| Value::I64(int(t.get(x)) * 2)),
    );
    g.derived(
        &k("model:boom"),
        compute(move |t| {
            if int(t.get(x)) > 100 {
                t.fail("too big")
            } else {
                Value::Null
            }
        }),
    );
    let log: Log = Rc::default();
    g.subscribe(twice, recorder(&log, "twice"));
    let before = bytes(&g);
    let SimulateResult::Applied { time, changed } = g.simulate("what if", &[(x, Value::I64(4))])
    else {
        panic!("rejected")
    };
    assert_eq!(
        (time, changed.clone()),
        (1, vec![(x, Value::I64(4)), (twice, Value::I64(8))])
    );
    assert_eq!(
        (bytes(&g), g.now(), log.borrow().len(), g.commit_log().len()),
        (before, 0, 0, 0)
    );
    assert!(matches!(
        g.simulate("big", &[(x, Value::I64(101))]),
        SimulateResult::Rejected(CommitError::Compute { .. })
    ));
    let SimulateResult::Applied { time, changed } = g.simulate("same", &[(x, Value::I64(1))])
    else {
        panic!()
    };
    assert_eq!((time, changed.len()), (0, 0));

    let c = g.commit("what if", &[(x, Value::I64(4))]).unwrap();
    assert_eq!(c.changed_nodes, vec![x, twice]);
    assert_eq!(g.read(twice), &Value::I64(8));
}

pub fn hydrate_round_trip<G: Graph>(mk: fn() -> G) {
    let make = || {
        let mut g = mk();
        let x = g.input(&k("model:x"), Value::I64(0));
        let h = g.input(
            &k("model:h"),
            Value::Handle(aibus_graph::StoreHandle {
                store: "transcript".into(),
                id: 1,
                version: 0,
            }),
        );
        let d = g.derived(
            &k("model:d"),
            compute(move |t| Value::I64(int(t.get(x)) + 1)),
        );
        (g, x, h, d)
    };
    let (mut a, x, h, _) = make();
    let handle = Value::Handle(aibus_graph::StoreHandle {
        store: "transcript".into(),
        id: 1,
        version: 7,
    });
    a.commit("w", &[(x, Value::I64(41)), (h, handle.clone())])
        .unwrap();
    let wire = serde_json::to_string(&a.snapshot()).unwrap();
    let (mut b, _, h2, d2) = make();
    b.hydrate(serde_json::from_str(&wire).unwrap()).unwrap();
    assert_eq!((b.read(d2), b.read(h2)), (&Value::I64(42), &handle));
    assert_eq!(b.snapshot().inputs, a.snapshot().inputs);
    assert_eq!((b.now(), b.commit_log().len()), (1, 1));
}
