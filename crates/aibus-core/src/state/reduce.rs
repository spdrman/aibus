//! `reduce(graph, msg) -> writes` (02 section 3, 03 section 6). No IO. The only side effect is
//! on the in-memory side store (sealing a block, pushing a log row), which is the spec's own
//! design: `reduce` is what bumps handle versions.
//!
//! Writes are keyed by `NodeKey`, not `Node`, because a message that creates a run writes
//! nodes that do not exist yet; `step` registers them just before the commit.

use aibus_graph::{Graph, NodeKey, Value};
use aibus_provider::StreamChunk;
use indexmap::IndexMap;
use ulid::Ulid;

use super::store::Store;
use super::value::{as_i64, as_list, as_str, get, list, map, n, pushed, s, with};
use super::*;
use crate::agent::{AgentDef, Isolation, Role};
use crate::error::{AibusError, CredentialError};
use crate::ids::{PermissionId, RunId};
use crate::msg::{AccountView, Command, Decision, Msg, Pane, Selection};
use crate::orchestrator::parse_mention;
use crate::run::{Progress, RunStatus, Usage};

/// What `reduce` may not read from the world itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env {
    pub now_ms: u64,
    pub ticks_dropped: u64,
}

pub type Writes = Vec<(NodeKey, Value)>;

static NULL: Value = Value::Null;
const ADVICE_KEEP: usize = 8;

/// A run to put in the queue. Four callers fill different subsets (the lead's first prompt,
/// `Delegate`, `Fork` and an `@agent-` mention), so the fields travel together.
struct Queue<'a> {
    agent: &'a str,
    role: Option<Role>,
    prompt: &'a str,
    background: bool,
    isolation: Isolation,
    parent: Option<RunId>,
    run: Option<RunId>,
    model: Option<&'a str>,
    account: Option<&'a str>,
    /// Starts from the conversation of `parent`, whose agent it runs.
    fork: bool,
}

impl<'a> Queue<'a> {
    fn new(
        agent: &'a str,
        prompt: &'a str,
        background: bool,
        isolation: Isolation,
        parent: Option<RunId>,
    ) -> Self {
        Self {
            agent,
            role: None,
            prompt,
            background,
            isolation,
            parent,
            run: None,
            model: None,
            account: None,
            fork: false,
        }
    }
}

struct R<'a> {
    g: &'a dyn Graph,
    store: &'a Store,
    env: Env,
    out: IndexMap<NodeKey, Value>,
}

impl R<'_> {
    fn cur(&self, key: &str) -> &Value {
        self.g.read_key(key).unwrap_or(&NULL)
    }

    /// The pending write for `key`, else the graph's value.
    fn now(&self, key: &str) -> Value {
        self.out
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.cur(key).clone())
    }

    fn put(&mut self, key: impl Into<NodeKey>, v: Value) {
        self.out.insert(key.into(), v);
    }

    fn log(&mut self, level: &str, text: &str) {
        let row = map([
            ("at", n(self.env.now_ms)),
            ("level", s(level)),
            ("text", s(text)),
        ]);
        let h = self.store.log_push(row);
        self.out.insert(LOG.into(), Value::Handle(h));
    }

    fn status(&self, run: &str) -> Option<RunStatus> {
        as_str(self.cur(&run_key(run, "status"))).and_then(RunStatus::parse)
    }

    fn set_status(&mut self, run: impl std::fmt::Display, st: RunStatus) {
        self.put(run_key(run, "status"), s(&st.as_value()));
    }

    fn run_exists(&self, run: &str) -> bool {
        as_list(self.cur(RUNS_ORDER))
            .iter()
            .any(|v| as_str(v) == Some(run))
    }

    fn def(&self, agent: &str) -> Option<Value> {
        as_list(self.cur(AGENTS_DEFS))
            .iter()
            .find(|d| get(d, "name").and_then(as_str) == Some(agent))
            .cloned()
    }

    /// Creates the nodes of a run and appends it to the order. Returns the transcript entry.
    fn new_run(&mut self, id: &str, meta: Value, status: RunStatus) {
        let entry = self.store.new_transcript();
        let (blocks, tail) = self.store.handles(entry);
        let order = pushed(&self.now(RUNS_ORDER), s(id));
        self.put(RUNS_ORDER, order);
        self.put(run_key(id, "meta"), meta);
        self.put(run_key(id, "status"), s(&status.as_value()));
        self.put(run_key(id, "turns"), n(0));
        self.put(run_key(id, "usage"), zero_usage());
        self.put(run_key(id, "progress"), Value::Null);
        self.put(run_key(id, "blocks"), Value::Handle(blocks));
        self.put(run_key(id, "tail"), Value::Handle(tail));
        self.put(run_key(id, "error"), Value::Null);
        self.put(run_key(id, "cancel_requested"), Value::Bool(false));
    }

    /// A deterministic id: the clock for the time part, the session and a counter for the rest.
    fn next_run_id(&mut self) -> RunId {
        let seq = as_i64(self.cur(RUN_SEQ)).unwrap_or(0) + 1;
        self.put(RUN_SEQ, n(seq));
        let session = as_str(self.cur(SESSION_ID))
            .and_then(|t| Ulid::from_string(t).ok())
            .map_or(0, |u| u.random());
        let random = ((session & 0xFFFF_FFFF_FFFF) << 16) | (seq as u128 & 0xFFFF);
        RunId(Ulid::from_parts(self.env.now_ms, random))
    }

    fn depth_of(&self, parent: Option<RunId>) -> i64 {
        parent.map_or(0, |p| {
            get(self.cur(&run_key(p, "meta")), "depth")
                .and_then(as_i64)
                .map_or(0, |d| d + 1)
        })
    }

    fn queue_run(&mut self, q: Queue) {
        let max = get(self.cur(LIMITS), "max_runs_per_session")
            .and_then(as_i64)
            .unwrap_or(500);
        if as_list(self.cur(RUNS_ORDER)).len() as i64 >= max {
            self.log("warn", "run limit reached, nothing queued");
            return;
        }
        // A command that arrives twice queues one run.
        if q.run.is_some_and(|id| self.run_exists(&id.to_string())) {
            return;
        }
        let id = q.run.unwrap_or_else(|| self.next_run_id());
        let def = self.def(q.agent);
        let role = q
            .role
            .or_else(|| {
                def.as_ref()
                    .and_then(|d| get(d, "role"))
                    .and_then(as_str)
                    .and_then(Role::parse)
            })
            .unwrap_or(Role::Worker);
        // A definition with `background: true` forces background (01 section 4.3).
        let background = q.background
            || def
                .as_ref()
                .and_then(|d| get(d, "background"))
                .is_some_and(|b| *b == Value::Bool(true));
        let account = q
            .account
            .or_else(|| {
                def.as_ref()
                    .and_then(|d| get(d, "account"))
                    .and_then(as_str)
            })
            .unwrap_or("")
            .to_owned();
        let label: String = q
            .prompt
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(48)
            .collect();
        let meta = map([
            ("agent", s(q.agent)),
            ("role", s(role.as_str())),
            (
                "parent",
                q.parent.map_or(Value::Null, |p| s(&p.to_string())),
            ),
            ("depth", n(self.depth_of(q.parent))),
            // What the caller asked for; `RunStarted` replaces it with what the run got.
            ("model", s(q.model.unwrap_or(""))),
            ("account", s(&account)),
            ("label", s(&label)),
            ("started_at", Value::Null),
            ("background", Value::Bool(background)),
            ("isolation", s(q.isolation.as_str())),
            ("prompt", self.store.inline_or_handle(q.prompt)),
        ]);
        let meta = if q.fork {
            with(&meta, "fork", Value::Bool(true))
        } else {
            meta
        };
        // A parent that already ended can no longer take the result: the run is never started.
        let orphan = q.parent.is_some_and(|p| {
            self.status(&p.to_string())
                .is_some_and(RunStatus::is_terminal)
        });
        let status = if orphan {
            RunStatus::Cancelled
        } else {
            RunStatus::Queued
        };
        self.new_run(&id.to_string(), meta, status);
        self.log("info", &format!("queued {}", q.agent));
    }

    /// Queued runs below `parent` can no longer start (their parent ended): cancels them, and
    /// the queued runs below those.
    fn cancel_queued_below(&mut self, parent: &str) {
        let order = as_list(self.cur(RUNS_ORDER)).to_vec();
        let mut dead = vec![parent.to_owned()];
        // Children come after their parents in the order, so one pass reaches them all.
        for id in order.iter().filter_map(as_str) {
            let of_dead = get(self.cur(&run_key(id, "meta")), "parent")
                .and_then(as_str)
                .is_some_and(|p| dead.iter().any(|d| d == p));
            if of_dead && self.status(id) == Some(RunStatus::Queued) {
                self.set_status(id, RunStatus::Cancelled);
                dead.push(id.to_owned());
            }
        }
    }

    fn transcript_id(&self, run: &str) -> Option<u64> {
        match self.cur(&run_key(run, "blocks")) {
            Value::Handle(h) => Some(h.id),
            _ => None,
        }
    }

    fn add_usage(&self, base: &Value, delta: &Usage) -> Value {
        let g = |k: &str| get(base, k).and_then(as_i64).unwrap_or(0);
        let cost = match get(base, "cost_usd") {
            Some(Value::F64(c)) => *c,
            _ => 0.0,
        };
        usage_value(
            g("input_tokens") + delta.input_tokens as i64,
            g("output_tokens") + delta.output_tokens as i64,
            g("cache_read") + delta.cache_read as i64,
            g("cache_write") + delta.cache_write as i64,
            cost + delta.cost_usd,
        )
    }

    fn decide(&mut self, id: PermissionId, decision: Decision) {
        let open = as_list(self.cur(PERMISSIONS_OPEN)).to_vec();
        let key = id.to_string();
        let Some(req) = open
            .iter()
            .find(|r| get(r, "id").and_then(as_str) == Some(&key))
            .cloned()
        else {
            return;
        };
        let rest: Vec<Value> = open
            .into_iter()
            .filter(|r| get(r, "id").and_then(as_str) != Some(&key))
            .collect();
        self.put(PERMISSIONS_OPEN, list(rest));
        let run = get(&req, "run").and_then(as_str).unwrap_or("").to_owned();
        if self.status(&run) == Some(RunStatus::WaitingPermission(id)) {
            self.set_status(&run, RunStatus::Running);
        }
        let call = get(&req, "call").and_then(as_str).unwrap_or("").to_owned();
        let pending = as_list(&self.now(TOOLS_PENDING)).to_vec();
        let next: Vec<Value> = pending
            .into_iter()
            .filter_map(|p| {
                if get(&p, "id").and_then(as_str) != Some(&call) {
                    Some(p)
                } else if decision == Decision::Deny {
                    None
                } else {
                    Some(with(&p, "approved", Value::Bool(true)))
                }
            })
            .collect();
        self.put(TOOLS_PENDING, list(next));
    }

    fn command(&mut self, c: &Command) {
        match c {
            Command::Delegate {
                agent,
                prompt,
                background,
                isolation,
                parent,
                run,
                model,
                account,
            } => self.queue_run(Queue {
                run: *run,
                model: model.as_deref(),
                account: account.as_ref().map(|a| a.0.as_str()),
                ..Queue::new(agent.as_str(), prompt, *background, *isolation, *parent)
            }),
            Command::Background { run } => {
                let key = run_key(run, "meta");
                if self.run_exists(&run.to_string()) {
                    let meta = with(self.cur(&key), "background", Value::Bool(true));
                    self.put(key, meta);
                    self.log("info", "moved to the background");
                }
            }
            Command::Fork { prompt, parent, run } => {
                // The user's `/fork` has no parent of its own: it forks the lead.
                let parent = parent.or_else(|| {
                    as_list(self.cur(RUNS_ORDER))
                        .first()
                        .and_then(as_str)
                        .and_then(|id| id.parse().ok())
                });
                let meta = parent.map(|p| self.cur(&run_key(p, "meta")).clone());
                let agent = meta.as_ref().and_then(|m| get(m, "agent")).and_then(as_str);
                match (parent, agent) {
                    (Some(parent), Some(agent)) => {
                        let role = get(meta.as_ref().unwrap_or(&NULL), "role")
                            .and_then(as_str)
                            .and_then(Role::parse);
                        let agent = agent.to_owned();
                        self.queue_run(Queue {
                            role,
                            run: *run,
                            fork: true,
                            ..Queue::new(&agent, prompt, true, Isolation::None, Some(parent))
                        })
                    }
                    _ => self.log("warn", "nothing to fork"),
                }
            }
            Command::Cancel { run } => {
                self.put(run_key(run, "cancel_requested"), Value::Bool(true));
                if self.status(&run.to_string()) == Some(RunStatus::Queued) {
                    self.set_status(run, RunStatus::Cancelled);
                    self.cancel_queued_below(&run.to_string());
                }
            }
            Command::Resume {
                run,
                message,
                parent,
            } => {
                let id = run.to_string();
                if !self.status(&id).is_some_and(RunStatus::is_terminal) {
                    self.log("warn", "only a finished run can be resumed");
                    return;
                }
                let key = run_key(run, "meta");
                let meta = with(
                    self.cur(&key),
                    "parent",
                    parent.map_or(Value::Null, |p| s(&p.to_string())),
                );
                let meta = with(&meta, "prompt", self.store.inline_or_handle(message));
                self.put(key, with(&meta, "resume", Value::Bool(true)));
                self.set_status(run, RunStatus::Queued);
                self.put(run_key(run, "error"), Value::Null);
                self.put(run_key(run, "cancel_requested"), Value::Bool(false));
            }
            Command::Retry { run } => {
                self.set_status(run, RunStatus::Queued);
                self.put(run_key(run, "error"), Value::Null);
                self.put(run_key(run, "cancel_requested"), Value::Bool(false));
            }
            Command::Permission { id, decision } => self.decide(*id, *decision),
            Command::Focus(p) => self.put(FOCUS, s(p.as_str())),
            Command::Scroll(p, d) => {
                let key = scroll_key(p.as_str());
                let next = (as_i64(self.cur(&key)).unwrap_or(0) + i64::from(*d)).max(0);
                self.put(key, n(next));
            }
            Command::Select(pane, sel) => {
                let (key, clear) = match pane {
                    Pane::Cards => (SELECTED_CARD, n(0)),
                    Pane::Log => (SELECTED_LOG, Value::Null),
                    _ => return,
                };
                let cur = as_i64(self.cur(key)).unwrap_or(0);
                let v = match sel {
                    Selection::Index(i) => n((*i).max(0)),
                    Selection::Next => n(cur + 1),
                    Selection::Prev => n((cur - 1).max(0)),
                    Selection::Clear => clear,
                };
                self.put(key, v);
            }
            Command::OpenTranscript(run) => self.put(MODAL, s(&format!("transcript:{run}"))),
            Command::CloseModal => self.put(MODAL, Value::Null),
            Command::OpenPalette => self.put(MODAL, s("palette")),
            Command::OpenHelp => self.put(MODAL, s("help")),
            Command::LedgerUpdate { section, text } => {
                if ["goal", "now", "in_progress"].contains(&section.as_str()) {
                    let v = self.store.inline_or_handle(text);
                    let l = with(self.cur(LEDGER), section, v);
                    self.put(LEDGER, l);
                } else {
                    self.log("warn", "unknown ledger section");
                }
            }
            Command::Compact { run } => self.set_status(run, RunStatus::Compacting),
            Command::Clear => {
                let h = self.store.log_clear();
                self.put(LOG, Value::Handle(h));
            }
            Command::Quit { force } => {
                let why = if *force { "forced" } else { "user_quit" };
                self.put(SHUTDOWN, s(why));
            }
            // Delivery to a run's mailbox, workflows and handoffs are effects that land with
            // the orchestrator; the model only records that they were asked for.
            Command::SendMessage { .. }
            | Command::Workflow { .. }
            | Command::CreateHandoff { .. } => {
                self.log("info", "queued request");
            }
        }
    }
}

/// `/compact` and `/fork <task>` typed while a lead runs: the command, aimed at the lead.
fn slash(text: &str, lead: Option<&str>) -> Option<Command> {
    let lead: RunId = lead?.parse().ok()?;
    let (word, rest) = text.trim().split_once(char::is_whitespace).unwrap_or((text.trim(), ""));
    match word {
        "/compact" => Some(Command::Compact { run: lead }),
        "/fork" if !rest.trim().is_empty() => Some(Command::Fork {
            prompt: rest.trim().to_owned(),
            parent: Some(lead),
            run: None,
        }),
        _ => None,
    }
}

pub fn usage_value(i: i64, o: i64, cr: i64, cw: i64, cost: f64) -> Value {
    map([
        ("input_tokens", n(i)),
        ("output_tokens", n(o)),
        ("cache_read", n(cr)),
        ("cache_write", n(cw)),
        ("cost_usd", Value::F64(cost)),
    ])
}

fn usage_abs(u: &Usage) -> Value {
    usage_value(
        u.input_tokens as i64,
        u.output_tokens as i64,
        u.cache_read as i64,
        u.cache_write as i64,
        u.cost_usd,
    )
}

fn def_summary(store: &Store, d: &AgentDef) -> Value {
    map([
        ("name", s(d.name.as_str())),
        ("description", s(&d.description)),
        ("role", s(d.role.as_str())),
        (
            "account",
            d.account.as_ref().map_or(Value::Null, |a| s(&a.0)),
        ),
        ("background", Value::Bool(d.background)),
        ("isolation", s(d.isolation.as_str())),
        ("max_turns", d.max_turns.map_or(Value::Null, n)),
        // Prompt bodies never enter the graph.
        ("prompt", Value::Handle(store.put_text(&d.system_prompt))),
    ])
}

fn account_value(a: &AccountView) -> Result<Value, AibusError> {
    let ok = a.key_ref == "none"
        || ["keyring:", "env:", "file:"]
            .iter()
            .any(|p| a.key_ref.starts_with(p));
    if !ok {
        return Err(CredentialError::NotAReference(a.name.clone()).into());
    }
    Ok(map([
        ("name", s(&a.name.0)),
        ("provider", s(&a.provider)),
        ("label", s(&a.label)),
        (
            "default_model",
            a.default_model.as_ref().map_or(Value::Null, |m| s(&m.0)),
        ),
        ("key_ref", s(&a.key_ref)),
    ]))
}

/// The writes for one message. Always includes `model:last_msg` and `eng:msg_seq`, so every
/// message changes the graph and therefore produces exactly one commit.
pub fn reduce(g: &dyn Graph, store: &Store, msg: &Msg, env: Env) -> Result<Writes, AibusError> {
    let mut r = R {
        g,
        store,
        env,
        out: IndexMap::new(),
    };
    arm(&mut r, msg)?;

    let seq = as_i64(r.cur(MSG_SEQ)).unwrap_or(0) + 1;
    r.put(MSG_SEQ, n(seq));
    r.put(LAST_MSG, with(&msg.summary(), "seq", n(seq)));
    let clock = (env.now_ms / 1000 * 1000) as i64;
    if as_i64(r.cur(CLOCK_MS)) != Some(clock) {
        r.put(CLOCK_MS, n(clock));
    }
    // The first message starts the 30 s clock; without this a fresh session would look as if
    // it had not been saved since 1970.
    if as_i64(r.cur(SNAPSHOT_MS)) == Some(0) && env.now_ms > 0 {
        r.put(SNAPSHOT_MS, n(env.now_ms));
    }
    if matches!(msg, Msg::Persisted) {
        r.put(SNAPSHOT_SEQ, n(seq));
        r.put(SNAPSHOT_MS, n(env.now_ms));
    }
    Ok(r.out.into_iter().collect())
}

fn arm(r: &mut R, msg: &Msg) -> Result<(), AibusError> {
    match msg {
        Msg::Key(_) | Msg::Mouse(_) | Msg::Persisted => {}
        Msg::Resize { cols, rows } => r.put(SIZE, map([("cols", n(*cols)), ("rows", n(*rows))])),
        Msg::Tick => {
            let t = as_i64(r.cur(TICK)).unwrap_or(0) + 1;
            r.put(TICK, n(t));
            r.put(TICKS_DROPPED, n(r.env.ticks_dropped));
        }
        Msg::SubmitPrompt { text } => {
            if as_list(r.cur(RUNS_ORDER)).is_empty() {
                r.queue_run(Queue {
                    role: Some(Role::Lead),
                    ..Queue::new("lead", text, false, Isolation::None, None)
                });
            } else if let Some(c) = slash(text, as_list(r.cur(RUNS_ORDER)).first().and_then(as_str)) {
                r.command(&c);
            } else if let Some((agent, rest)) = parse_mention(text) {
                // `@agent-<name> <rest>` is a forced delegate; the lead still gets the result.
                let lead = as_list(r.cur(RUNS_ORDER))
                    .first()
                    .and_then(as_str)
                    .and_then(|id| id.parse().ok());
                r.queue_run(Queue::new(agent.as_str(), rest, true, Isolation::None, lead));
            } else {
                r.log("info", "prompt for the running lead");
            }
        }
        Msg::Command(c) => r.command(c),
        Msg::RunStarted {
            run,
            agent,
            role,
            parent,
            model,
            account,
        } => {
            let id = run.to_string();
            let old = r.cur(&run_key(&id, "meta")).clone();
            let had = r.run_exists(&id);
            let base = |k: &str, d: Value| get(&old, k).cloned().unwrap_or(d);
            let meta = map([
                ("agent", s(agent.as_str())),
                ("role", s(role.as_str())),
                ("parent", parent.map_or(Value::Null, |p| s(&p.to_string()))),
                (
                    "depth",
                    if had {
                        base("depth", n(0))
                    } else {
                        n(r.depth_of(*parent))
                    },
                ),
                ("model", s(&model.0)),
                ("account", s(&account.0)),
                ("label", base("label", s(agent.as_str()))),
                ("started_at", n(r.env.now_ms)),
                ("background", base("background", Value::Bool(false))),
                ("isolation", base("isolation", s("none"))),
                ("prompt", base("prompt", s(""))),
            ]);
            // Only a fork carries the mark, and a resume's is spent once the run starts.
            let meta = match get(&old, "fork") {
                Some(f) => with(&meta, "fork", f.clone()),
                None => meta,
            };
            if had {
                r.put(run_key(&id, "meta"), meta);
                r.set_status(&id, RunStatus::Starting);
            } else {
                r.new_run(&id, meta, RunStatus::Starting);
            }
            r.log("info", &format!("started {agent}"));
        }
        Msg::Stream { run, chunks } => {
            let id = run.to_string();
            let Some(entry) = r.transcript_id(&id) else {
                return Ok(());
            };
            let mut text = String::new();
            let mut usage: Option<Value> = None;
            let mut waiting: Option<&str> = None;
            for c in chunks {
                match c {
                    StreamChunk::TextDelta(t) => text.push_str(t),
                    StreamChunk::Waiting(t) => waiting = Some(t),
                    StreamChunk::Usage(u) => {
                        let base = usage
                            .clone()
                            .unwrap_or_else(|| r.cur(&run_key(&id, "usage")).clone());
                        usage = Some(r.add_usage(&base, u));
                    }
                    _ => {}
                }
            }
            if !text.is_empty() {
                let sealed = r.store.append(entry, &text);
                let (blocks, tail) = r.store.handles(entry);
                r.put(run_key(&id, "tail"), Value::Handle(tail));
                if sealed {
                    r.put(run_key(&id, "blocks"), Value::Handle(blocks));
                }
            }
            if let Some(u) = usage {
                r.put(run_key(&id, "usage"), u);
            }
            // The card shows `waiting for <account> slot` while the rate limit holds it back.
            if let Some(w) = waiting {
                let note = if w.is_empty() { Value::Null } else { s(w) };
                let meta = with(r.cur(&run_key(&id, "meta")), "waiting", note);
                r.put(run_key(&id, "meta"), meta);
            }
            let started = chunks.iter().any(|c| !matches!(c, StreamChunk::Waiting(_)));
            if started && r.status(&id) == Some(RunStatus::Starting) {
                r.set_status(&id, RunStatus::Running);
            }
        }
        Msg::ToolCallRequested { run, call } => {
            let entry = map([
                ("id", s(&call.id.0.to_string())),
                ("run", s(&run.to_string())),
                ("name", s(&call.name)),
                ("input", r.store.tool_input(&call.input)),
                ("approved", Value::Bool(!call.needs_permission)),
            ]);
            r.put(TOOLS_PENDING, pushed(r.cur(TOOLS_PENDING), entry));
            r.log("info", &format!("tool {} requested", call.name));
            // `ask_user` parks the run until the user answers.
            let id = run.to_string();
            if call.name == "ask_user"
                && matches!(r.status(&id), Some(RunStatus::Running | RunStatus::Starting))
            {
                r.set_status(&id, RunStatus::WaitingUser);
            }
        }
        Msg::ToolCallFinished {
            run,
            call_id,
            result,
            duration,
        } => {
            let key = call_id.0.to_string();
            let pending = as_list(r.cur(TOOLS_PENDING)).to_vec();
            let name = pending
                .iter()
                .find(|p| get(p, "id").and_then(as_str) == Some(&key))
                .and_then(|p| get(p, "name"))
                .and_then(as_str)
                .unwrap_or("tool")
                .to_owned();
            let rest: Vec<Value> = pending
                .into_iter()
                .filter(|p| get(p, "id").and_then(as_str) != Some(&key))
                .collect();
            r.put(TOOLS_PENDING, list(rest));
            let ok = if result.is_error { "failed" } else { "ok" };
            r.log("info", &format!("{name} {ok} {} ms", duration.as_millis()));
            let id = run.to_string();
            if name == "ask_user" && r.status(&id) == Some(RunStatus::WaitingUser) {
                r.set_status(&id, RunStatus::Running);
            }
            if !result.artifacts.is_empty() {
                let mut found = as_list(get(r.cur(&run_key(&id, "meta")), "artifacts").unwrap_or(&Value::Null)).to_vec();
                for a in &result.artifacts {
                    found.push(map([
                        ("name", s(&a.name)),
                        ("kind", s(&a.kind)),
                        ("path", s(&a.path.to_string_lossy())),
                    ]));
                }
                let meta = with(r.cur(&run_key(&id, "meta")), "artifacts", list(found));
                r.put(run_key(&id, "meta"), meta);
            }
            match result.progress {
                Some(Progress::Fraction(f)) => r.put(run_key(&id, "progress"), Value::F64(f64::from(f))),
                Some(Progress::Done) => r.put(run_key(&id, "progress"), Value::F64(1.0)),
                Some(Progress::Indeterminate) => r.put(run_key(&id, "progress"), Value::Null),
                None => {}
            }
        }
        Msg::PermissionNeeded { request } => {
            let entry = map([
                ("id", s(&request.id.to_string())),
                ("agent", s(request.agent.as_str())),
                ("run", s(&request.run.to_string())),
                ("call", s(&request.call.0.to_string())),
                ("tool", s(&request.tool)),
                ("summary", r.store.inline_or_handle(&request.summary)),
            ]);
            r.put(PERMISSIONS_OPEN, pushed(r.cur(PERMISSIONS_OPEN), entry));
            r.set_status(request.run, RunStatus::WaitingPermission(request.id));
            let call = request.call.0.to_string();
            let pending: Vec<Value> = as_list(r.cur(TOOLS_PENDING))
                .iter()
                .map(|p| {
                    if get(p, "id").and_then(as_str) == Some(&call) {
                        with(p, "approved", Value::Bool(false))
                    } else {
                        p.clone()
                    }
                })
                .collect();
            r.put(TOOLS_PENDING, list(pending));
        }
        Msg::PermissionDecided { id, decision, .. } => r.decide(*id, *decision),
        Msg::RunFinished { run, result } => {
            let id = run.to_string();
            r.set_status(&id, result.status);
            r.put(run_key(&id, "turns"), n(result.turns));
            r.put(run_key(&id, "usage"), usage_abs(&result.usage));
            if result.status == RunStatus::Completed {
                r.put(run_key(&id, "progress"), Value::F64(1.0));
            }
            let meta = with(r.cur(&run_key(&id, "meta")), "ended_at", n(r.env.now_ms));
            r.put(run_key(&id, "meta"), meta);
            if let Some(entry) = r.transcript_id(&id) {
                r.store.evict_finished(entry);
                let (blocks, tail) = r.store.handles(entry);
                r.put(run_key(&id, "blocks"), Value::Handle(blocks));
                r.put(run_key(&id, "tail"), Value::Handle(tail));
            }
            r.log("info", &format!("finished {}", result.status.as_value()));
            r.cancel_queued_below(&id);
        }
        Msg::RunFailed { run, error } => {
            let id = run.to_string();
            r.set_status(&id, RunStatus::Failed);
            r.put(
                run_key(&id, "error"),
                map([
                    ("kind", s(error.kind.as_str())),
                    ("message", s(&error.message)),
                    ("retryable", Value::Bool(error.retryable)),
                ]),
            );
            r.log("error", &format!("failed: {}", error.kind.as_str()));
            r.cancel_queued_below(&id);
        }
        Msg::UsageUpdate { run, usage } => r.put(run_key(run, "usage"), usage_abs(usage)),
        Msg::HookFired {
            run,
            event,
            outcome,
        } => {
            let run_s = run.map(|x| x.to_string());
            let mut removed = false;
            let due: Vec<Value> = as_list(r.cur(HOOKS_DUE))
                .iter()
                .filter(|h| {
                    let same = get(h, "event").and_then(as_str) == Some(event.as_str())
                        && get(h, "run").and_then(as_str) == run_s.as_deref();
                    let drop = same && !removed;
                    removed |= drop;
                    !drop
                })
                .cloned()
                .collect();
            r.put(HOOKS_DUE, list(due));
            // A hook that failed to run says so (`hook error: <name>`); one that ran just
            // reports its exit code.
            match &outcome.message {
                Some(m) if m.starts_with("hook error: ") => r.log("warn", m),
                _ => r.log(
                    "info",
                    &format!("hook {} exit {}", event.as_str(), outcome.exit_code),
                ),
            }
        }
        Msg::RouterAnswered {
            fork,
            question,
            answer,
        } => {
            let forks = as_i64(r.cur(ROUTER_FORKS)).unwrap_or(0).max(fork.0 as i64);
            r.put(ROUTER_FORKS, n(forks));
            r.put(
                router_last(&question.kind),
                map([
                    ("choice", s(&answer.choice)),
                    ("p", Value::F64(answer.p)),
                    ("verdict", s(answer.verdict.as_str())),
                    ("at", n(r.env.now_ms)),
                ]),
            );
            let queue: Vec<Value> = as_list(r.cur(ROUTER_QUEUE))
                .iter()
                .filter(|q| get(q, "fork").and_then(as_i64) != Some(fork.0 as i64))
                .cloned()
                .collect();
            r.put(ROUTER_QUEUE, list(queue));
            r.log("info", &format!("router {} answered", question.kind));
        }
        Msg::CheckpointHit { checkpoint, .. } => {
            let a = with(
                r.cur(ADVISOR_STATE),
                "active_checkpoint",
                s(checkpoint.name()),
            );
            r.put(ADVISOR_STATE, with(&a, "status", s("reading")));
            r.log("info", &format!("checkpoint {}", checkpoint.name()));
        }
        Msg::AdvisorSpoke {
            checkpoint,
            advice,
            tokens_read,
        } => {
            let st = r.cur(ADVISOR_STATE).clone();
            let calls = get(&st, "calls").and_then(as_i64).unwrap_or(0) + 1;
            let read = get(&st, "tokens_read").and_then(as_i64).unwrap_or(0) + *tokens_read as i64;
            let st = with(&st, "calls", n(calls));
            let st = with(&st, "tokens_read", n(read));
            let st = with(&st, "status", s("on_call"));
            r.put(ADVISOR_STATE, with(&st, "active_checkpoint", Value::Null));
            let mut all = as_list(r.cur(ADVISOR_ADVICE)).to_vec();
            all.push(r.store.inline_or_handle(advice));
            let skip = all.len().saturating_sub(ADVICE_KEEP);
            r.put(ADVISOR_ADVICE, list(all.split_off(skip)));
            r.log("info", &format!("advisor spoke at {}", checkpoint.name()));
        }
        Msg::Compacted { run, .. } => {
            if r.status(&run.to_string()) == Some(RunStatus::Compacting) {
                r.set_status(run, RunStatus::Running);
            }
            r.log("info", "compacted");
        }
        Msg::HandoffWritten { path } => {
            let p = path.to_string_lossy();
            r.put(
                HANDOFF_LATEST,
                map([
                    ("path", r.store.inline_or_handle(&p)),
                    ("at", n(r.env.now_ms)),
                ]),
            );
            r.log("info", "handoff written");
        }
        Msg::AgentsReloaded { defs, warnings } => {
            r.put(
                AGENTS_DEFS,
                list(defs.iter().map(|d| def_summary(r.store, d)).collect()),
            );
            r.put(
                AGENTS_WARNINGS,
                list(warnings.iter().map(|w| s(w)).collect()),
            );
            r.log("info", &format!("{} agents loaded", defs.len()));
        }
        Msg::AccountsReloaded { accounts } => {
            let items = accounts
                .iter()
                .map(account_value)
                .collect::<Result<Vec<_>, _>>()?;
            r.put(ACCOUNTS, list(items));
            r.log("info", &format!("{} accounts loaded", accounts.len()));
        }
        Msg::IoError { context, .. } => {
            // A compaction that failed leaves the run running again (`Compact` set `compacting`).
            if let Some(run) = context.strip_prefix("compacting ") {
                if r.status(run) == Some(RunStatus::Compacting) {
                    r.set_status(run, RunStatus::Running);
                }
            }
            r.log("error", &format!("io error in {context}"))
        }
        Msg::Shutdown { reason } => r.put(SHUTDOWN, s(reason.as_str())),
    }
    Ok(())
}
