//! The in-house engine behind `Graph` (D-02): staged Kahn recompute, tracked reads, equality
//! cutoff, a bounded commit history, snapshot as serde.

mod engine;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::{
    Commit, CommitError, CommitObserver, Compute, Explanation, Graph, GraphTime, HydrateError,
    Node, NodeKey, NodeKind, NodeObserver, RetentionResult, SimulateResult, Snapshot, Unsubscribe,
    Value, COMMIT_HISTORY_CAP, NULL,
};
use engine::{run_compute, Stage, Staged};

/// Evicted entries are dropped from the front of the vectors in batches this big.
const COMPACT_EVERY: usize = COMMIT_HISTORY_CAP / 8;

struct Slot {
    key: NodeKey,
    kind: NodeKind,
    compute: Option<Compute>,
    value: Value,
    /// The value at `MiniGraph::base_time`, the oldest time `read_at` can answer.
    base: Value,
    version: u32,
    deps: Vec<Node>,
    dependents: Vec<Node>,
    changed_at: GraphTime,
    intent: NodeKey,
    observers: Vec<(Arc<AtomicBool>, NodeObserver)>,
}

#[derive(Default)]
pub struct MiniGraph {
    slots: Vec<Slot>,
    by_key: HashMap<NodeKey, Node>,
    /// Commit-metadata deriveds in registration order.
    metadata: Vec<Node>,
    now: GraphTime,
    /// `commits[start..]` is the retained window; `changes` runs in step with it.
    commits: Vec<Commit>,
    changes: Vec<Vec<(Node, Value)>>,
    start: usize,
    base_time: GraphTime,
    commit_observers: Vec<(Arc<AtomicBool>, CommitObserver)>,
}

impl MiniGraph {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, node: Node) -> Option<&Slot> {
        self.slots.get(node.idx())
    }

    fn add(
        &mut self,
        key: &NodeKey,
        kind: NodeKind,
        compute: Option<Compute>,
        value: Value,
    ) -> Node {
        let node = Node(self.slots.len() as u32);
        self.by_key.insert(key.clone(), node);
        self.slots.push(Slot {
            key: key.clone(),
            kind,
            compute,
            base: value.clone(),
            value,
            version: 0,
            deps: Vec::new(),
            dependents: Vec::new(),
            changed_at: 0,
            intent: NodeKey::default(),
            observers: Vec::new(),
        });
        node
    }

    /// Shared by `derived` and `commit_metadata_derived`: evaluate once against the current values.
    fn add_computed(&mut self, key: &NodeKey, kind: NodeKind, compute: Compute) -> Node {
        if let Some(&existing) = self.by_key.get(key) {
            return existing; // first registration wins
        }
        let read = |n: Node| self.slot(n).map(|s| &s.value);
        let (value, tracked) = run_compute(self, &compute, &read, None);
        let value = if tracked.error.is_some() {
            Value::Null
        } else {
            value
        };
        let node = self.add(key, kind, Some(compute), value);
        if kind == NodeKind::CommitMetadata {
            self.metadata.push(node);
        }
        self.set_deps(node, tracked.deps);
        node
    }

    fn set_deps(&mut self, node: Node, new: Vec<Node>) {
        let old = std::mem::replace(&mut self.slots[node.idx()].deps, new.clone());
        for d in old.iter().filter(|d| !new.contains(d)) {
            self.slots[d.idx()].dependents.retain(|x| *x != node);
        }
        for d in new.iter().filter(|d| !old.contains(d)) {
            self.slots[d.idx()].dependents.push(node);
        }
    }

    /// Record a staged commit, evict past the history cap, then fire subscribers.
    fn apply(&mut self, staged: Staged) -> Commit {
        let Staged {
            commit,
            mut values,
            deps,
        } = staged;
        for (n, new) in deps {
            self.set_deps(n, new);
        }
        let mut changes = Vec::with_capacity(commit.changed_nodes.len());
        for &n in &commit.changed_nodes {
            let value = values.remove(&n).expect("changed node has a staged value");
            let slot = &mut self.slots[n.idx()];
            slot.value = value.clone();
            slot.version += 1;
            slot.changed_at = commit.time;
            slot.intent = commit.intent.clone();
            changes.push((n, value));
        }
        self.now = commit.time;
        // Grow in small exact steps: doubling would hold up to twice the window in capacity.
        if self.commits.len() == self.commits.capacity() {
            self.commits.reserve_exact(COMPACT_EVERY);
            self.changes.reserve_exact(COMPACT_EVERY);
        }
        self.commits.push(commit.clone());
        self.changes.push(changes);
        while self.commits.len() - self.start > COMMIT_HISTORY_CAP {
            for (n, value) in std::mem::take(&mut self.changes[self.start]) {
                self.slots[n.idx()].base = value;
            }
            self.base_time = self.commits[self.start].time;
            self.start += 1;
        }
        if self.start >= COMPACT_EVERY {
            self.commits.drain(..self.start);
            self.changes.drain(..self.start);
            self.start = 0;
        }

        // Node observers in changed_nodes order, each node's in insertion order, then commit observers.
        for n in &commit.changed_nodes {
            let slot = &mut self.slots[n.idx()];
            slot.observers
                .retain(|(gone, _)| !gone.load(Ordering::Relaxed));
            for (gone, observer) in slot.observers.iter_mut() {
                if !gone.load(Ordering::Relaxed) {
                    observer(&slot.value, &commit);
                }
            }
        }
        self.commit_observers
            .retain(|(gone, _)| !gone.load(Ordering::Relaxed));
        for (gone, observer) in self.commit_observers.iter_mut() {
            if !gone.load(Ordering::Relaxed) {
                observer(&commit);
            }
        }
        commit
    }

    /// Input values as of `t` (which must be inside the retained window).
    fn inputs_at(&self, t: GraphTime) -> IndexMap<NodeKey, Value> {
        let mut values: HashMap<Node, &Value> = HashMap::new();
        let upto = self.start + (t - self.base_time) as usize;
        for changes in &self.changes[self.start..upto] {
            values.extend(changes.iter().map(|(n, v)| (*n, v)));
        }
        let mut inputs = IndexMap::new();
        for (i, slot) in self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == NodeKind::Input)
        {
            let value = values.get(&Node(i as u32)).copied().unwrap_or(&slot.base);
            inputs.insert(slot.key.clone(), value.clone());
        }
        inputs
    }

    /// `Some` when `t` is outside the window `read_at` / `snapshot_at` can answer.
    fn miss<T>(&self, t: GraphTime) -> Option<RetentionResult<T>> {
        if t > self.now {
            Some(RetentionResult::Future { now: self.now })
        } else if t < self.base_time {
            Some(RetentionResult::Evicted {
                oldest_retained: self.base_time,
            })
        } else {
            None
        }
    }
}

fn watch<F>(list: &mut Vec<(Arc<AtomicBool>, F)>, observer: F) -> Unsubscribe {
    let gone = Arc::new(AtomicBool::new(false));
    list.push((gone.clone(), observer));
    Unsubscribe(gone)
}

impl Graph for MiniGraph {
    fn input(&mut self, key: &NodeKey, initial: Value) -> Node {
        match self.by_key.get(key) {
            Some(&existing) => existing, // first registration wins
            None => self.add(key, NodeKind::Input, None, initial),
        }
    }

    fn derived(&mut self, key: &NodeKey, compute: Compute) -> Node {
        self.add_computed(key, NodeKind::Derived, compute)
    }

    fn commit_metadata_derived(&mut self, key: &NodeKey, compute: Compute) -> Node {
        self.add_computed(key, NodeKind::CommitMetadata, compute)
    }

    fn commit(&mut self, intent: &str, writes: &[(Node, Value)]) -> Result<Commit, CommitError> {
        let staged = Stage::run(self, intent, writes, None, false)?;
        // No change means no new time: one commit creates exactly one time or zero.
        if staged.commit.time == self.now {
            return Ok(staged.commit);
        }
        Ok(self.apply(staged))
    }

    fn read(&self, node: Node) -> &Value {
        self.slot(node).map_or(&NULL, |s| &s.value)
    }

    fn read_key(&self, key: &str) -> Option<&Value> {
        self.by_key.get(key).map(|n| self.read(*n))
    }

    fn subscribe(&mut self, node: Node, observer: NodeObserver) -> Unsubscribe {
        watch(&mut self.slots[node.idx()].observers, observer)
    }

    fn subscribe_commits(&mut self, observer: CommitObserver) -> Unsubscribe {
        watch(&mut self.commit_observers, observer)
    }

    fn explain(&self, node: Node) -> Explanation {
        let slot = &self.slots[node.idx()];
        let mut inputs = BTreeSet::new();
        let mut todo = vec![node];
        let mut seen = BTreeSet::new();
        while let Some(n) = todo.pop() {
            if !seen.insert(n) {
                continue;
            }
            let s = &self.slots[n.idx()];
            if s.kind == NodeKind::Input {
                inputs.insert(n);
            }
            todo.extend(&s.deps);
        }
        Explanation {
            node,
            key: slot.key.clone(),
            kind: slot.kind,
            value: slot.value.clone(),
            version: slot.version,
            changed_at: slot.changed_at,
            intent: slot.intent.clone(),
            dependencies: slot.deps.clone(),
            inputs: inputs.into_iter().collect(),
        }
    }

    fn commit_log(&self) -> &[Commit] {
        &self.commits[self.start..]
    }

    fn snapshot(&self) -> Snapshot {
        let inputs = self.slots.iter().filter(|s| s.kind == NodeKind::Input);
        Snapshot {
            now: self.now,
            inputs: inputs.map(|s| (s.key.clone(), s.value.clone())).collect(),
        }
    }

    fn hydrate(&mut self, snap: Snapshot) -> Result<Commit, HydrateError> {
        let mut writes = Vec::with_capacity(snap.inputs.len());
        for (key, value) in snap.inputs {
            let node = *self
                .by_key
                .get(&key)
                .ok_or_else(|| HydrateError::UnknownKey(key.clone()))?;
            if self.slots[node.idx()].kind != NodeKind::Input {
                return Err(HydrateError::NotAnInput(key));
            }
            writes.push((node, value));
        }
        // Hydrate always advances time by one, even when every input already matches.
        let staged = Stage::run(self, "hydrate", &writes, Some(snap.now), true)
            .map_err(HydrateError::Commit)?;
        Ok(self.apply(staged))
    }

    fn read_at(&self, node: Node, t: GraphTime) -> RetentionResult<Value> {
        if let Some(miss) = self.miss(t) {
            return miss;
        }
        let Some(slot) = self.slot(node) else {
            return RetentionResult::Found(NULL.clone());
        };
        let upto = self.start + (t - self.base_time) as usize;
        let latest = self.changes[self.start..upto]
            .iter()
            .rev()
            .find_map(|c| c.iter().find(|(n, _)| *n == node));
        RetentionResult::Found(latest.map_or(&slot.base, |(_, v)| v).clone())
    }

    fn snapshot_at(&self, t: GraphTime) -> RetentionResult<Snapshot> {
        if let Some(miss) = self.miss(t) {
            return miss;
        }
        RetentionResult::Found(Snapshot {
            now: t,
            inputs: self.inputs_at(t),
        })
    }

    fn simulate(&self, intent: &str, writes: &[(Node, Value)]) -> SimulateResult {
        match Stage::run(self, intent, writes, None, false) {
            Err(e) => SimulateResult::Rejected(e),
            Ok(mut staged) => {
                let changed = staged
                    .commit
                    .changed_nodes
                    .iter()
                    .map(|n| {
                        (
                            *n,
                            staged
                                .values
                                .remove(n)
                                .expect("changed node has a staged value"),
                        )
                    })
                    .collect();
                SimulateResult::Applied {
                    time: staged.commit.time,
                    changed,
                }
            }
        }
    }

    fn dependencies(&self, node: Node) -> Vec<Node> {
        self.slot(node).map(|s| s.deps.clone()).unwrap_or_default()
    }

    fn dependents(&self, node: Node) -> Vec<Node> {
        self.slot(node)
            .map(|s| s.dependents.clone())
            .unwrap_or_default()
    }

    fn now(&self) -> GraphTime {
        self.now
    }

    fn node(&self, key: &str) -> Option<Node> {
        self.by_key.get(key).copied()
    }

    fn version(&self, node: Node) -> u32 {
        self.slot(node).map_or(0, |s| s.version)
    }
}
