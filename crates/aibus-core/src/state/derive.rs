//! The engine-side deriveds (03 section 5, 7). Pure: they read through the tracker only.

use aibus_graph::{Compute, Graph, NodeKey, Tracker, Value};

use super::store::Store;
use super::value::{as_i64, as_list, as_str, get, list, map, n, s};
use super::*;
use crate::run::RunStatus;

fn status_of(t: &mut Tracker, id: &str) -> Option<RunStatus> {
    as_str(t.get_key(&run_key(id, "status"))?).and_then(RunStatus::parse)
}

fn limit(t: &mut Tracker, name: &str, default: i64) -> i64 {
    t.get_key(LIMITS)
        .and_then(|l| get(l, name))
        .and_then(as_i64)
        .unwrap_or(default)
}

/// The slots one account has: its own entry under `per_account`, else `max_per_account`.
fn account_limit(t: &mut Tracker, account: &str, default: i64) -> i64 {
    t.get_key(LIMITS)
        .and_then(|l| get(l, "per_account"))
        .and_then(|m| get(m, account))
        .and_then(as_i64)
        .unwrap_or(default)
}

/// Queued runs that may start now (01 section 4.4): the parent is alive, the depth is within
/// `max_spawn_depth`, fewer than `max_concurrent_runs` are active, the account has a free slot
/// and the session has not used up `max_runs_per_session`.
fn runnable(t: &mut Tracker) -> Value {
    let order: Vec<Value> = t
        .get_key(RUNS_ORDER)
        .map(|v| as_list(v).to_vec())
        .unwrap_or_default();
    let max_depth = limit(t, "max_spawn_depth", 3);
    let max_conc = limit(t, "max_concurrent_runs", 8);
    let max_acct = limit(t, "max_per_account", 4);
    let max_total = limit(t, "max_runs_per_session", 500);

    let mut statuses = Vec::with_capacity(order.len());
    let mut active = 0i64;
    let mut per_account: std::collections::HashMap<String, i64> = Default::default();
    for id in order.iter().filter_map(as_str) {
        let st = status_of(t, id);
        if st.is_some_and(RunStatus::is_active) {
            active += 1;
            let meta = t.get_key(&run_key(id, "meta"));
            if let Some(a) = meta.and_then(|m| get(m, "account")).and_then(as_str) {
                if !a.is_empty() {
                    *per_account.entry(a.to_owned()).or_default() += 1;
                }
            }
        }
        statuses.push((id, st));
    }

    // Runs that left the queue (started, or ended without starting) count against the total.
    let mut used = statuses
        .iter()
        .filter(|(_, st)| *st != Some(RunStatus::Queued))
        .count() as i64;
    let mut out = Vec::new();
    for (id, st) in &statuses {
        if *st != Some(RunStatus::Queued) || active >= max_conc || used >= max_total {
            continue;
        }
        let Some(meta) = t.get_key(&run_key(id, "meta")).cloned() else {
            continue;
        };
        let meta = &meta;
        let depth = get(meta, "depth").and_then(as_i64).unwrap_or(0);
        if depth > max_depth {
            continue;
        }
        let parent = get(meta, "parent").and_then(as_str);
        if let Some(p) = parent {
            let alive = statuses
                .iter()
                .any(|(pid, pst)| *pid == p && pst.is_some_and(RunStatus::is_active));
            if !alive {
                continue;
            }
        }
        let account = get(meta, "account").and_then(as_str).unwrap_or("");
        if !account.is_empty()
            && per_account.get(account).copied().unwrap_or(0) >= account_limit(t, account, max_acct)
        {
            continue;
        }
        let field = |k: &str| get(meta, k).cloned().unwrap_or(Value::Null);
        out.push(map([
            ("run", s(id)),
            ("agent", field("agent")),
            ("parent", field("parent")),
            ("depth", field("depth")),
            ("background", field("background")),
            ("isolation", field("isolation")),
            ("prompt", field("prompt")),
            ("model", field("model")),
            ("account", field("account")),
        ]));
        active += 1;
        used += 1;
        if !account.is_empty() {
            *per_account.entry(account.to_owned()).or_default() += 1;
        }
    }
    list(out)
}

/// `done` is Completed or Partial; `failed` is Failed, Cancelled or Denied.
fn counts(t: &mut Tracker) -> Value {
    let order: Vec<Value> = t
        .get_key(RUNS_ORDER)
        .map(|v| as_list(v).to_vec())
        .unwrap_or_default();
    let (mut running, mut queued, mut done, mut failed) = (0, 0, 0, 0);
    for id in order.iter().filter_map(as_str) {
        match status_of(t, id) {
            Some(RunStatus::Queued) => queued += 1,
            Some(RunStatus::Completed | RunStatus::Partial) => done += 1,
            Some(RunStatus::Failed | RunStatus::Cancelled | RunStatus::Denied) => failed += 1,
            Some(_) => running += 1,
            None => {}
        }
    }
    map([
        ("running", n(running)),
        ("queued", n(queued)),
        ("done", n(done)),
        ("failed", n(failed)),
    ])
}

fn checkpoint_pending(t: &mut Tracker) -> Value {
    t.get_key(ADVISOR_STATE)
        .and_then(|a| get(a, "active_checkpoint"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Due after 64 messages since the last snapshot, or 30 s with at least one message.
fn persist_due(t: &mut Tracker) -> Value {
    let mut num = |k: &str| t.get_key(k).and_then(as_i64).unwrap_or(0);
    let (seq, snap_seq, clock, snap_ms) = (
        num(MSG_SEQ),
        num(SNAPSHOT_SEQ),
        num(CLOCK_MS),
        num(SNAPSHOT_MS),
    );
    let since = seq - snap_seq;
    Value::Bool(since >= PERSIST_EVERY_MSGS || (since > 0 && clock - snap_ms >= PERSIST_EVERY_MS))
}

/// True while a run is starting, running or compacting. Ticks are sent only while it is.
/// `aibus-tui` extends this with its own animations by registering its own node (a key can
/// only be registered once, so it cannot replace this one).
fn animating(t: &mut Tracker) -> Value {
    let order: Vec<Value> = t
        .get_key(RUNS_ORDER)
        .map(|v| as_list(v).to_vec())
        .unwrap_or_default();
    let any = order.iter().filter_map(as_str).any(|id| {
        matches!(
            status_of(t, id),
            Some(RunStatus::Starting | RunStatus::Running | RunStatus::Compacting)
        )
    });
    Value::Bool(any)
}

pub fn register<G: Graph>(g: &mut G) -> Vec<NodeKey> {
    let list: [(&str, Compute); 5] = [
        (ORCH_RUNNABLE, Box::new(runnable)),
        (ORCH_COUNTS, Box::new(counts)),
        (CHECKPOINT_PENDING, Box::new(checkpoint_pending)),
        (PERSIST_DUE, Box::new(persist_due)),
        (ANIMATING, Box::new(animating)),
    ];
    list.into_iter()
        .map(|(key, compute)| {
            let key = NodeKey::from(key);
            g.derived(&key, compute);
            key
        })
        .collect()
}

/// A transcript's row counts at `width` as a derived: the wrapped-line cache lives in the store
/// and only the tail is walked. Reads both handles through the tracker so a seal or an append
/// is the change signal. `aibus-tui` builds `view:transcript:<run>:lines` on the same shape.
pub fn transcript_rows(store: Store, run: String, width: usize) -> Compute {
    Box::new(move |t| {
        let blocks = t.get_key(&run_key(&run, "blocks"));
        let tail = t.get_key(&run_key(&run, "tail"));
        let id = match (blocks, tail) {
            (Some(Value::Handle(h)), Some(Value::Handle(_))) => h.id,
            _ => return Value::Null,
        };
        let (sealed, tail_rows) = store.rows(id, width);
        map([
            ("rows", n(sealed + tail_rows)),
            ("sealed_rows", n(sealed)),
            ("tail_rows", n(tail_rows)),
        ])
    })
}
