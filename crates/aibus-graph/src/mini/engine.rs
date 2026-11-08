//! One commit worth of recompute, staged without touching the graph.
//!
//! `Stage::run` is shared by `commit`, `hydrate` and `simulate`. Nothing is written to the graph
//! until the caller applies the result, so a failing compute needs no rollback at all.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::SystemTime;

use super::MiniGraph;
use crate::{
    Commit, CommitError, Compute, GraphTime, Node, NodeKind, Tracked, Tracker, Value, NULL,
};

/// A finished recompute: the commit to record, the new values and the new dependency lists.
pub(super) struct Staged {
    pub commit: Commit,
    pub values: HashMap<Node, Value>,
    pub deps: Vec<(Node, Vec<Node>)>,
}

pub(super) struct Stage<'g> {
    g: &'g MiniGraph,
    values: HashMap<Node, Value>,
    changed: Vec<Node>,
    /// Affected deriveds not settled yet. A read of one of these blocks the reader.
    pending: HashSet<Node>,
    running: HashSet<Node>,
    deps: Vec<(Node, Vec<Node>)>,
    metadata_pass: bool,
}

/// Run `compute` against `read`; also used to evaluate a derived at registration time.
pub(super) fn run_compute<'a>(
    g: &'a MiniGraph,
    compute: &Compute,
    read: &'a dyn Fn(Node) -> Option<&'a Value>,
    commit: Option<&'a Commit>,
) -> (Value, Tracked) {
    let mut tracker = Tracker::new(read, &g.by_key, commit);
    let value = compute(&mut tracker);
    (value, tracker.finish())
}

impl<'g> Stage<'g> {
    pub fn run(
        g: &'g MiniGraph,
        intent: &str,
        writes: &[(Node, Value)],
        originated_at: Option<GraphTime>,
        force_time: bool,
    ) -> Result<Staged, CommitError> {
        let mut st = Stage {
            g,
            values: HashMap::new(),
            changed: Vec::new(),
            pending: HashSet::new(),
            running: HashSet::new(),
            deps: Vec::new(),
            metadata_pass: false,
        };
        let mut written = HashSet::new();
        for (n, v) in writes {
            let slot = g.slots.get(n.idx()).ok_or(CommitError::UnknownNode(*n))?;
            if slot.kind != NodeKind::Input {
                return Err(CommitError::NotAnInput(*n));
            }
            if !written.insert(*n) {
                return Err(CommitError::DuplicateWrite(*n));
            }
            // Equality cutoff at the edge: writing the current value changes nothing.
            if *v != slot.value {
                st.values.insert(*n, v.clone());
                st.changed.push(*n);
            }
        }
        st.kahn()?;

        let advances = force_time || !st.changed.is_empty();
        let mut commit = Commit {
            time: if advances { g.now + 1 } else { g.now },
            intent: intent.into(),
            originated_at,
            changed_nodes: Vec::new(),
            at: SystemTime::now(),
        };
        if advances {
            commit.changed_nodes = st.changed.clone();
            st.metadata_pass(&commit)?;
        }
        commit.changed_nodes = st.changed;
        Ok(Staged {
            commit,
            values: st.values,
            deps: st.deps,
        })
    }

    /// Recompute the affected deriveds in topological order (Kahn), skipping any whose
    /// dependencies did not actually change.
    fn kahn(&mut self) -> Result<(), CommitError> {
        let g = self.g;
        let mut affected = Vec::new();
        let mut seen = HashSet::new();
        let mut todo = self.changed.clone();
        while let Some(n) = todo.pop() {
            for &m in &g.slots[n.idx()].dependents {
                if g.slots[m.idx()].kind == NodeKind::Derived && seen.insert(m) {
                    affected.push(m);
                    todo.push(m);
                }
            }
        }
        affected.sort_unstable();
        self.pending = affected.iter().copied().collect();
        let mut indegree: HashMap<Node, usize> = affected
            .iter()
            .map(|m| {
                (
                    *m,
                    g.slots[m.idx()]
                        .deps
                        .iter()
                        .filter(|d| self.pending.contains(d))
                        .count(),
                )
            })
            .collect();
        let mut queue: VecDeque<Node> = affected
            .iter()
            .copied()
            .filter(|m| indegree[m] == 0)
            .collect();
        while let Some(n) = queue.pop_front() {
            self.settle(n)?;
            for m in &g.slots[n.idx()].dependents {
                if let Some(left) = indegree.get_mut(m) {
                    *left -= 1;
                    if *left == 0 {
                        queue.push_back(*m);
                    }
                }
            }
        }
        Ok(())
    }

    /// Settle `n`. A compute that reads a node that is still pending (a dependency it only
    /// picked up on this recompute) blocks; that node is settled first and the compute retried.
    fn settle(&mut self, n: Node) -> Result<(), CommitError> {
        if !self.pending.contains(&n) {
            return Ok(());
        }
        let g = self.g;
        let slot = &g.slots[n.idx()];
        if !self.running.insert(n) {
            return Err(CommitError::Cycle(slot.key.clone()));
        }
        for &d in &slot.deps {
            self.settle(d)?;
        }
        if slot.deps.iter().any(|d| self.values.contains_key(d)) {
            let compute = slot.compute.as_ref().expect("derived has a compute");
            loop {
                let this = &*self;
                let read = |n: Node| this.read(n);
                let (value, tracked) = run_compute(g, compute, &read, None);
                if let Some(message) = tracked.error {
                    return Err(CommitError::Compute {
                        key: slot.key.clone(),
                        message,
                    });
                }
                if let Some(b) = tracked.blocked {
                    if g.slots[b.idx()].kind == NodeKind::CommitMetadata {
                        return Err(CommitError::MetadataDependency(
                            g.slots[b.idx()].key.clone(),
                        ));
                    }
                    self.settle(b)?;
                    continue;
                }
                self.store(n, value, tracked.deps);
                break;
            }
        }
        self.running.remove(&n);
        self.pending.remove(&n);
        Ok(())
    }

    /// Commit-metadata deriveds run last, on every advancing commit, and see the provisional commit.
    fn metadata_pass(&mut self, commit: &Commit) -> Result<(), CommitError> {
        let g = self.g;
        self.metadata_pass = true;
        for &n in &g.metadata {
            let slot = &g.slots[n.idx()];
            let this = &*self;
            let read = |n: Node| this.read(n);
            let compute = slot
                .compute
                .as_ref()
                .expect("metadata derived has a compute");
            let (value, tracked) = run_compute(g, compute, &read, Some(commit));
            if let Some(message) = tracked.error {
                return Err(CommitError::Compute {
                    key: slot.key.clone(),
                    message,
                });
            }
            self.store(n, value, tracked.deps);
        }
        Ok(())
    }

    fn store(&mut self, n: Node, value: Value, deps: Vec<Node>) {
        if value != self.g.slots[n.idx()].value {
            self.values.insert(n, value);
            self.changed.push(n);
        }
        self.deps.push((n, deps));
    }

    fn read(&self, n: Node) -> Option<&Value> {
        let slot = self.g.slots.get(n.idx());
        let unsettled = self.pending.contains(&n)
            || (!self.metadata_pass && slot.is_some_and(|s| s.kind == NodeKind::CommitMetadata));
        if unsettled {
            return None;
        }
        Some(
            self.values
                .get(&n)
                .unwrap_or_else(|| slot.map_or(&NULL, |s| &s.value)),
        )
    }
}
