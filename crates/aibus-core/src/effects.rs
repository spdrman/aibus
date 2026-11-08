//! The effects executor (02 section 3.2, 03 section 7).
//!
//! It reads the seven effect nodes after a commit and hands what is new to a sink. Every effect
//! is keyed by an id and the executor remembers what it already handed over, so reading the
//! same derived after an unrelated commit, or replaying a commit, starts nothing twice.
//!
//! The sink is a closure rather than the spec's pile of registries: the real spawner, tool
//! executor and hook runner arrive with A2 and capture whatever they need.

use std::collections::HashSet;
use std::sync::Arc;

use aibus_graph::{Commit, Graph, Value};
use smol_str::SmolStr;

use crate::agent::Isolation;
use crate::ids::{AccountName, AgentName, RunId, ToolCallId};
use crate::orchestrator::{Launcher, Orchestrator};
use crate::run::RunStatus;
use crate::state::store::Store;
use crate::state::value::{as_i64, as_list, as_str, get};
use crate::state::{
    run_key, CHECKPOINT_PENDING, HOOKS_DUE, LIMITS, ORCH_COUNTS, ORCH_RUNNABLE, PERSIST_DUE,
    ROUTER_QUEUE, RUNS_ORDER, TOOLS_PENDING,
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
    /// A model the caller asked for, over the definition's.
    pub model: Option<String>,
    /// An account the caller asked for, or the definition's.
    pub account: Option<AccountName>,
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
    /// A run is (or was made) a background run: a foreground wait on it ends.
    Detach(RunId),
}

/// What starts runs when the executor is wired to an orchestrator.
struct Wired {
    orch: Arc<Orchestrator>,
    launcher: Arc<dyn Launcher>,
    store: Store,
}

pub struct Effects {
    sink: Box<dyn FnMut(Effect)>,
    started: HashSet<SmolStr>,
    wired: Option<Wired>,
}

impl Effects {
    pub fn new(sink: impl FnMut(Effect) + 'static) -> Self {
        Self {
            sink: Box::new(sink),
            started: HashSet::new(),
            wired: None,
        }
    }

    /// An executor that starts runs through `orch` (built by `launcher`), cancels them and
    /// detaches foreground waits itself. Everything else goes to `sink`.
    pub fn orchestrated(
        orch: Arc<Orchestrator>,
        launcher: Arc<dyn Launcher>,
        store: Store,
        sink: impl FnMut(Effect) + 'static,
    ) -> Self {
        Self {
            wired: Some(Wired {
                orch,
                launcher,
                store,
            }),
            ..Self::new(sink)
        }
    }

    /// Starts one run the orchestrator listed. Idempotent by id: asking again for a run that is
    /// live does nothing.
    pub fn spawn_run(&mut self, spec: SpawnSpec) {
        match &self.wired {
            Some(w) => w.orch.start(&*w.launcher, &w.store, spec),
            None => (self.sink)(Effect::Spawn(spec)),
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
        if let Some(w) = &self.wired {
            w.orch.observe(graph);
        }

        // A run that was handed to the spawner but has not said `RunStarted` yet still shows as
        // queued, so `runnable` has not charged it a slot. It heads the list (the order is
        // oldest first), so taking no more than the free slots keeps the limit.
        let num = |k: &str, of: &str| graph.read_key(k).and_then(|v| get(v, of)).and_then(as_i64);
        let room = num(LIMITS, "max_concurrent_runs").unwrap_or(8)
            - num(ORCH_COUNTS, "running").unwrap_or(0);
        for v in read(ORCH_RUNNABLE).iter().take(room.max(0) as usize) {
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
            let Ok(run) = id.parse::<RunId>() else {
                continue;
            };
            let flagged =
                graph.read_key(&run_key(id, "cancel_requested")) == Some(&Value::Bool(true));
            if flagged {
                found.push((format!("cancel:{id}").into(), Effect::Cancel(run)));
            }
            let background = graph
                .read_key(&run_key(id, "meta"))
                .and_then(|m| get(m, "background"))
                == Some(&Value::Bool(true));
            let ended = graph
                .read_key(&run_key(id, "status"))
                .and_then(as_str)
                .and_then(RunStatus::parse)
                .is_none_or(RunStatus::is_terminal);
            if background && !ended {
                found.push((format!("detach:{id}").into(), Effect::Detach(run)));
            }
        }

        let live: HashSet<SmolStr> = found.iter().map(|(k, _)| k.clone()).collect();
        let orch = self.wired.as_ref().map(|w| w.orch.clone());
        for (key, effect) in found {
            if !self.started.insert(key) {
                continue;
            }
            match (effect, &orch) {
                (Effect::Spawn(spec), _) => self.spawn_run(spec),
                (Effect::Cancel(run), Some(o)) => o.cancel(run),
                (Effect::Detach(run), Some(o)) => o.detach(run),
                // Nothing to detach without an orchestrator.
                (Effect::Detach(_), None) => {}
                (other, _) => (self.sink)(other),
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
        model: get(v, "model")
            .and_then(as_str)
            .filter(|m| !m.is_empty())
            .map(str::to_owned),
        account: get(v, "account")
            .and_then(as_str)
            .filter(|a| !a.is_empty())
            .map(|a| AccountName(a.into())),
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
