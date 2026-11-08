//! Data types of the Graph contract (spec 03, section 4).

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smol_str::SmolStr;

use crate::{GraphTime, NodeKey};

/// JSON-shaped on purpose: it is what both backends can hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Str(SmolStr),
    Bytes(Bytes),
    List(Arc<[Value]>),
    Map(Arc<IndexMap<SmolStr, Value>>),
    Handle(StoreHandle),
}

/// A reference into the side store (spec 03, section 8). Equality is `(store, id, version)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoreHandle {
    pub store: SmolStr,
    pub id: u64,
    pub version: u32,
}

/// Opaque bytes; base64 (standard alphabet, padded) on the wire, cheap to clone in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bytes(pub Arc<[u8]>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let raw = STANDARD.decode(text).map_err(serde::de::Error::custom)?;
        Ok(Bytes(raw.into()))
    }
}

/// Dense index inside `aibus-graph`; a backend may map it to its own id. The field is public
/// so a backend that lives in another crate (`aibus-graph-causl`) can mint nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Node(pub u32);

impl Node {
    pub(crate) fn idx(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub time: GraphTime,
    pub intent: SmolStr,
    /// Set on hydrate: the `now` of the snapshot that was loaded.
    pub originated_at: Option<GraphTime>,
    pub changed_nodes: Vec<Node>,
    pub at: SystemTime,
}

/// The inputs plus `now`; deriveds are recomputed on hydrate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: GraphTime,
    pub inputs: IndexMap<NodeKey, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Input,
    Derived,
    CommitMetadata,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Explanation {
    pub node: Node,
    pub key: NodeKey,
    pub kind: NodeKind,
    pub value: Value,
    pub version: u32,
    /// Time of the commit that last changed the value (0 if never).
    pub changed_at: GraphTime,
    /// Intent of that commit.
    pub intent: SmolStr,
    pub dependencies: Vec<Node>,
    /// Every input this node depends on, directly or not, in node order.
    pub inputs: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RetentionResult<T> {
    Found(T),
    /// `t` is older than the retained window.
    Evicted {
        oldest_retained: GraphTime,
    },
    /// `t` has not happened yet.
    Future {
        now: GraphTime,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum SimulateResult {
    /// What `commit` would do: the time it would create (unchanged for a no-op) and the new values.
    Applied {
        time: GraphTime,
        changed: Vec<(Node, Value)>,
    },
    Rejected(CommitError),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommitError {
    UnknownNode(Node),
    NotAnInput(Node),
    DuplicateWrite(Node),
    /// A compute called `Tracker::fail`.
    Compute {
        key: NodeKey,
        message: String,
    },
    /// A compute read itself, directly or through other deriveds.
    Cycle(NodeKey),
    /// A compute read a commit-metadata derived, which settles after everything else.
    MetadataDependency(NodeKey),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HydrateError {
    UnknownKey(NodeKey),
    NotAnInput(NodeKey),
    Commit(CommitError),
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(n) => write!(f, "unknown node {}", n.0),
            Self::NotAnInput(n) => write!(f, "node {} is not an input", n.0),
            Self::DuplicateWrite(n) => write!(f, "node {} written twice in one commit", n.0),
            Self::Compute { key, message } => write!(f, "compute of {key} failed: {message}"),
            Self::Cycle(key) => write!(f, "dependency cycle through {key}"),
            Self::MetadataDependency(key) => {
                write!(f, "read of commit-metadata node {key} inside a compute")
            }
        }
    }
}

impl fmt::Display for HydrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey(k) => write!(f, "snapshot key {k} is not registered"),
            Self::NotAnInput(k) => write!(f, "snapshot key {k} is not an input"),
            Self::Commit(e) => write!(f, "hydrate commit failed: {e}"),
        }
    }
}

impl std::error::Error for CommitError {}
impl std::error::Error for HydrateError {}

/// Cancels a subscription. Dropping it does nothing; call `unsubscribe`.
pub struct Unsubscribe(pub Arc<AtomicBool>);

impl Unsubscribe {
    pub fn unsubscribe(self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub(crate) static NULL: Value = Value::Null;

/// The only way to read inside a compute: every read is recorded as a dependency.
pub struct Tracker<'a> {
    read: &'a dyn Fn(Node) -> Option<&'a Value>,
    keys: &'a HashMap<NodeKey, Node>,
    commit: Option<&'a Commit>,
    deps: Vec<Node>,
    blocked: Option<Node>,
    error: Option<String>,
}

/// What a finished compute touched: dependencies in read order, a node it needed that is not
/// settled yet, and a failure message.
pub struct Tracked {
    pub deps: Vec<Node>,
    pub blocked: Option<Node>,
    pub error: Option<String>,
}

impl<'a> Tracker<'a> {
    /// `read` returns `None` for a node whose value is not settled in this commit yet.
    pub fn new(
        read: &'a dyn Fn(Node) -> Option<&'a Value>,
        keys: &'a HashMap<NodeKey, Node>,
        commit: Option<&'a Commit>,
    ) -> Self {
        Self {
            read,
            keys,
            commit,
            deps: Vec::new(),
            blocked: None,
            error: None,
        }
    }

    /// The returned reference outlives the borrow of the tracker, so a compute can hold several.
    pub fn get(&mut self, node: Node) -> &'a Value {
        if !self.deps.contains(&node) {
            self.deps.push(node);
        }
        match (self.read)(node) {
            Some(v) => v,
            None => {
                self.blocked.get_or_insert(node);
                &NULL
            }
        }
    }

    pub fn get_key(&mut self, key: &str) -> Option<&'a Value> {
        let node = *self.keys.get(key)?;
        Some(self.get(node))
    }

    /// The commit being built. Only commit-metadata deriveds see one (`None` elsewhere).
    pub fn commit(&self) -> Option<&'a Commit> {
        self.commit
    }

    /// Compute is pure and does not panic, so this is how it reports an error: the commit is
    /// rolled back with `CommitError::Compute`. Returns `Null` so a compute can `return t.fail(..)`.
    pub fn fail(&mut self, message: impl Into<String>) -> Value {
        self.error.get_or_insert(message.into());
        Value::Null
    }

    pub fn finish(self) -> Tracked {
        Tracked {
            deps: self.deps,
            blocked: self.blocked,
            error: self.error,
        }
    }
}
