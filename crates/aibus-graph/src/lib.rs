//! Causl contract (trait) and the mini engine. Depends on nothing in the workspace.
//!
//! `Graph` is the Causl SPEC section 12 surface as aibus programs against it (spec 03, section 4).
//! `mini::MiniGraph` is the in-house engine that implements it.
#![forbid(unsafe_code)]

pub mod mini;
mod types;

pub use smol_str::SmolStr;
pub use types::*;

/// Commits retained for `read_at` / `snapshot_at` (spec 03, section 9).
pub const COMMIT_HISTORY_CAP: usize = 4096;

pub type NodeKey = SmolStr;
pub type GraphTime = u64;

/// Called after a commit for each changed node it watches.
pub type NodeObserver = Box<dyn FnMut(&Value, &Commit)>;
/// Called once after every commit.
pub type CommitObserver = Box<dyn FnMut(&Commit)>;

/// Registered by `derived`; the closure reads only through the `Tracker`.
pub type Compute = Box<dyn Fn(&mut Tracker) -> Value + Send>;

pub trait Graph {
    fn input(&mut self, key: &NodeKey, initial: Value) -> Node;
    fn derived(&mut self, key: &NodeKey, compute: Compute) -> Node;
    fn commit(&mut self, intent: &str, writes: &[(Node, Value)]) -> Result<Commit, CommitError>;
    fn read(&self, node: Node) -> &Value;
    fn read_key(&self, key: &str) -> Option<&Value>;
    fn subscribe(&mut self, node: Node, observer: NodeObserver) -> Unsubscribe;
    fn subscribe_commits(&mut self, observer: CommitObserver) -> Unsubscribe;
    fn explain(&self, node: Node) -> Explanation;
    fn commit_log(&self) -> &[Commit];
    fn snapshot(&self) -> Snapshot;
    fn hydrate(&mut self, snap: Snapshot) -> Result<Commit, HydrateError>;
    fn read_at(&self, node: Node, t: GraphTime) -> RetentionResult<Value>;
    fn snapshot_at(&self, t: GraphTime) -> RetentionResult<Snapshot>;
    fn simulate(&self, intent: &str, writes: &[(Node, Value)]) -> SimulateResult;
    fn dependencies(&self, node: Node) -> Vec<Node>;
    fn dependents(&self, node: Node) -> Vec<Node>;
    fn commit_metadata_derived(&mut self, key: &NodeKey, compute: Compute) -> Node;
    fn now(&self) -> GraphTime;
    fn node(&self, key: &str) -> Option<Node>;
    fn version(&self, node: Node) -> u32;
}
