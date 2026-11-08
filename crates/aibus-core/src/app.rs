//! The Elm loop (02 section 3, 03 section 6): `step` commits exactly once per `Msg`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aibus_graph::{Commit, Graph, Node, NodeKey, Snapshot, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::effects::Effects;
use crate::error::AibusError;
use crate::msg::Msg;
use crate::persist::commits::CommitRecord;
use crate::persist::time::rfc3339;
use crate::orchestrator::Limits;
use crate::state::reduce::{reduce, Env};
use crate::state::store::Store;
use crate::state::{self, register_run, router_last, ANIMATING, PERMISSIONS_BYPASS, SHUTDOWN};

/// Capacity of the `Msg` channel (08 section 3).
pub const MSG_CAPACITY: usize = 4096;

/// The tick timer. It sends `Msg::Tick` only while animating and drops (counting) when the
/// channel is full, so a stalled loop cannot be flooded by it.
pub struct Ticker {
    animating: watch::Sender<bool>,
    dropped: Arc<AtomicU64>,
    task: JoinHandle<()>,
}

impl Ticker {
    /// Must be called inside a tokio runtime.
    pub fn spawn(tx: mpsc::Sender<Msg>, hz: u32) -> Self {
        let (animating, mut rx) = watch::channel(false);
        let dropped = Arc::new(AtomicU64::new(0));
        let count = dropped.clone();
        let period = Duration::from_secs_f64(1.0 / f64::from(hz.max(1)));
        let task = tokio::spawn(async move {
            loop {
                if rx.wait_for(|on| *on).await.is_err() {
                    return;
                }
                let mut timer = tokio::time::interval(period);
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = timer.tick() => match tx.try_send(Msg::Tick) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                count.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                        },
                        changed = rx.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            if !*rx.borrow() {
                                break;
                            }
                        }
                    }
                }
            }
        });
        Self {
            animating,
            dropped,
            task,
        }
    }

    pub fn set_animating(&self, on: bool) {
        self.animating
            .send_if_modified(|cur| std::mem::replace(cur, on) != on);
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct App<G: Graph> {
    pub graph: G,
    pub store: Store,
    pub effects: Effects,
    /// Nodes changed since the widgets last drew. `aibus-tui` maps them to `view:dirty`.
    pub dirty: Vec<Node>,
    /// Every key registered so far (for the layering test and `explain`).
    pub keys: Vec<NodeKey>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    ticker: Option<Ticker>,
    /// Every commit that changed an input, since the last `take_journal`. `None` until
    /// `record_commits`.
    journal: Option<Vec<CommitRecord>>,
    now: Box<dyn Fn() -> u64>,
}

impl<G: Graph> App<G> {
    /// Registers every static node. `now` is the wall clock in milliseconds.
    pub fn new(mut graph: G, now: impl Fn() -> u64 + 'static) -> Self {
        let keys = state::register(&mut graph);
        let (tx, rx) = mpsc::channel(MSG_CAPACITY);
        Self {
            graph,
            store: Store::new(),
            effects: Effects::idle(),
            dirty: Vec::new(),
            keys,
            tx,
            rx,
            ticker: None,
            journal: None,
            now: Box::new(now),
        }
    }

    pub fn sender(&self) -> mpsc::Sender<Msg> {
        self.tx.clone()
    }

    /// Turns on `model:permissions:bypass`. This is startup configuration, not a message, so it
    /// is one commit of its own before the first `step`. Callers go through
    /// `permission::bypass_allowed` first.
    pub fn enable_bypass(&mut self) -> Result<(), AibusError> {
        let node = self
            .graph
            .node(PERMISSIONS_BYPASS)
            .ok_or_else(|| std::io::Error::other("bypass key is not registered"))?;
        self.graph
            .commit("BypassEnabled", &[(node, Value::Bool(true))])
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }

    /// Writes the session's limits (config with the `AIBUS_MAX_*` overrides) to `model:limits`,
    /// where `runnable` reads them. Startup configuration, one commit of its own.
    pub fn set_limits(&mut self, limits: &Limits) -> Result<(), AibusError> {
        let node = self
            .graph
            .node(state::LIMITS)
            .ok_or_else(|| std::io::Error::other("limits key is not registered"))?;
        self.graph
            .commit("LimitsSet", &[(node, limits.to_value())])
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }

    /// Starts the 30 Hz timer. Inside a tokio runtime only.
    pub fn start_ticker(&mut self, hz: u32) {
        self.ticker = Some(Ticker::spawn(self.tx.clone(), hz));
    }

    pub fn should_quit(&self) -> bool {
        !matches!(self.graph.read_key(SHUTDOWN), None | Some(Value::Null))
    }

    /// Registers the nodes a key names, if it is one that appears at run time.
    fn register_dynamic(&mut self, key: &str) -> Result<(), AibusError> {
        let new = if let Some(rest) = key.strip_prefix("model:run:") {
            let id = rest.split(':').next().unwrap_or("");
            register_run(&mut self.graph, id)
        } else if let Some(kind) = key.strip_prefix("model:router:last:") {
            let k = router_last(kind);
            self.graph.input(&k, Value::Null);
            vec![k]
        } else {
            return Err(std::io::Error::other(format!("write to unregistered key {key}")).into());
        };
        self.keys.extend(new);
        Ok(())
    }

    /// From now on every commit that changes an input is kept for `take_journal`, which is what
    /// the commit log (07 section 2.1) is written from.
    pub fn record_commits(&mut self) {
        self.journal.get_or_insert_with(Vec::new);
    }

    pub fn take_journal(&mut self) -> Vec<CommitRecord> {
        self.journal.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Registers any run keys the writes name, commits them under `intent` and journals them.
    fn commit_writes(
        &mut self,
        intent: &str,
        writes: Vec<(NodeKey, Value)>,
    ) -> Result<Commit, AibusError> {
        let mut resolved = Vec::with_capacity(writes.len());
        for (key, value) in &writes {
            if self.graph.node(key).is_none() {
                self.register_dynamic(key)?;
            }
            let node = self
                .graph
                .node(key)
                .ok_or_else(|| std::io::Error::other(format!("key {key} did not register")))?;
            resolved.push((node, value.clone()));
        }
        // One span per commit for `--trace` (08 section 6); a disabled span costs a load.
        let span = tracing::trace_span!(
            target: "aibus_trace",
            "commit",
            intent = intent,
            changed = tracing::field::Empty,
            recompute_us = tracing::field::Empty,
        );
        let began = (!span.is_disabled()).then(std::time::Instant::now);
        let commit = self
            .graph
            .commit(intent, &resolved)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(t0) = began {
            span.record("changed", commit.changed_nodes.len() as u64);
            span.record("recompute_us", t0.elapsed().as_micros() as u64);
        }
        drop(span);
        self.dirty.extend(commit.changed_nodes.iter().copied());
        if let Some(journal) = &mut self.journal {
            let changed: Vec<(NodeKey, Value)> = writes
                .into_iter()
                .zip(&resolved)
                .filter(|(_, (node, _))| commit.changed_nodes.contains(node))
                .map(|(w, _)| w)
                .collect();
            if !changed.is_empty() {
                journal.push(CommitRecord {
                    time: commit.time,
                    intent: intent.into(),
                    changed,
                    at: rfc3339(commit.at),
                });
            }
        }
        Ok(commit)
    }

    /// One message: reduce, one commit, then the effects. An error leaves the graph untouched.
    pub fn step(&mut self, msg: Msg) -> Result<(), AibusError> {
        let env = Env {
            now_ms: (self.now)(),
            ticks_dropped: self.ticker.as_ref().map_or(0, Ticker::dropped),
        };
        let writes = reduce(&self.graph, &self.store, &msg, env)?;
        let commit = self.commit_writes(msg.name(), writes)?;
        if let (Some(t), Some(Value::Bool(on))) = (&self.ticker, self.graph.read_key(ANIMATING)) {
            t.set_animating(*on);
        }
        self.effects.after_commit(&self.graph, &commit);
        Ok(())
    }

    /// Loads a snapshot, replays the logged commits that came after it, then runs the effects
    /// once so queued work starts again.
    pub fn hydrate(&mut self, snap: Snapshot, tail: &[CommitRecord]) -> Result<(), AibusError> {
        let after = snap.now;
        let run_keys: Vec<NodeKey> = snap
            .inputs
            .keys()
            .filter(|k| k.starts_with("model:run:") || k.starts_with("model:router:last:"))
            .cloned()
            .collect();
        for k in run_keys {
            if self.graph.node(&k).is_none() {
                self.register_dynamic(&k)?;
            }
        }
        let mut commit = self
            .graph
            .hydrate(snap)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        for rec in tail.iter().filter(|r| r.time > after) {
            commit = self.commit_writes(&rec.intent, rec.changed.clone())?;
        }
        self.effects.after_commit(&self.graph, &commit);
        Ok(())
    }

    /// Drains the channel until `Shutdown` or `Quit`, or every sender is gone.
    pub async fn run(&mut self) -> Result<(), AibusError> {
        while let Some(msg) = self.rx.recv().await {
            self.step(msg)?;
            if self.should_quit() {
                break;
            }
        }
        Ok(())
    }
}
