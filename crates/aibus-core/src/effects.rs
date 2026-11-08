//! The effects executor (02 section 3.2, 03 section 7).
//!
//! It reads the seven effect nodes after a commit and hands what is new to a sink. Every effect
//! is keyed by an id and the executor remembers what it already handed over, so reading the
//! same derived after an unrelated commit, or replaying a commit, starts nothing twice.
//!
//! The sink is a closure rather than the spec's pile of registries: the real spawner, tool
//! executor and hook runner arrive with A2 and capture whatever they need.

use std::collections::HashSet;

use aibus_graph::{Commit, Graph, Value};
use smol_str::SmolStr;

use crate::agent::Isolation;
use crate::ids::{AgentName, RunId, ToolCallId};
use crate::state::value::{as_i64, as_list, as_str, get};
use crate::state::{
    run_key, CHECKPOINT_PENDING, HOOKS_DUE, ORCH_RUNNABLE, PERSIST_DUE, ROUTER_QUEUE, RUNS_ORDER,
    TOOLS_PENDING,
};

#[derive(Clone, PartialEq, Debug)]
pub struct SpawnSpec {
    pub run: RunId,
    pub agent: AgentName,
    pub parent: Option<RunId>,
    pub depth: u8,
    pub background: bool,
    pub isolation: Isolation,
    /// The prompt, inline or a handle into the store.
    pub prompt: Value,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PendingTool {
    pub id: ToolCallId,
    pub run: RunId,
    pub name: SmolStr,
    pub input: Value,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Effect {
    Spawn(SpawnSpec),
    Tool(PendingTool),
    /// A `model:hooks:due` entry as the graph holds it.
    Hook(Value),
    /// A `model:router:queue` entry as the graph holds it.
    Router(Value),
    Advise {
        checkpoint: SmolStr,
    },
    /// Snapshot and commit-log flush; the executor answers with `Msg::Persisted`.
    Persist,
    Cancel(RunId),
}

pub struct Effects {
    sink: Box<dyn FnMut(Effect)>,
    started: HashSet<SmolStr>,
}

impl Effects {
    pub fn new(sink: impl FnMut(Effect) + 'static) -> Self {
        Self {
            sink: Box::new(sink),
            started: HashSet::new(),
        }
    }

    /// An executor that starts nothing, for an `App` that has not been wired yet.
    pub fn idle() -> Self {
        Self::new(|_| {})
    }

    /// Called by the loop after every commit.
    pub fn after_commit(&mut self, graph: &dyn Graph, commit: &Commit) {
        if commit.changed_nodes.is_empty() {
            return;
        }
        let read = |k: &str| graph.read_key(k).map(as_list).unwrap_or(&[]);
        let mut found: Vec<(SmolStr, Effect)> = Vec::new();

        for v in read(ORCH_RUNNABLE) {
            if let Some(spec) = spawn_spec(v) {
                found.push((format!("run:{}", spec.run).into(), Effect::Spawn(spec)));
            }
        }
        for v in read(TOOLS_PENDING) {
            if get(v, "approved") != Some(&Value::Bool(true)) {
                continue;
            }
            if let Some(t) = pending_tool(v) {
                found.push((format!("tool:{}", t.id.0).into(), Effect::Tool(t)));
            }
        }
        for v in read(HOOKS_DUE) {
            if let Some(id) = get(v, "id").and_then(as_str) {
                found.push((format!("hook:{id}").into(), Effect::Hook(v.clone())));
            }
        }
        for v in read(ROUTER_QUEUE) {
            if let Some(f) = get(v, "fork").and_then(as_i64) {
                found.push((format!("router:{f}").into(), Effect::Router(v.clone())));
            }
        }
        if let Some(Value::Str(cp)) = graph.read_key(CHECKPOINT_PENDING) {
            found.push((
                format!("advise:{cp}").into(),
                Effect::Advise {
                    checkpoint: cp.clone(),
                },
            ));
        }
        if graph.read_key(PERSIST_DUE) == Some(&Value::Bool(true)) {
            found.push(("persist".into(), Effect::Persist));
        }
        for id in read(RUNS_ORDER).iter().filter_map(as_str) {
            let flagged =
                graph.read_key(&run_key(id, "cancel_requested")) == Some(&Value::Bool(true));
            if let (true, Ok(run)) = (flagged, id.parse::<RunId>()) {
                found.push((format!("cancel:{id}").into(), Effect::Cancel(run)));
            }
        }

        let live: HashSet<SmolStr> = found.iter().map(|(k, _)| k.clone()).collect();
        for (key, effect) in found {
            if self.started.insert(key) {
                (self.sink)(effect);
            }
        }
        // A key that left the listing is finished; if it comes back (a retry), it starts again.
        self.started.retain(|k| live.contains(k));
    }
}

fn spawn_spec(v: &Value) -> Option<SpawnSpec> {
    Some(SpawnSpec {
        run: get(v, "run").and_then(as_str)?.parse().ok()?,
        agent: get(v, "agent").and_then(as_str)?.parse().ok()?,
        parent: get(v, "parent")
            .and_then(as_str)
            .and_then(|p| p.parse().ok()),
        depth: get(v, "depth").and_then(as_i64).unwrap_or(0).clamp(0, 255) as u8,
        background: get(v, "background") == Some(&Value::Bool(true)),
        isolation: if get(v, "isolation").and_then(as_str) == Some("worktree") {
            Isolation::Worktree
        } else {
            Isolation::None
        },
        prompt: get(v, "prompt").cloned().unwrap_or(Value::Null),
    })
}

fn pending_tool(v: &Value) -> Option<PendingTool> {
    let id: ulid::Ulid = get(v, "id").and_then(as_str)?.parse().ok()?;
    Some(PendingTool {
        id: ToolCallId(id),
        run: get(v, "run").and_then(as_str)?.parse().ok()?,
        name: get(v, "name").and_then(as_str)?.into(),
        input: get(v, "input").cloned().unwrap_or(Value::Null),
    })
}
