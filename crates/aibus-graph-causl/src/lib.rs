//! `CauslGraph`: the `Graph` contract (spec 03, section 4) hosted on `causl-engine-core`.
//!
//! The engine only offers the state-machine layer, so this is the thin host layer around it:
//! string keys map to engine node ids, the registered Rust closures run inside a
//! `ComputeCallback` with a tracked recorder, subscriptions go through the engine's subscriber
//! dispatch, and values cross the boundary as tagged JSON. Everything here is behind the
//! `causl-rs` feature; with the feature off this crate is empty and needs no private source.
#![cfg(feature = "causl-rs")]
#![forbid(unsafe_code)]

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use aibus_graph::{
    Bytes, Commit, CommitError, CommitObserver, Compute, Explanation, Graph, GraphTime,
    HydrateError, Node, NodeKey, NodeKind, NodeObserver, RetentionResult, SimulateResult, SmolStr,
    Snapshot, StoreHandle, Tracker, Unsubscribe, Value, COMMIT_HISTORY_CAP,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use causl_engine_core::json_value::JsonObject;
use causl_engine_core::phase_step::{transition_phased_in_place_with_callbacks, DEFAULT_ENGINE_ID};
use causl_engine_core::state::{IRCommit, COMMIT_LOG_ID};
use causl_engine_core::{
    Action, ComputeCallback, ComputeError, JsonValue, NodeId, ObserverId, RaceClass, ReadAtResult,
    State, SubscriberCallback, SubscriberFiring,
};
use indexmap::IndexMap;
use smallvec::SmallVec;

/// Reserved object key: a JSON object carrying it is an encoded `Value` that JSON cannot hold.
const TAG: &str = "$t";
/// Integers up to this magnitude are exact in an `f64`, so they travel as plain JSON numbers.
const EXACT_INT: u64 = 1 << 53;

fn tagged(tag: &str, payload: JsonValue) -> JsonValue {
    let mut o = JsonObject::default();
    o.insert(TAG.into(), JsonValue::String(tag.into()));
    o.insert("v".into(), payload);
    JsonValue::Object(o)
}

/// `Value` to the engine's `JsonValue`. Lossless: plain numbers are always `I64`, everything
/// JSON cannot say (`F64`, big ints, bytes, handles, maps that use the tag key) is a tagged object.
fn encode(v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::I64(i) if i.unsigned_abs() <= EXACT_INT => JsonValue::Number(*i as f64),
        Value::I64(i) => tagged("i64", JsonValue::String(i.to_string().into())),
        Value::F64(f) => tagged("f64", JsonValue::Number(*f)),
        Value::Str(s) => JsonValue::String(s.clone()),
        Value::Bytes(b) => tagged("bytes", JsonValue::String(STANDARD.encode(&b.0).into())),
        Value::List(l) => JsonValue::Array(l.iter().map(encode).collect()),
        Value::Map(m) => {
            let o: JsonObject = m.iter().map(|(k, v)| (k.clone(), encode(v))).collect();
            if m.contains_key(TAG) {
                tagged("map", JsonValue::Object(o))
            } else {
                JsonValue::Object(o)
            }
        }
        Value::Handle(h) => tagged(
            "handle",
            JsonValue::Array(vec![
                JsonValue::String(h.store.clone()),
                JsonValue::String(h.id.to_string().into()),
                JsonValue::Number(f64::from(h.version)),
            ]),
        ),
    }
}

fn decode(j: &JsonValue) -> Value {
    match j {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) if n.fract() == 0.0 && n.abs() <= EXACT_INT as f64 => {
            Value::I64(*n as i64)
        }
        JsonValue::Number(n) => Value::F64(*n),
        JsonValue::String(s) => Value::Str(s.clone()),
        JsonValue::Array(a) => Value::List(a.iter().map(decode).collect()),
        JsonValue::Object(o) => match (o.get(TAG), o.get("v")) {
            (Some(JsonValue::String(tag)), Some(payload)) => decode_tagged(tag, payload),
            _ => Value::Map(Arc::new(
                o.iter().map(|(k, v)| (k.clone(), decode(v))).collect(),
            )),
        },
    }
}

fn decode_tagged(tag: &str, payload: &JsonValue) -> Value {
    match (tag, payload) {
        ("i64", JsonValue::String(s)) => s.parse().map_or(Value::Null, Value::I64),
        ("f64", JsonValue::Number(n)) => Value::F64(*n),
        ("bytes", JsonValue::String(s)) => STANDARD
            .decode(s.as_str())
            .map_or(Value::Null, |raw| Value::Bytes(Bytes(raw.into()))),
        ("map", JsonValue::Object(o)) => Value::Map(Arc::new(
            o.iter().map(|(k, v)| (k.clone(), decode(v))).collect(),
        )),
        ("handle", JsonValue::Array(a)) => match a.as_slice() {
            [JsonValue::String(store), JsonValue::String(id), JsonValue::Number(version)] => {
                Value::Handle(StoreHandle {
                    store: store.clone(),
                    id: id.parse().unwrap_or(0),
                    version: *version as u32,
                })
            }
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

fn node_id(n: Node) -> NodeId {
    NodeId::new(n.0)
}

/// `None` for the engine's own commit-log node, which has no aibus counterpart.
fn node_of(id: NodeId) -> Option<Node> {
    (id != COMMIT_LOG_ID).then_some(Node(id.slot))
}

struct Meta {
    key: NodeKey,
    kind: NodeKind,
    compute: Option<Compute>,
    value: Value,
    /// The value at registration: the engine keeps no seed for a derived.
    initial: Value,
    version: u32,
    changed_at: GraphTime,
    intent: SmolStr,
}

struct Watch {
    gone: Arc<AtomicBool>,
    observer: NodeObserver,
}

/// What the engine runs during one transition: the compute bodies, the values they may read, and
/// the subscriber firings it hands back.
struct Host<'a> {
    nodes: &'a [Meta],
    keys: &'a HashMap<NodeKey, Node>,
    /// Values produced or written in this transition; nodes not touched read `nodes[i].value`.
    overlay: Vec<OnceCell<Value>>,
    firings: RefCell<Vec<SubscriberFiring>>,
    /// Nodes being pulled right now, so a cycle of new dependencies cannot recurse forever.
    pulling: RefCell<Vec<u32>>,
}

impl Host<'_> {
    /// The settled value of `n` for a compute whose dependency list so far is `prior`. The engine
    /// orders recomputes by the old edges, so a dependency this compute just picked up can be a
    /// derived it has not reached yet: that one is computed on the spot (a pull).
    fn read(&self, state: &State, prior: &[NodeId], n: Node) -> Option<&Value> {
        let i = n.0 as usize;
        if let Some(v) = self.overlay.get(i).and_then(OnceCell::get) {
            return Some(v);
        }
        let meta = self.nodes.get(i)?;
        let pullable = meta.compute.is_some()
            && !prior.contains(&node_id(n))
            && !self.pulling.borrow().contains(&n.0)
            && self.stale(state, n);
        if pullable {
            self.pulling.borrow_mut().push(n.0);
            if let Ok((value, _)) = self.eval(state, node_id(n), None, true) {
                let _ = self.overlay[i].set(value);
            }
            self.pulling.borrow_mut().pop();
            if let Some(v) = self.overlay[i].get() {
                return Some(v);
            }
        }
        Some(&meta.value)
    }

    /// A derived is stale when a dependency changed in this transition or is itself stale.
    fn stale(&self, state: &State, n: Node) -> bool {
        let id = node_id(n);
        let Some(cell) = state.derived(id).filter(|c| c.id == id) else {
            return false;
        };
        cell.deps.iter().filter_map(|d| node_of(*d)).any(|d| {
            let i = d.0 as usize;
            match self.overlay[i].get() {
                Some(v) => *v != self.nodes[i].value,
                None => self.nodes[i].compute.is_some() && self.stale(state, d),
            }
        })
    }

    /// Run the registered closure of `id` through a tracker: its value and the nodes it read.
    /// The engine has ordered an ordinary recompute after its old dependencies; a `pulled` one
    /// has no such guarantee, so every read it makes is checked for staleness.
    fn eval(
        &self,
        state: &State,
        id: NodeId,
        commit: Option<&Commit>,
        pulled: bool,
    ) -> Result<(Value, Vec<Node>), String> {
        let compute = self.nodes[id.slot as usize]
            .compute
            .as_ref()
            .ok_or("node has no compute")?;
        let prior = state
            .derived(id)
            .filter(|c| c.id == id && !pulled)
            .map_or(&[][..], |c| c.deps.as_slice());
        let read = |n: Node| self.read(state, prior, n);
        let mut tracker = Tracker::new(&read, self.keys, commit);
        let value = compute(&mut tracker);
        let tracked = tracker.finish();
        match tracked.error {
            Some(message) => Err(message),
            None => Ok((value, tracked.deps)),
        }
    }
}

impl ComputeCallback for Host<'_> {
    fn compute(
        &self,
        state: &mut State,
        derived_id: NodeId,
        _dep_values: &[(NodeId, &JsonValue)],
    ) -> Result<(JsonValue, SmallVec<[NodeId; 4]>), ComputeError> {
        // A commit-metadata derived sees the commit being built, as the engine has it by now.
        let commit = (self.nodes[derived_id.slot as usize].kind == NodeKind::CommitMetadata)
            .then(|| state.commit_history.back().map(|c| commit_of(c, None)))
            .flatten();
        let (value, deps) = self
            .eval(state, derived_id, commit.as_ref(), false)
            .map_err(|message| ComputeError::threw(derived_id, message))?;
        let encoded = encode(&value);
        let _ = self.overlay[derived_id.slot as usize].set(value);
        Ok((encoded, deps.into_iter().map(node_id).collect()))
    }
}

impl SubscriberCallback for Host<'_> {
    fn fire_batch(&self, firings: &[SubscriberFiring]) -> Vec<causl_engine_core::ObserverError> {
        self.firings.borrow_mut().extend_from_slice(firings);
        Vec::new()
    }
}

fn commit_of(c: &IRCommit, originated_at: Option<GraphTime>) -> Commit {
    Commit {
        time: c.time,
        intent: SmolStr::new(&c.intent),
        originated_at,
        changed_nodes: c
            .changed_nodes
            .iter()
            .copied()
            .filter_map(node_of)
            .collect(),
        at: SystemTime::now(),
    }
}

/// What one transition produced.
struct Tx {
    commit: Commit,
    /// New value of every changed node, in changed order.
    values: Vec<(Node, Value)>,
    firings: Vec<SubscriberFiring>,
}

/// Validate, then run one engine transition against `state`. Atomic: on error `state` is as it was.
/// `force` makes a commit that changes nothing still advance time (hydrate).
fn transact(
    nodes: &[Meta],
    keys: &HashMap<NodeKey, Node>,
    state: &mut State,
    intent: &str,
    writes: &[(Node, Value)],
    force: bool,
) -> Result<Tx, CommitError> {
    let mut seen = BTreeSet::new();
    for (n, _) in writes {
        let meta = nodes
            .get(n.0 as usize)
            .ok_or(CommitError::UnknownNode(*n))?;
        if meta.kind != NodeKind::Input {
            return Err(CommitError::NotAnInput(*n));
        }
        if !seen.insert(*n) {
            return Err(CommitError::DuplicateWrite(*n));
        }
    }
    let changed: Vec<&(Node, Value)> = writes
        .iter()
        .filter(|(n, v)| nodes[n.0 as usize].value != *v)
        .collect();
    if changed.is_empty() && !force {
        let commit = Commit {
            time: state.now,
            intent: SmolStr::new(intent),
            originated_at: None,
            changed_nodes: Vec::new(),
            at: SystemTime::now(),
        };
        return Ok(Tx {
            commit,
            values: Vec::new(),
            firings: Vec::new(),
        });
    }
    let host = Host {
        nodes,
        keys,
        overlay: nodes.iter().map(|_| OnceCell::new()).collect(),
        firings: RefCell::new(Vec::new()),
        pulling: RefCell::new(Vec::new()),
    };
    for (n, v) in &changed {
        let _ = host.overlay[n.0 as usize].set(v.clone());
    }
    let action = Action::CommitWithValues {
        intent: intent.to_owned(),
        writes: changed
            .iter()
            .map(|(n, v)| (node_id(*n), encode(v)))
            .collect(),
    };
    let record = transition_phased_in_place_with_callbacks(
        state,
        &action,
        DEFAULT_ENGINE_ID,
        &host,
        &host,
        &mut Vec::new(),
    )
    .map_err(|e| match e {
        RaceClass::ComputeThrew { slot, reason } => CommitError::Compute {
            key: nodes[slot as usize].key.clone(),
            message: reason,
        },
        RaceClass::CycleClosed { path } => CommitError::Cycle(
            path.first()
                .and_then(|id| nodes.get(id.slot as usize))
                .map_or_else(NodeKey::default, |m| m.key.clone()),
        ),
        other => CommitError::Compute {
            key: NodeKey::new("engine"),
            message: format!("{other:?}"),
        },
    })?;
    let changed_nodes: Vec<Node> = record
        .changed_nodes
        .iter()
        .copied()
        .filter_map(node_of)
        .collect();
    let values = changed_nodes
        .iter()
        .map(|&n| {
            let value = host.overlay[n.0 as usize]
                .get()
                .cloned()
                .unwrap_or_else(|| state.get_value(node_id(n)).map_or(Value::Null, decode));
            (n, value)
        })
        .collect();
    let commit = Commit {
        time: record.time,
        intent: SmolStr::new(&record.intent),
        originated_at: None,
        changed_nodes,
        at: SystemTime::now(),
    };
    Ok(Tx {
        commit,
        values,
        firings: host.firings.into_inner(),
    })
}

pub struct CauslGraph {
    state: State,
    nodes: Vec<Meta>,
    by_key: HashMap<NodeKey, Node>,
    /// `log[start..]` is the retained window.
    log: Vec<Commit>,
    start: usize,
    /// Node subscriptions by the engine observer id they were registered under.
    watches: HashMap<String, Watch>,
    commit_watches: Vec<(Arc<AtomicBool>, CommitObserver)>,
    next_observer: u64,
    /// Table cells below these slots are settled: live nodes or retired placeholders.
    inputs_sealed: usize,
    deriveds_sealed: usize,
}

impl Default for CauslGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl CauslGraph {
    pub fn new() -> Self {
        let state = State {
            // The engine counts retained states, the contract counts commits: one more for time 0.
            commit_history_cap: COMMIT_HISTORY_CAP + 1,
            retain_derived_rows: true,
            ..State::default()
        };
        Self {
            state,
            nodes: Vec::new(),
            by_key: HashMap::new(),
            log: Vec::new(),
            start: 0,
            watches: HashMap::new(),
            commit_watches: Vec::new(),
            next_observer: 0,
            inputs_sealed: 0,
            deriveds_sealed: 0,
        }
    }

    /// Slots are shared by inputs and deriveds here (slot = node index), so the cells the engine
    /// auto-creates in one table for slots of the other kind must not match a live id.
    fn retire_placeholders(&mut self, input: bool) {
        let upto = self.nodes.len();
        if input {
            let from = std::mem::replace(&mut self.inputs_sealed, upto + 1);
            for c in self.state.inputs.iter_mut().take(upto).skip(from) {
                c.id = NodeId::with_gen(c.id.slot, u32::MAX);
                c.generation = u32::MAX;
            }
        } else {
            let from = std::mem::replace(&mut self.deriveds_sealed, upto + 1);
            for c in self.state.deriveds.iter_mut().take(upto).skip(from) {
                c.id = NodeId::with_gen(c.id.slot, u32::MAX);
                c.generation = u32::MAX;
            }
        }
    }

    fn add_input(&mut self, key: &NodeKey, initial: Value) -> Node {
        let node = Node(self.nodes.len() as u32);
        let id = node_id(node);
        self.state.seed_input_inplace(id, encode(&initial));
        self.retire_placeholders(true);
        self.state.registered_at.insert(id, self.state.now);
        self.push_meta(key, NodeKind::Input, None, initial)
    }

    fn add_computed(&mut self, key: &NodeKey, kind: NodeKind, compute: Compute) -> Node {
        if let Some(&existing) = self.by_key.get(key) {
            return existing; // first registration wins
        }
        let node = Node(self.nodes.len() as u32);
        let id = node_id(node);
        let read = |n: Node| self.nodes.get(n.0 as usize).map(|m| &m.value);
        let mut tracker = Tracker::new(&read, &self.by_key, None);
        let value = compute(&mut tracker);
        let tracked = tracker.finish();
        let value = if tracked.error.is_some() {
            Value::Null
        } else {
            value
        };
        let deps: SmallVec<[NodeId; 4]> = tracked.deps.into_iter().map(node_id).collect();
        let now = self.state.now;
        let cell = self.state.derived_mut_or_insert(id);
        cell.id = id;
        cell.generation = 0;
        cell.last_value = encode(&value);
        cell.deps = deps.clone();
        cell.computed = true;
        cell.last_time = now;
        for parent in deps {
            self.state.register_dep(parent, id);
        }
        self.retire_placeholders(false);
        self.state.registered_at.insert(id, now);
        if kind == NodeKind::CommitMetadata {
            self.state.register_commit_metadata(id);
        }
        self.push_meta(key, kind, Some(compute), value)
    }

    fn push_meta(
        &mut self,
        key: &NodeKey,
        kind: NodeKind,
        compute: Option<Compute>,
        value: Value,
    ) -> Node {
        let node = Node(self.nodes.len() as u32);
        self.by_key.insert(key.clone(), node);
        self.nodes.push(Meta {
            key: key.clone(),
            kind,
            compute,
            initial: value.clone(),
            value,
            version: 0,
            changed_at: 0,
            intent: SmolStr::default(),
        });
        node
    }

    /// Fold a finished transition into the host's view, then fire subscribers.
    fn apply(&mut self, tx: Tx, originated_at: Option<GraphTime>) -> Commit {
        let mut commit = tx.commit;
        commit.originated_at = originated_at;
        for (n, value) in tx.values {
            let meta = &mut self.nodes[n.0 as usize];
            meta.value = value;
            meta.version += 1;
            meta.changed_at = commit.time;
            meta.intent = commit.intent.clone();
        }
        self.log.push(commit.clone());
        while self.log.len() - self.start > COMMIT_HISTORY_CAP {
            self.start += 1;
        }
        if self.start >= COMMIT_HISTORY_CAP / 8 {
            self.log.drain(..self.start);
            self.start = 0;
        }
        for firing in tx.firings {
            let Some(watch) = self.watches.get_mut(firing.observer.0.as_str()) else {
                continue;
            };
            if watch.gone.load(Ordering::Relaxed) {
                continue;
            }
            let value = &self.nodes[firing.node.slot as usize].value;
            (watch.observer)(value, &commit);
        }
        self.watches.retain(|_, w| !w.gone.load(Ordering::Relaxed));
        self.commit_watches
            .retain(|(gone, _)| !gone.load(Ordering::Relaxed));
        for (_, observer) in &mut self.commit_watches {
            observer(&commit);
        }
        commit
    }

    fn miss<T>(&self, t: GraphTime) -> Option<RetentionResult<T>> {
        if t > self.state.now {
            Some(RetentionResult::Future {
                now: self.state.now,
            })
        } else if t < self.state.oldest_retained_time() {
            Some(RetentionResult::Evicted {
                oldest_retained: self.state.oldest_retained_time(),
            })
        } else {
            None
        }
    }

    fn value_at(&self, node: Node, t: GraphTime) -> Value {
        match self.state.read_at_result(node_id(node), t) {
            ReadAtResult::Retained { value, .. } => decode(&value),
            // Inside the window but never changed since registration.
            ReadAtResult::Evicted { .. } => self.nodes[node.0 as usize].initial.clone(),
        }
    }
}

impl Graph for CauslGraph {
    fn input(&mut self, key: &NodeKey, initial: Value) -> Node {
        match self.by_key.get(key) {
            Some(&existing) => existing, // first registration wins
            None => self.add_input(key, initial),
        }
    }

    fn derived(&mut self, key: &NodeKey, compute: Compute) -> Node {
        self.add_computed(key, NodeKind::Derived, compute)
    }

    fn commit_metadata_derived(&mut self, key: &NodeKey, compute: Compute) -> Node {
        self.add_computed(key, NodeKind::CommitMetadata, compute)
    }

    fn commit(&mut self, intent: &str, writes: &[(Node, Value)]) -> Result<Commit, CommitError> {
        let tx = transact(
            &self.nodes,
            &self.by_key,
            &mut self.state,
            intent,
            writes,
            false,
        )?;
        // No change means no new time: one commit creates exactly one time or zero.
        if tx.commit.changed_nodes.is_empty() {
            return Ok(tx.commit);
        }
        Ok(self.apply(tx, None))
    }

    fn read(&self, node: Node) -> &Value {
        self.nodes
            .get(node.0 as usize)
            .map_or(&Value::Null, |m| &m.value)
    }

    fn read_key(&self, key: &str) -> Option<&Value> {
        self.by_key.get(key).map(|n| self.read(*n))
    }

    fn subscribe(&mut self, node: Node, observer: NodeObserver) -> Unsubscribe {
        let name = format!("o{}", self.next_observer);
        self.next_observer += 1;
        // A node that exists cannot be disposed, so registration cannot fail.
        let _ = self
            .state
            .register_subscriber(node_id(node), ObserverId(name.clone()));
        let gone = Arc::new(AtomicBool::new(false));
        self.watches.insert(
            name,
            Watch {
                gone: gone.clone(),
                observer,
            },
        );
        Unsubscribe(gone)
    }

    fn subscribe_commits(&mut self, observer: CommitObserver) -> Unsubscribe {
        let gone = Arc::new(AtomicBool::new(false));
        self.commit_watches.push((gone.clone(), observer));
        Unsubscribe(gone)
    }

    fn explain(&self, node: Node) -> Explanation {
        let meta = &self.nodes[node.0 as usize];
        let mut inputs = BTreeSet::new();
        let mut todo = vec![node];
        let mut seen = BTreeSet::new();
        while let Some(n) = todo.pop() {
            if !seen.insert(n) {
                continue;
            }
            if self.nodes[n.0 as usize].kind == NodeKind::Input {
                inputs.insert(n);
            }
            todo.extend(self.dependencies(n));
        }
        Explanation {
            node,
            key: meta.key.clone(),
            kind: meta.kind,
            value: meta.value.clone(),
            version: meta.version,
            changed_at: meta.changed_at,
            intent: meta.intent.clone(),
            dependencies: self.dependencies(node),
            inputs: inputs.into_iter().collect(),
        }
    }

    fn commit_log(&self) -> &[Commit] {
        &self.log[self.start..]
    }

    fn snapshot(&self) -> Snapshot {
        let inputs = self.nodes.iter().filter(|m| m.kind == NodeKind::Input);
        Snapshot {
            now: self.state.now,
            inputs: inputs.map(|m| (m.key.clone(), m.value.clone())).collect(),
        }
    }

    fn hydrate(&mut self, snap: Snapshot) -> Result<Commit, HydrateError> {
        let mut writes = Vec::with_capacity(snap.inputs.len());
        for (key, value) in snap.inputs {
            let node = *self
                .by_key
                .get(&key)
                .ok_or_else(|| HydrateError::UnknownKey(key.clone()))?;
            if self.nodes[node.0 as usize].kind != NodeKind::Input {
                return Err(HydrateError::NotAnInput(key));
            }
            writes.push((node, value));
        }
        // Hydrate always advances time by one, even when every input already matches.
        let tx = transact(
            &self.nodes,
            &self.by_key,
            &mut self.state,
            "hydrate",
            &writes,
            true,
        )
        .map_err(HydrateError::Commit)?;
        Ok(self.apply(tx, Some(snap.now)))
    }

    fn read_at(&self, node: Node, t: GraphTime) -> RetentionResult<Value> {
        if let Some(miss) = self.miss(t) {
            return miss;
        }
        if node.0 as usize >= self.nodes.len() {
            return RetentionResult::Found(Value::Null);
        }
        RetentionResult::Found(self.value_at(node, t))
    }

    fn snapshot_at(&self, t: GraphTime) -> RetentionResult<Snapshot> {
        if let Some(miss) = self.miss(t) {
            return miss;
        }
        let inputs: IndexMap<NodeKey, Value> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, m)| m.kind == NodeKind::Input)
            .map(|(i, m)| (m.key.clone(), self.value_at(Node(i as u32), t)))
            .collect();
        RetentionResult::Found(Snapshot { now: t, inputs })
    }

    fn simulate(&self, intent: &str, writes: &[(Node, Value)]) -> SimulateResult {
        let mut scratch = self.state.clone();
        match transact(
            &self.nodes,
            &self.by_key,
            &mut scratch,
            intent,
            writes,
            false,
        ) {
            Err(e) => SimulateResult::Rejected(e),
            Ok(tx) => SimulateResult::Applied {
                time: tx.commit.time,
                changed: tx.values,
            },
        }
    }

    fn dependencies(&self, node: Node) -> Vec<Node> {
        self.state
            .derived(node_id(node))
            .filter(|c| c.id == node_id(node))
            .map(|c| c.deps.iter().copied().filter_map(node_of).collect())
            .unwrap_or_default()
    }

    fn dependents(&self, node: Node) -> Vec<Node> {
        self.state
            .dependents
            .get(&node_id(node))
            .map(|d| d.iter().copied().filter_map(node_of).collect())
            .unwrap_or_default()
    }

    fn now(&self) -> GraphTime {
        self.state.now
    }

    fn node(&self, key: &str) -> Option<Node> {
        self.by_key.get(key).copied()
    }

    fn version(&self, node: Node) -> u32 {
        self.nodes.get(node.0 as usize).map_or(0, |m| m.version)
    }
}
