//! Delegation (01 section 4): the limits, `@agent-<name>`, and the runtime half of the
//! orchestrator that the `delegate` tool and the effects executor share.
//!
//! The graph decides what may start (`model:orchestrator:runnable`, fed by `Limits`). This side
//! knows which runs are live, so it can start one, cancel one, answer a foreground `delegate`
//! when its child ends, and hand a background result to the parent's mailbox.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use aibus_graph::{Graph, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{roster, AgentDef, Isolation, Role, Scope};
use crate::config;
use crate::effects::SpawnSpec;
use crate::error::AibusError;
use crate::ids::{AgentName, RunId};
use crate::msg::{Command, Msg};
use crate::run::{
    result, RunConfig, RunDeps, RunError, RunErrorKind, RunMail, RunResult,
    RunStatus, Start,
};
use crate::state::store::Store;
use crate::state::value::{as_i64, get, map, n};
use crate::state::{ORCH_COUNTS, RUNS_ORDER};

/// The limits of 01 section 4.4. They reach the graph as `model:limits` and the `delegate` tool
/// as the check it makes before it asks for a run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Limits {
    /// Nesting depth below the lead.
    pub max_spawn_depth: u8,
    pub max_concurrent_runs: u32,
    /// Slots an account has unless `per_account` names it (`rate.max_concurrent`).
    pub max_per_account: u32,
    pub max_runs_per_session: u32,
    pub per_account: BTreeMap<String, u32>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_spawn_depth: 3,
            max_concurrent_runs: 8,
            max_per_account: 4,
            max_runs_per_session: 500,
            per_account: BTreeMap::new(),
        }
    }
}

impl Limits {
    /// `[limits]` from config, then `AIBUS_MAX_SPAWN_DEPTH` and `AIBUS_MAX_CONCURRENT_RUNS` on
    /// top (06 section 7). `env` reads a variable. Each account's `rate.max_concurrent` is its
    /// slot count.
    pub fn resolve(
        cfg: &config::Config,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, AibusError> {
        let number = |name: &str| -> Result<Option<u32>, AibusError> {
            match env(name) {
                None => Ok(None),
                Some(v) => v.trim().parse().map(Some).map_err(|_| {
                    AibusError::Usage(format!("{name}: want a whole number, got {v:?}"))
                }),
            }
        };
        let depth = number("AIBUS_MAX_SPAWN_DEPTH")?;
        let l = &cfg.limits;
        Ok(Self {
            max_spawn_depth: depth.map_or(l.max_spawn_depth, |d| d.min(255) as u8),
            max_concurrent_runs: number("AIBUS_MAX_CONCURRENT_RUNS")?
                .unwrap_or(l.max_concurrent_runs),
            max_per_account: Self::default().max_per_account,
            max_runs_per_session: l.max_runs_per_session,
            per_account: cfg
                .accounts
                .iter()
                .map(|a| (a.name.clone(), a.rate.max_concurrent))
                .collect(),
        })
    }

    /// The value of `model:limits`.
    pub fn to_value(&self) -> Value {
        let per_account = Value::Map(Arc::new(
            self.per_account
                .iter()
                .map(|(k, v)| (k.as_str().into(), n(*v)))
                .collect(),
        ));
        map([
            ("max_spawn_depth", n(self.max_spawn_depth)),
            ("max_concurrent_runs", n(self.max_concurrent_runs)),
            ("max_per_account", n(self.max_per_account)),
            ("max_runs_per_session", n(self.max_runs_per_session)),
            ("per_account", per_account),
        ])
    }
}

/// `@agent-<name> <rest>` at the start of a prompt: the name and the rest. `None` when the prompt
/// does not start with a mention, or the name is not a valid agent name.
pub fn parse_mention(text: &str) -> Option<(AgentName, &str)> {
    let body = text.trim_start().strip_prefix("@agent-")?;
    let (name, rest) = body
        .split_once(char::is_whitespace)
        .unwrap_or((body, ""));
    Some((name.parse().ok()?, rest.trim_start()))
}

/// What a `delegate` call may name: the workers, and the text that goes in its description.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Roster {
    /// Delegatable agents, and whether the definition forces them into the background.
    pub agents: Vec<(AgentName, bool)>,
    /// One `- name: description` line per non-built-in definition (01 section 1.5).
    pub text: String,
    pub builtins: Vec<AgentName>,
    /// Agents whose definition says `isolation: worktree`.
    pub isolated: Vec<AgentName>,
}

impl Roster {
    pub fn of<'a>(defs: impl IntoIterator<Item = &'a AgentDef> + Clone) -> Self {
        let workers = || defs.clone().into_iter().filter(|d| d.role == Role::Worker);
        Self {
            agents: workers().map(|d| (d.name.clone(), d.background)).collect(),
            text: roster::text(defs.clone()),
            builtins: workers()
                .filter(|d| d.source.scope == Scope::Builtin)
                .map(|d| d.name.clone())
                .collect(),
            isolated: workers()
                .filter(|d| d.isolation == Isolation::Worktree)
                .map(|d| d.name.clone())
                .collect(),
        }
    }

    pub fn forces_background(&self, agent: &AgentName) -> bool {
        self.agents.iter().any(|(a, bg)| a == agent && *bg)
    }
}

/// How a foreground wait ended.
#[derive(Debug)]
pub enum Waited {
    Done(Box<RunResult>),
    /// `Ctrl+b`: the result will arrive in the parent's mailbox instead.
    Detached,
}

/// Builds what a queued run needs to start. The binary implements it with the real accounts,
/// tools and hooks; a test implements it with fakes.
pub trait Launcher: Send + Sync {
    /// `tx` is where the run reports; it takes the place of `RunDeps::tx` so the orchestrator
    /// sees the run end. `Err` is why the run cannot start.
    fn launch(
        &self,
        spec: &SpawnSpec,
        prompt: String,
        tx: mpsc::Sender<Msg>,
    ) -> Result<(RunConfig, RunDeps), String>;
}

#[derive(Default)]
struct Tally {
    active: u32,
    queued: u32,
    total: u32,
}

struct Live {
    mailbox: mpsc::Sender<RunMail>,
    cancel: CancellationToken,
}

#[derive(Default)]
struct Inner {
    roster: Roster,
    tally: Tally,
    /// Runs a tool asked for that the graph has not shown yet.
    pending: HashSet<RunId>,
    live: HashMap<RunId, Live>,
    waiting: HashMap<RunId, oneshot::Sender<Waited>>,
    parent_of: HashMap<RunId, RunId>,
    /// Every run the graph has shown, oldest first, with its agent: what `resume_run` and
    /// `send_message` look names and ids up in.
    known: Vec<(RunId, AgentName)>,
}

pub struct Orchestrator {
    limits: Limits,
    tx: mpsc::Sender<Msg>,
    inner: Mutex<Inner>,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Orchestrator {
    pub fn new(limits: Limits, tx: mpsc::Sender<Msg>) -> Arc<Self> {
        Arc::new(Self {
            limits,
            tx,
            inner: Mutex::new(Inner::default()),
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The agents `delegate` offers, from the current definitions.
    pub fn set_roster(&self, roster: Roster) {
        lock(&self.inner).roster = roster;
    }

    pub fn roster(&self) -> Roster {
        lock(&self.inner).roster.clone()
    }

    /// Takes a run that something else started (the lead), so children can reach its mailbox and
    /// it counts as a live parent.
    pub fn adopt(&self, run: RunId, mailbox: mpsc::Sender<RunMail>, cancel: CancellationToken) {
        lock(&self.inner).live.insert(run, Live { mailbox, cancel });
    }

    /// A live run's mailbox, for a permission answer or a message.
    pub fn mailbox(&self, run: RunId) -> Option<mpsc::Sender<RunMail>> {
        lock(&self.inner).live.get(&run).map(|l| l.mailbox.clone())
    }

    /// The run a tool names: an id, or an agent name (the latest run of that agent). `Err` is
    /// what the tool says back.
    pub fn resolve(&self, key: &str) -> Result<RunId, String> {
        let g = lock(&self.inner);
        if let Ok(id) = key.parse::<RunId>() {
            return match g.known.iter().any(|(r, _)| *r == id) || g.live.contains_key(&id) {
                true => Ok(id),
                false => Err(format!("no run {key} in this session")),
            };
        }
        g.known
            .iter()
            .rev()
            .find(|(_, a)| a.as_str() == key)
            .map(|(r, _)| *r)
            .ok_or_else(|| format!("no run of an agent named {key} in this session"))
    }

    /// Whether `run` is going (it holds a mailbox).
    pub fn is_live(&self, run: RunId) -> bool {
        lock(&self.inner).live.contains_key(&run)
    }

    /// `send_message` (01 section 5.4): queues `text` for a run that is still going, which reads
    /// it at its next turn. `Err` is what the tool says back.
    pub fn message(&self, run: RunId, text: String) -> Result<(), String> {
        match self.mailbox(run) {
            None => Err(format!(
                "run {run} has finished; use resume_run to continue it"
            )),
            Some(m) => m
                .try_send(RunMail::UserMessage(text))
                .map_err(|_| format!("run {run} cannot take another message right now")),
        }
    }

    /// Refreshes the counts the limit check reads from the graph, and forgets the requests the
    /// graph now shows.
    pub fn observe(&self, graph: &dyn Graph) {
        let count = |k: &str| {
            graph
                .read_key(ORCH_COUNTS)
                .and_then(|c| get(c, k))
                .and_then(as_i64)
                .unwrap_or(0) as u32
        };
        let (running, queued, done, failed) = (
            count("running"),
            count("queued"),
            count("done"),
            count("failed"),
        );
        let mut g = lock(&self.inner);
        g.tally = Tally {
            active: running,
            queued,
            total: running + queued + done + failed,
        };
        if let Some(Value::List(order)) = graph.read_key(RUNS_ORDER) {
            // Runs are only ever added to the end, and an agent never changes.
            for id in order.iter().skip(g.known.len()) {
                let run = crate::state::value::as_str(id).and_then(|i| i.parse::<RunId>().ok());
                let agent = run
                    .and_then(|r| graph.read_key(&crate::state::run_key(r, "meta")))
                    .and_then(|m| get(m, "agent"))
                    .and_then(crate::state::value::as_str)
                    .and_then(|a| a.parse::<AgentName>().ok());
                if let (Some(run), Some(agent)) = (run, agent) {
                    g.known.push((run, agent));
                } else {
                    break;
                }
            }
        }
        if !g.pending.is_empty() {
            if let Some(Value::List(order)) = graph.read_key(RUNS_ORDER) {
                for id in order.iter() {
                    if let Some(id) = crate::state::value::as_str(id).and_then(|i| i.parse().ok()) {
                        g.pending.remove(&id);
                    }
                }
            }
        }
    }

    /// The check a `delegate` call makes (01 section 4.4): `Err` is the text of the tool result's
    /// `error`. On success the run has an id, and a foreground call has the receiver its result
    /// will arrive on. Account slots are not checked here: the account is only known when the
    /// run starts, so a run that finds its account full waits in the queue.
    pub fn reserve(
        &self,
        parent: RunId,
        background: bool,
    ) -> Result<(RunId, Option<oneshot::Receiver<Waited>>), String> {
        self.reserve_as(parent, background, None)
    }

    /// `reserve`, for a run that exists already when `again` names it (`resume_run`): it is not
    /// a new run for the session's count, and the graph already shows it.
    pub fn reserve_as(
        &self,
        parent: RunId,
        background: bool,
        again: Option<RunId>,
    ) -> Result<(RunId, Option<oneshot::Receiver<Waited>>), String> {
        let l = &self.limits;
        let mut g = lock(&self.inner);
        let pending = g.pending.len() as u32;
        if g.tally.active + g.tally.queued + pending >= l.max_concurrent_runs {
            return Err(format!(
                "concurrent run limit reached ({})",
                l.max_concurrent_runs
            ));
        }
        if again.is_none() && g.tally.total + pending >= l.max_runs_per_session {
            return Err(format!("session run limit reached ({})", l.max_runs_per_session));
        }
        let id = again.unwrap_or_else(RunId::new);
        if again.is_none() {
            g.pending.insert(id);
        }
        g.parent_of.insert(id, parent);
        let rx = (!background).then(|| {
            let (tx, rx) = oneshot::channel();
            g.waiting.insert(id, tx);
            rx
        });
        Ok((id, rx))
    }

    /// `Ctrl+b`: the foreground wait on `run` ends now and its result goes to the parent's
    /// mailbox when the run does. No-op when nobody waits.
    pub fn detach(&self, run: RunId) {
        if let Some(w) = lock(&self.inner).waiting.remove(&run) {
            let _ = w.send(Waited::Detached);
        }
    }

    /// `/compact`: the run compacts at its next turn boundary.
    pub fn compact(&self, run: RunId) {
        if let Some(m) = self.mailbox(run) {
            let _ = m.try_send(RunMail::Compact);
        }
    }

    pub fn cancel(&self, run: RunId) {
        if let Some(l) = lock(&self.inner).live.get(&run) {
            l.cancel.cancel();
        }
    }

    /// Cancels every live run (shutdown).
    pub fn cancel_all(&self) {
        for l in lock(&self.inner).live.values() {
            l.cancel.cancel();
        }
    }

    /// A run ended: the foreground waiter gets it, else the parent's mailbox does (wrapped, 01
    /// section 5.3).
    pub fn finished(&self, r: &RunResult) {
        let mut g = lock(&self.inner);
        g.live.remove(&r.run_id);
        let parent = g.parent_of.remove(&r.run_id);
        if let Some(w) = g.waiting.remove(&r.run_id) {
            let _ = w.send(Waited::Done(Box::new(r.clone())));
            return;
        }
        let mailbox = parent.and_then(|p| g.live.get(&p)).map(|l| l.mailbox.clone());
        drop(g);
        if let Some(m) = mailbox {
            let _ = m.try_send(RunMail::ChildFinished(result::wrap(r)));
        }
    }

    /// Starts a queued run. Idempotent by id: a run that is live is left alone. A run whose parent
    /// is not live is cancelled instead of started. A fork first asks its parent for the
    /// conversation it starts from, so it is on its way once the parent reaches a turn boundary.
    pub fn start(
        self: &Arc<Self>,
        launcher: &Arc<dyn Launcher>,
        store: &Store,
        spec: SpawnSpec,
    ) {
        let (parent_gone, parent_mail) = {
            let g = lock(&self.inner);
            if g.live.contains_key(&spec.run) {
                return;
            }
            let parent = spec.parent.and_then(|p| g.live.get(&p));
            (
                spec.parent.is_some() && parent.is_none(),
                parent.map(|l| l.mailbox.clone()),
            )
        };
        if parent_gone {
            self.send_later(Msg::Command(Command::Cancel { run: spec.run }));
            return;
        }
        let prompt = match &spec.prompt {
            Value::Str(t) => t.to_string(),
            Value::Handle(h) => store.text(h).map(|t| t.to_string()).unwrap_or_default(),
            _ => String::new(),
        };
        let start = if spec.resume { Start::Resume } else { Start::New };
        if !spec.fork {
            return self.launch(launcher, &spec, prompt, start);
        }
        let (reply, seed) = oneshot::channel();
        let asked = parent_mail.is_some_and(|m| m.try_send(RunMail::Fork { reply }).is_ok());
        if !asked {
            return self.refuse(&spec, "the run to fork from cannot be reached".into());
        }
        let (me, launcher) = (self.clone(), launcher.clone());
        tokio::spawn(async move {
            match seed.await {
                Ok(mut conv) => {
                    conv.push_user_text(prompt.clone());
                    me.launch(&launcher, &spec, prompt, Start::Fork(Box::new(conv)));
                }
                Err(_) => me.refuse(&spec, "the run to fork from ended first".into()),
            }
        });
    }

    fn launch(
        self: &Arc<Self>,
        launcher: &Arc<dyn Launcher>,
        spec: &SpawnSpec,
        prompt: String,
        start: Start,
    ) {
        let (tap, mut seen) = mpsc::channel(crate::app::MSG_CAPACITY);
        let (mut cfg, deps): (RunConfig, RunDeps) = match launcher.launch(spec, prompt, tap) {
            Ok(built) => built,
            Err(why) => return self.refuse(spec, why),
        };
        cfg.start = start;
        let handle = crate::run::spawn(cfg, deps);
        {
            let mut g = lock(&self.inner);
            g.live.insert(
                spec.run,
                Live {
                    mailbox: handle.mailbox,
                    cancel: handle.cancel,
                },
            );
            if let Some(p) = spec.parent {
                g.parent_of.insert(spec.run, p);
            }
        }
        // Everything the run says goes on to the loop; its end is also told to the orchestrator,
        // after the loop has it.
        let (me, out) = (self.clone(), self.tx.clone());
        tokio::spawn(async move {
            while let Some(msg) = seen.recv().await {
                let ended = match &msg {
                    Msg::RunFinished { result, .. } => Some(result.clone()),
                    _ => None,
                };
                if out.send(msg).await.is_err() {
                    break;
                }
                if let Some(result) = ended {
                    me.finished(&result);
                }
            }
        });
    }

    /// A queued run that could not be built ends `Failed`, which also answers a foreground wait.
    fn refuse(&self, spec: &SpawnSpec, why: String) {
        let error = RunError {
            kind: RunErrorKind::Internal,
            message: format!("could not start {}: {why}", spec.agent),
            retryable: false,
        };
        self.send_later(Msg::RunFailed {
            run: spec.run,
            error: error.clone(),
        });
        self.finished(&RunResult {
            run_id: spec.run,
            agent: spec.agent.clone(),
            status: RunStatus::Failed,
            turns: 0,
            usage: Default::default(),
            summary: error.message.clone(),
            summary_file: None,
            duration: Default::default(),
            error: Some(error),
            worktree: None,
        });
    }

    fn send_later(&self, msg: Msg) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(msg).await;
        });
    }
}
