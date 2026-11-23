//! The deterministic DAG scheduler (#288, spec sections 4.2, 5.2 and 5.3). It takes a compiled plan
//! (the `validate -> plan` half lives in `compile`) and decides, one call at a time, which steps
//! may start: dependencies ready, routes taken, Jev's routing decision, the approvals a step holds,
//! the parallel and run budgets. It runs nothing itself. A host starts what `advance` returns and
//! tells the scheduler how each step ended, so the same plan and the same sequence of outcomes
//! always give the same actions and the same event log, with no clock, no randomness and no model
//! call anywhere in the path.
//!
//! Every decision is a typed `Event` carrying its reason code, and the run header names the plan
//! hash and the policy revision that made them. The whole scheduler is serializable, so the host
//! persists the pending decision before it surfaces it (`save`), and a restart resumes it.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::graph::{Cond, Pred};
use super::handoff::{self, Context, Party};
use super::jev::{self, Request};
use super::{compile, Mode, Runtime};

/// The run's state (spec 5.3). A plan that fails `Scheduler::new` never gets past validation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    /// Nothing is running and a step that may have changed the world outside aibus was cut off.
    /// Steps that do not depend on it still start; it, and whatever depends on it, wait until a
    /// person says whether it happened (`reconcile`), and the run cannot finish before that.
    RequiresReconciliation,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Ready,
    WaitingApproval,
    Running,
    Completed,
    Failed {
        retryable: bool,
        reason: String,
    },
    /// No route led here, or every route that did was not taken.
    Skipped,
    Cancelled,
    Interrupted,
    /// Policy could not decide, or a limit or safety rule stopped it; a person is told.
    Escalated,
    /// It has irreversible side effects and was running when the host went away, so it may have
    /// happened. Never started again on its own: a person answers with `reconcile`.
    RequiresReconciliation,
}

impl Status {
    fn terminal(&self) -> bool {
        matches!(
            self,
            Status::Completed
                | Status::Failed { .. }
                | Status::Skipped
                | Status::Cancelled
                | Status::Escalated
        )
    }
}

/// How a host says a step it started ended.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// `artifact_valid` is what `<step>.artifact_valid` reads in a route.
    Completed { artifact_valid: bool },
    /// `retryable` is the host's classification of the error (a timeout is, a refusal is not).
    Failed { retryable: bool, reason: String },
}

/// What the host should do now.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Start {
        step: String,
        runtime: String,
        mode: Mode,
        attempt: u32,
    },
    /// A running step lost its approval (its account moved to a provider nobody approved).
    Stop { step: String, reason: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    RunState,
    Started,
    Finished,
    ApprovalRequested,
    ApprovalGranted,
    ApprovalDenied,
    Skipped,
    Deferred,
    Rerun,
    Escalated,
    RequiresReconciliation,
    Reconciled,
}

/// One logged decision. `reason_code` says which rule made it; `detail` carries the Jev request and
/// decision, the holds, or the limit, whatever the code needs.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub kind: EventKind,
    pub step: Option<String>,
    pub reason_code: String,
    pub detail: Value,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
struct Node {
    key: String,
    runtime: Runtime,
    request: Request,
    /// Capabilities the plan holds for a person's yes before this step runs.
    holds: Vec<String>,
    isolated: bool,
    status: Status,
    attempts: u32,
    /// Holds granted for the attempt that is current; a re-run asks again.
    granted: BTreeSet<String>,
    account: Option<(String, String)>,
    artifact_valid: bool,
    /// The last thing logged about this step while it waited, so a repeat is not logged again.
    waiting_on: Option<String>,
    /// A vendor CLI step cut off by a restart. Its session is the vendor's and is not restored,
    /// so starting it over in a new session waits for a person (`restart:vendor_session`).
    #[serde(default)]
    fresh_session: bool,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
struct Edge {
    from: usize,
    to: usize,
    when: String,
    /// A route back (a loop, or a chain's retry): taken at most `max` times.
    back: bool,
    max: u32,
    taken: u32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Scheduler {
    plan_hash: String,
    policy_revision: String,
    /// The hash of the rules this file applies on top of Jev's (limits, holds, retries), so a
    /// log says which scheduler made it.
    scheduler_revision: String,
    state: RunState,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Data edges between scheduled steps, as (from, to).
    data: Vec<(usize, usize)>,
    max_parallel: u32,
    max_runs: Option<u32>,
    max_wall: Option<u32>,
    started: u32,
    halted: bool,
    /// Runtimes the user approved for this run (`openrouter`).
    approved: BTreeSet<String>,
    /// Data edges the user agreed to, as `from->to`.
    consented: BTreeSet<String>,
    /// The preview hashes of handoffs already delivered. Pass it to `handoff::deliver`; it is saved
    /// with the run, so a restart does not deliver an old consent again.
    pub spent_handoffs: BTreeSet<String>,
    events: Vec<Event>,
}

fn holds_cond(c: &Cond, fact: &dyn Fn(&str, Pred) -> bool) -> bool {
    match c {
        Cond::Is { step, what } => fact(step, *what),
        Cond::Not(c) => !holds_cond(c, fact),
        Cond::All(cs) => cs.iter().all(|c| holds_cond(c, fact)),
        Cond::Any(cs) => cs.iter().any(|c| holds_cond(c, fact)),
    }
}

impl Scheduler {
    /// Validates `plan` and queues the run: the plan has to be one this build compiled (its hash
    /// matches its content), made under the current policy revision, with every route, sequence and
    /// retry edge between steps it schedules and no cycle among the forward routes. `Err` is every
    /// reason it was refused.
    pub fn new(plan: &Value) -> Result<Self, Vec<String>> {
        let mut bad = Vec::new();
        if plan["kind"] != "aibus.workflow.plan" {
            return Err(vec!["not a workflow plan".into()]);
        }
        if plan["plan_hash"].as_str() != Some(compile::plan_hash(plan).as_str()) {
            bad.push(
                "the plan_hash does not match the plan: it was edited after it was planned".into(),
            );
        }
        let policy = jev::policy_revision();
        if plan["jev"]["policy_revision"].as_str() != Some(policy.as_str()) {
            bad.push("the plan was made under another policy revision: plan it again".into());
        }
        let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
        let mut nodes: Vec<Node> = Vec::new();
        for s in list(&plan["steps"]) {
            // A native subagent lives in its parent's process; aibus does not schedule it.
            if s["mode"] == "native" {
                continue;
            }
            let key = s["key"].as_str().unwrap_or_default().to_owned();
            let request: Option<Request> = list(&plan["jev"]["decisions"])
                .iter()
                .find(|d| d["step"] == key.as_str())
                .and_then(|d| serde_json::from_value(d["request"].clone()).ok());
            let runtime: Option<Runtime> = serde_json::from_value(s["runtime"].clone()).ok();
            let (Some(request), Some(runtime)) = (request, runtime) else {
                bad.push(format!(
                    "step {key}: no routing request or unknown runtime in the plan"
                ));
                continue;
            };
            nodes.push(Node {
                holds: plan["approvals"]["steps"][&key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                isolated: s["isolation"] == "git_worktree",
                key,
                runtime,
                request,
                status: Status::Pending,
                attempts: 0,
                granted: BTreeSet::new(),
                account: None,
                artifact_valid: false,
                waiting_on: None,
                fresh_session: false,
            });
        }
        let at = |k: &Value| nodes.iter().position(|n| k == n.key.as_str());
        let mut edges = Vec::new();
        let mut data = Vec::new();
        for e in list(&plan["edges"]) {
            let kind = e["kind"].as_str().unwrap_or_default();
            let (Some(from), Some(to)) = (at(&e["from"]), at(&e["to"])) else {
                // A data edge from `input` or a native step has no scheduled end, and that is
                // fine. A route without one would leave its target with no way in, so the target
                // would start at once: refused, whatever the plan hash says.
                if matches!(kind, "sequence" | "retry" | "route" | "loop") {
                    bad.push(format!(
                        "{kind} edge {} -> {}: not a step this plan schedules",
                        e["from"], e["to"]
                    ));
                }
                continue;
            };
            let (when, back, max) = match kind {
                "sequence" => (format!("{}.completed", nodes[from].key), false, 0),
                "retry" => (
                    format!("{}.failed", nodes[from].key),
                    true,
                    e["max_iterations"].as_u64().unwrap_or(0) as u32,
                ),
                "route" | "loop" => {
                    let when = e["when"].as_str().unwrap_or_default().to_owned();
                    if let Err(why) = Cond::parse(&when) {
                        bad.push(format!(
                            "edge {} -> {}: {why}",
                            nodes[from].key, nodes[to].key
                        ));
                    }
                    (
                        when,
                        kind == "loop",
                        e["max_iterations"].as_u64().unwrap_or(0) as u32,
                    )
                }
                "data" => {
                    data.push((from, to));
                    continue;
                }
                _ => continue,
            };
            edges.push(Edge {
                from,
                to,
                when,
                back,
                max,
                taken: 0,
            });
        }
        if nodes.is_empty() {
            bad.push("the plan has no step aibus schedules".into());
        }
        // Kahn over the forward edges: a cycle is an invalid DAG, refused before anything starts.
        let mut into = vec![0usize; nodes.len()];
        for e in edges.iter().filter(|e| !e.back) {
            into[e.to] += 1;
        }
        let mut stack: Vec<usize> = (0..nodes.len()).filter(|&i| into[i] == 0).collect();
        let mut seen = 0;
        while let Some(i) = stack.pop() {
            seen += 1;
            for e in edges.iter().filter(|e| !e.back && e.from == i) {
                into[e.to] -= 1;
                if into[e.to] == 0 {
                    stack.push(e.to);
                }
            }
        }
        if seen != nodes.len() {
            bad.push(
                "the forward routes have a cycle; only a route with max_iterations may go back"
                    .into(),
            );
        }
        if !bad.is_empty() {
            return Err(bad);
        }
        let env = &plan["envelope"];
        let limit = |k: &str| env[k].as_u64().map(|v| v as u32);
        Ok(Scheduler {
            plan_hash: plan["plan_hash"].as_str().unwrap_or_default().to_owned(),
            policy_revision: policy,
            scheduler_revision: compile::content_hash(
                "aibus-scheduler/2\nroute: a step starts when its forward routes are settled and one was taken\nvalidate: routes only between scheduled steps\nholds: an aibus step waits for its account; the OpenRouter approval holds for the run; the plan's approvals and handoff consent are asked per attempt\nlimits: max_parallel (default 1), max_agent_runs, write-exclusive without isolation, wall clock\nretry: bounded, retryable only, never over a step with side effects after a failure\nfailure: nothing new starts, a person is told\nresume: aibus and OpenRouter steps come back, a vendor CLI step waits for a yes to start a new session, a step with side effects requires reconciliation\n",
            ),
            state: RunState::Queued,
            nodes,
            edges,
            data,
            // No `max_parallel`: one step at a time, as the chain always ran.
            max_parallel: limit("max_parallel").unwrap_or(1).max(1),
            max_runs: limit("max_agent_runs"),
            max_wall: limit("max_wall_seconds"),
            started: 0,
            halted: false,
            approved: BTreeSet::new(),
            consented: BTreeSet::new(),
            spent_handoffs: BTreeSet::new(),
            events: Vec::new(),
        })
    }

    pub fn state(&self) -> RunState {
        self.state
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn plan_hash(&self) -> &str {
        &self.plan_hash
    }

    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
    }

    pub fn scheduler_revision(&self) -> &str {
        &self.scheduler_revision
    }

    pub fn status(&self, step: &str) -> Option<&Status> {
        self.nodes.iter().find(|n| n.key == step).map(|n| &n.status)
    }

    /// Every scheduled step and its status, in plan order.
    pub fn steps(&self) -> impl Iterator<Item = (&str, &Status)> {
        self.nodes.iter().map(|n| (n.key.as_str(), &n.status))
    }

    /// What a step is held for: `account:unresolved` (an `aibus` step whose account the host has
    /// not set yet; only `set_account` clears it), `approval:<capability>`, `runtime:openrouter` or
    /// `handoff:<from>-><to>`. Show the last three, then `approve`.
    pub fn pending(&self, step: &str) -> Vec<String> {
        self.nodes
            .iter()
            .position(|n| n.key == step)
            .map(|i| self.holds_for(i).1)
            .unwrap_or_default()
    }

    fn log(&mut self, kind: EventKind, step: Option<&str>, reason: &str, detail: Value) {
        let seq = self.events.len() as u64 + 1;
        self.events.push(Event {
            seq,
            kind,
            step: step.map(str::to_owned),
            reason_code: reason.to_owned(),
            detail,
        });
    }

    fn index(&self, step: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.key == step)
    }

    fn edge_active(&self, e: &Edge) -> bool {
        let fact = |step: &str, what: Pred| {
            self.nodes
                .iter()
                .find(|n| n.key == step)
                .is_some_and(|n| match what {
                    Pred::Completed => n.status == Status::Completed,
                    Pred::Failed => matches!(n.status, Status::Failed { .. }),
                    Pred::ArtifactValid => n.status == Status::Completed && n.artifact_valid,
                })
        };
        let from = &self.nodes[e.from];
        matches!(from.status, Status::Completed | Status::Failed { .. })
            && Cond::parse(&e.when).is_ok_and(|c| holds_cond(&c, &fact))
    }

    /// Moves every Pending step whose forward routes are all settled: Ready when one was taken,
    /// Skipped when none was.
    fn settle_pending(&mut self) {
        loop {
            let mut changed = false;
            for i in 0..self.nodes.len() {
                if self.nodes[i].status != Status::Pending {
                    continue;
                }
                let incoming: Vec<&Edge> =
                    self.edges.iter().filter(|e| !e.back && e.to == i).collect();
                let next = if incoming.is_empty() {
                    Some(Status::Ready)
                } else if incoming
                    .iter()
                    .all(|e| self.nodes[e.from].status.terminal())
                {
                    Some(if incoming.iter().any(|e| self.edge_active(e)) {
                        Status::Ready
                    } else {
                        Status::Skipped
                    })
                } else {
                    None
                };
                let halted = self.halted;
                if let Some(s) = next {
                    let key = self.nodes[i].key.clone();
                    if s == Status::Skipped || halted {
                        let reason = if halted {
                            "run_halted"
                        } else {
                            "route_not_taken"
                        };
                        self.nodes[i].status = if halted {
                            Status::Cancelled
                        } else {
                            Status::Skipped
                        };
                        self.log(EventKind::Skipped, Some(&key), reason, json!({}));
                    } else {
                        self.nodes[i].status = s;
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn request_for(&self, i: usize) -> Request {
        let n = &self.nodes[i];
        let mut r = n.request.clone();
        r.approved = self.approved.iter().cloned().collect();
        r.account_provider = n.account.as_ref().map(|(p, _)| p.clone());
        r
    }

    /// Jev's decision for step `i` and what it is still held for.
    fn holds_for(&self, i: usize) -> (jev::Decision, Vec<String>) {
        let party = |i: usize| {
            let n = &self.nodes[i];
            let (provider, account) = n
                .account
                .clone()
                .unwrap_or_else(|| ("unresolved".into(), "unresolved".into()));
            Party {
                step: n.key.clone(),
                runtime: n.runtime,
                provider: handoff::provider_of(n.runtime)
                    .map(str::to_owned)
                    .unwrap_or(provider),
                account,
            }
        };
        let decision = jev::decide(&self.request_for(i));
        let mut holds = Vec::new();
        // Jev can only hold an `aibus` step for OpenRouter once it knows the account, so until the
        // host says which one, the step waits (fail closed, like a handoff to an unknown account).
        if self.nodes[i].runtime == Runtime::Aibus && self.nodes[i].account.is_none() {
            holds.push("account:unresolved".to_owned());
        }
        if decision.reason_code == "openrouter_needs_approval" {
            holds.push("runtime:openrouter".to_owned());
        }
        for c in &self.nodes[i].holds {
            let h = format!("approval:{c}");
            if !self.nodes[i].granted.contains(&h) {
                holds.push(h);
            }
        }
        let restart = "restart:vendor_session";
        if self.nodes[i].fresh_session && !self.nodes[i].granted.contains(restart) {
            holds.push(restart.to_owned());
        }
        for &(from, _) in self.data.iter().filter(|d| d.1 == i) {
            let context = Context {
                run_id: String::new(),
                plan_hash: self.plan_hash.clone(),
                from: party(from),
                to: party(i),
            };
            let edge = format!("{}->{}", self.nodes[from].key, self.nodes[i].key);
            if handoff::needs_consent(&context) && !self.consented.contains(&edge) {
                holds.push(format!("handoff:{edge}"));
            }
        }
        (decision, holds)
    }

    /// The steps that may start now, in plan order. A step that must wait for a person moves to
    /// `WaitingApproval` and is not returned; call `pending` for what it is held for.
    pub fn advance(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        if matches!(
            self.state,
            RunState::Completed | RunState::Failed | RunState::Cancelled | RunState::Interrupted
        ) {
            return out;
        }
        self.settle_pending();
        for i in 0..self.nodes.len() {
            if !matches!(self.nodes[i].status, Status::Ready) || self.halted {
                continue;
            }
            let key = self.nodes[i].key.clone();
            let (decision, holds) = self.holds_for(i);
            if holds.iter().any(|h| h == "account:unresolved") {
                // Not a person's call: the host sets the account, then the step is decided.
                if self.nodes[i].waiting_on.as_deref() != Some("account_unresolved") {
                    self.nodes[i].waiting_on = Some("account_unresolved".to_owned());
                    self.log(
                        EventKind::Deferred,
                        Some(&key),
                        "account_unresolved",
                        json!({}),
                    );
                }
                continue;
            }
            let detail = json!({
                "request": self.request_for(i),
                "decision": decision,
                "policy_revision": self.policy_revision,
            });
            if decision.selected_runtime.is_none()
                && decision.reason_code != "openrouter_needs_approval"
            {
                self.nodes[i].status = Status::Escalated;
                self.log(
                    EventKind::Escalated,
                    Some(&key),
                    &decision.reason_code,
                    detail,
                );
                continue;
            }
            if !holds.is_empty() {
                self.nodes[i].status = Status::WaitingApproval;
                self.nodes[i].waiting_on = Some(holds.join(","));
                self.log(
                    EventKind::ApprovalRequested,
                    Some(&key),
                    "approval_required",
                    json!({ "holds": holds, "decision": detail["decision"], "policy_revision": self.policy_revision }),
                );
                continue;
            }
            if self.max_runs.is_some_and(|m| self.started >= m) {
                self.nodes[i].status = Status::Escalated;
                self.log(
                    EventKind::Escalated,
                    Some(&key),
                    "run_budget_exhausted",
                    json!({ "max_agent_runs": self.max_runs }),
                );
                continue;
            }
            let running = self
                .nodes
                .iter()
                .filter(|n| n.status == Status::Running)
                .count();
            let defer = if running as u32 >= self.max_parallel {
                Some("max_parallel")
            } else if self.nodes[i].request.risk_class != "read_only"
                && !self.nodes[i].isolated
                && self.nodes.iter().any(|n| {
                    n.status == Status::Running
                        && n.request.risk_class != "read_only"
                        && !n.isolated
                })
            {
                Some("write_exclusive")
            } else {
                None
            };
            if let Some(why) = defer {
                if self.nodes[i].waiting_on.as_deref() != Some(why) {
                    self.nodes[i].waiting_on = Some(why.to_owned());
                    let detail = if why == "max_parallel" {
                        json!({ "max_parallel": self.max_parallel, "running": running })
                    } else {
                        json!({})
                    };
                    self.log(EventKind::Deferred, Some(&key), why, detail);
                }
                continue;
            }
            let n = &mut self.nodes[i];
            n.status = Status::Running;
            n.attempts += 1;
            n.waiting_on = None;
            n.fresh_session = false;
            let attempt = n.attempts;
            self.started += 1;
            self.log(
                EventKind::Started,
                Some(&key),
                &decision.reason_code,
                detail,
            );
            out.push(Action::Start {
                step: key,
                runtime: decision.selected_runtime.unwrap_or_default(),
                mode: decision.execution_mode.unwrap_or_default(),
                attempt,
            });
        }
        self.settle();
        out
    }

    /// Settles the run's state from its steps.
    fn settle(&mut self) {
        if matches!(self.state, RunState::Cancelled | RunState::Interrupted) {
            return;
        }
        let any = |f: &dyn Fn(&Status) -> bool| self.nodes.iter().any(|n| f(&n.status));
        let next = if any(&|s| *s == Status::Running) {
            RunState::Running
        } else if any(&|s| *s == Status::RequiresReconciliation) {
            RunState::RequiresReconciliation
        } else if any(&|s| *s == Status::WaitingApproval) {
            RunState::WaitingApproval
        } else if self.nodes.iter().all(|n| n.status.terminal()) {
            let unhandled = self.nodes.iter().enumerate().any(|(i, n)| {
                n.status == Status::Escalated
                    || (matches!(n.status, Status::Failed { .. })
                        && !self
                            .edges
                            .iter()
                            .any(|e| !e.back && e.from == i && self.edge_active(e)))
            });
            if unhandled || self.halted {
                RunState::Failed
            } else {
                RunState::Completed
            }
        } else if any(&|s| *s == Status::Ready) {
            RunState::Running
        } else {
            RunState::Queued
        };
        if next != self.state {
            self.state = next;
            let name = json!(next);
            self.log(EventKind::RunState, None, name.as_str().unwrap_or_default(), json!({ "plan_hash": self.plan_hash, "policy_revision": self.policy_revision, "scheduler_revision": self.scheduler_revision }));
        }
    }

    /// Records how step `step` ended, then takes any route back its outcome calls for.
    pub fn report(&mut self, step: &str, outcome: Outcome) -> Result<(), String> {
        let i = self.index(step).ok_or_else(|| format!("no step {step}"))?;
        if self.nodes[i].status != Status::Running {
            return Err(format!("step {step} is not running"));
        }
        let (status, reason, failed) = match outcome {
            Outcome::Completed { artifact_valid } => {
                self.nodes[i].artifact_valid = artifact_valid;
                (Status::Completed, "completed".to_owned(), false)
            }
            Outcome::Failed { retryable, reason } => (
                Status::Failed {
                    retryable,
                    reason: reason.clone(),
                },
                reason,
                true,
            ),
        };
        self.nodes[i].status = status.clone();
        self.log(
            EventKind::Finished,
            Some(step),
            if failed { "failed" } else { "completed" },
            json!({ "attempt": self.nodes[i].attempts, "detail": reason }),
        );
        let backs: Vec<usize> = (0..self.edges.len())
            .filter(|&k| {
                self.edges[k].back && self.edges[k].from == i && self.edge_active(&self.edges[k])
            })
            .collect();
        for k in backs {
            let (to, max, taken) = (self.edges[k].to, self.edges[k].max, self.edges[k].taken);
            if taken >= max {
                if failed {
                    self.escalate_failure(i, "retries_exhausted");
                }
                continue;
            }
            // What the route back runs again: the steps from `to` forward that can reach `i`.
            let reach = |start: usize, rev: bool| {
                let mut seen = BTreeSet::from([start]);
                let mut stack = vec![start];
                while let Some(at) = stack.pop() {
                    for e in self.edges.iter().filter(|e| !e.back) {
                        let (a, b) = if rev { (e.to, e.from) } else { (e.from, e.to) };
                        if a == at && seen.insert(b) {
                            stack.push(b);
                        }
                    }
                }
                seen
            };
            let range: Vec<usize> = reach(to, false)
                .intersection(&reach(i, true))
                .copied()
                .collect();
            if failed {
                let retryable = matches!(
                    status,
                    Status::Failed {
                        retryable: true,
                        ..
                    }
                );
                if !retryable {
                    self.escalate_failure(i, "not_retryable");
                    continue;
                }
                // A failure is never answered by running an irreversible step again.
                if range
                    .iter()
                    .any(|&j| self.nodes[j].request.risk_class == "side_effects")
                {
                    self.escalate_failure(i, "irreversible_not_retried");
                    continue;
                }
            }
            self.edges[k].taken += 1;
            let keys: Vec<String> = range.iter().map(|&j| self.nodes[j].key.clone()).collect();
            for &j in &range {
                let n = &mut self.nodes[j];
                n.status = Status::Pending;
                n.granted.clear();
                n.waiting_on = None;
                n.artifact_valid = false;
                let key = n.key.clone();
                self.consented.retain(|c| !c.ends_with(&format!("->{key}")));
            }
            self.log(
                EventKind::Rerun,
                Some(step),
                if failed { "retry" } else { "loop" },
                json!({ "steps": keys, "iteration": self.edges[k].taken, "max": max }),
            );
            break;
        }
        if failed
            && matches!(self.nodes[i].status, Status::Failed { .. })
            && !self
                .edges
                .iter()
                .any(|e| !e.back && e.from == i && self.edge_active(e))
            && !self.halted
        {
            self.escalate_failure(i, "no_route_for_failure");
        }
        self.settle_pending();
        self.settle();
        Ok(())
    }

    /// A failure nothing handles: the run stops starting anything new and a person is told.
    fn escalate_failure(&mut self, i: usize, why: &str) {
        let key = self.nodes[i].key.clone();
        if !self.halted {
            let reason = match &self.nodes[i].status {
                Status::Failed { reason, .. } => reason.clone(),
                _ => String::new(),
            };
            self.log(
                EventKind::Escalated,
                Some(&key),
                why,
                json!({ "error": reason }),
            );
            // Nothing new starts: what has not started is cancelled, what is running finishes.
            self.halted = true;
            for j in 0..self.nodes.len() {
                if matches!(
                    self.nodes[j].status,
                    Status::Pending | Status::Ready | Status::WaitingApproval
                ) {
                    let key = self.nodes[j].key.clone();
                    self.nodes[j].status = Status::Cancelled;
                    self.log(EventKind::Skipped, Some(&key), "run_halted", json!({}));
                }
            }
        }
    }

    /// Grants everything `step` is waiting on, as `by`, and logs exactly what was granted.
    pub fn approve(&mut self, step: &str, by: &str) -> Result<(), String> {
        let i = self.index(step).ok_or_else(|| format!("no step {step}"))?;
        if by.trim().is_empty() {
            return Err("an approval needs the name of who gave it".into());
        }
        if self.nodes[i].status != Status::WaitingApproval {
            return Err(format!("step {step} is not waiting for approval"));
        }
        let holds = self.holds_for(i).1;
        for h in &holds {
            if let Some(rt) = h.strip_prefix("runtime:") {
                self.approved.insert(rt.to_owned());
            } else if let Some(edge) = h.strip_prefix("handoff:") {
                self.consented.insert(edge.to_owned());
            } else {
                self.nodes[i].granted.insert(h.clone());
            }
        }
        self.nodes[i].status = Status::Ready;
        self.nodes[i].waiting_on = None;
        self.log(
            EventKind::ApprovalGranted,
            Some(step),
            "approved",
            json!({ "by": by, "holds": holds }),
        );
        self.settle();
        Ok(())
    }

    /// The person said no: the step fails, not retryable, and no route is taken that needs it.
    pub fn deny(&mut self, step: &str, by: &str) -> Result<(), String> {
        let i = self.index(step).ok_or_else(|| format!("no step {step}"))?;
        if by.trim().is_empty() {
            return Err("a refusal needs the name of who gave it".into());
        }
        if self.nodes[i].status != Status::WaitingApproval {
            return Err(format!("step {step} is not waiting for approval"));
        }
        let holds = self.holds_for(i).1;
        self.nodes[i].status = Status::Failed {
            retryable: false,
            reason: "approval_denied".into(),
        };
        self.log(
            EventKind::ApprovalDenied,
            Some(step),
            "approval_denied",
            json!({ "by": by, "holds": holds }),
        );
        if !self
            .edges
            .iter()
            .any(|e| !e.back && e.from == i && self.edge_active(e))
        {
            self.escalate_failure(i, "approval_denied");
        }
        self.settle_pending();
        self.settle();
        Ok(())
    }

    /// Tells the scheduler which provider and account `step` runs under. A step that has not
    /// started is checked when it does. One already running (a per-account fallback moved it) is
    /// checked now: if the new party needs an approval nobody gave, it is stopped and waits.
    pub fn set_account(&mut self, step: &str, provider: &str, account: &str) -> Vec<Action> {
        let Some(i) = self.index(step) else {
            return Vec::new();
        };
        self.nodes[i].account = Some((provider.to_owned(), account.to_owned()));
        let mut out = Vec::new();
        if self.nodes[i].status == Status::Running {
            let holds = self.holds_for(i).1;
            if !holds.is_empty() {
                self.nodes[i].status = Status::WaitingApproval;
                self.nodes[i].waiting_on = Some(holds.join(","));
                self.log(
                    EventKind::ApprovalRequested,
                    Some(step),
                    "approval_rechecked",
                    json!({ "holds": holds }),
                );
                out.push(Action::Stop {
                    step: step.to_owned(),
                    reason: "approval_rechecked".into(),
                });
            }
        }
        self.settle();
        out
    }

    /// Cancels the run: nothing new starts, and the host stops what is running.
    pub fn cancel(&mut self, by: &str) -> Vec<Action> {
        let mut out = Vec::new();
        if matches!(
            self.state,
            RunState::Completed | RunState::Failed | RunState::Cancelled
        ) {
            return out;
        }
        for n in &mut self.nodes {
            if n.status == Status::Running {
                out.push(Action::Stop {
                    step: n.key.clone(),
                    reason: "cancelled".into(),
                });
            }
            // A cancel is not an answer to "did it happen", so that question stays open.
            if !n.status.terminal() && n.status != Status::RequiresReconciliation {
                n.status = Status::Cancelled;
            }
        }
        self.state = RunState::Cancelled;
        self.log(EventKind::RunState, None, "cancelled", json!({ "by": by }));
        out
    }

    /// The host went away with steps running (a restart, a lost connection).
    pub fn interrupt(&mut self) {
        if matches!(
            self.state,
            RunState::Completed | RunState::Failed | RunState::Cancelled
        ) {
            return;
        }
        for n in &mut self.nodes {
            if n.status == Status::Running {
                n.status = Status::Interrupted;
            }
        }
        self.state = RunState::Interrupted;
        self.log(EventKind::RunState, None, "interrupted", json!({}));
    }

    /// Picks an interrupted run up again. Only aibus-owned work comes back on its own: a step that
    /// was running and has irreversible side effects may have happened, so it goes to
    /// `RequiresReconciliation` for a person; a vendor CLI step comes back Ready but its session is
    /// not restored, so starting it over in a new one is held for a person too. Everything else
    /// that was running (an `aibus` or OpenRouter step, whose state aibus holds) is Ready again.
    pub fn resume(&mut self) {
        if self.state != RunState::Interrupted {
            return;
        }
        for i in 0..self.nodes.len() {
            if self.nodes[i].status != Status::Interrupted {
                continue;
            }
            let n = &mut self.nodes[i];
            let (key, runtime, attempt) = (n.key.clone(), n.runtime, n.attempts);
            n.granted.clear();
            let (kind, reason) = if n.request.risk_class == "side_effects" {
                n.status = Status::RequiresReconciliation;
                (
                    EventKind::RequiresReconciliation,
                    "irreversible_may_have_happened",
                )
            } else if matches!(runtime, Runtime::Aibus | Runtime::OpenRouter) {
                n.status = Status::Ready;
                (EventKind::Rerun, "resumed_aibus_owned")
            } else {
                n.status = Status::Ready;
                n.fresh_session = true;
                (EventKind::Rerun, "vendor_session_not_restored")
            };
            self.log(
                kind,
                Some(&key),
                reason,
                json!({ "attempt": attempt, "runtime": runtime }),
            );
        }
        self.state = RunState::Queued;
        self.settle();
    }

    /// A person's answer for a step in `RequiresReconciliation`, as `by`. `applied` says the
    /// action did happen: the step counts as completed, with no artifact to read. Otherwise it did
    /// not, and the step is Ready to run again, asking for its approvals afresh.
    pub fn reconcile(&mut self, step: &str, applied: bool, by: &str) -> Result<(), String> {
        let i = self.index(step).ok_or_else(|| format!("no step {step}"))?;
        if by.trim().is_empty() {
            return Err("a reconciliation needs the name of who checked".into());
        }
        if self.nodes[i].status != Status::RequiresReconciliation {
            return Err(format!("step {step} does not need reconciliation"));
        }
        // Not applied on a run that stopped starting things: it stays undone, it does not run.
        let stopped = self.halted || self.state == RunState::Cancelled;
        let n = &mut self.nodes[i];
        n.status = if applied {
            Status::Completed
        } else if stopped {
            Status::Cancelled
        } else {
            Status::Ready
        };
        n.artifact_valid = false;
        n.granted.clear();
        self.log(
            EventKind::Reconciled,
            Some(step),
            if applied { "applied" } else { "not_applied" },
            json!({ "by": by }),
        );
        self.settle_pending();
        self.settle();
        Ok(())
    }

    /// The wall clock the host measured. Past `max_wall_seconds` the run is stopped and escalated.
    pub fn elapsed(&mut self, seconds: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if self.max_wall.is_none_or(|m| seconds <= u64::from(m))
            || matches!(
                self.state,
                RunState::Completed | RunState::Failed | RunState::Cancelled
            )
        {
            return out;
        }
        self.halted = true;
        for n in &mut self.nodes {
            if n.status == Status::Running {
                out.push(Action::Stop {
                    step: n.key.clone(),
                    reason: "wall_clock_exhausted".into(),
                });
                n.status = Status::Cancelled;
            } else if !n.status.terminal() && n.status != Status::RequiresReconciliation {
                n.status = Status::Cancelled;
            }
        }
        self.log(
            EventKind::Escalated,
            None,
            "wall_clock_exhausted",
            json!({ "max_wall_seconds": self.max_wall, "elapsed": seconds }),
        );
        self.state = RunState::Failed;
        self.log(
            EventKind::RunState,
            None,
            "failed",
            json!({ "plan_hash": self.plan_hash }),
        );
        out
    }

    /// Writes the run to `path` atomically, the file owner-only (0600), so the pending decision is
    /// on disk before anyone is shown it. A directory it creates gets 0700; one that already exists
    /// keeps its mode (the file's own mode and owner are what `load` checks).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let dir = path.parent().unwrap_or(Path::new("."));
        #[cfg(unix)]
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        #[cfg(not(unix))]
        std::fs::create_dir_all(dir)?;
        // A leftover temp file (or a symlink planted in its place) is never written through:
        // it goes, and the new one is created exclusively, so it gets this mode and no other.
        let tmp = path.with_extension("tmp");
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// Reads a run `save` wrote. A symlink, a file someone else owns, or one that is not
    /// owner-only is refused: others could have edited the approvals in it.
    pub fn load(path: &Path) -> Result<Self, String> {
        use std::io::Read;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        // The checks run on the file that was opened, never on the path a second time, and a
        // symlink is not followed, so nothing can be swapped in between. Non-blocking, so a FIFO
        // planted here cannot hang the open.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
            opts.custom_flags(flags.bits() as i32);
        }
        let mut f = opts.open(path).map_err(|e| {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                format!("{}: not a regular file", path.display())
            } else {
                e.to_string()
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = f.metadata().map_err(|e| e.to_string())?;
            if !meta.file_type().is_file() {
                return Err(format!("{}: not a regular file", path.display()));
            }
            if meta.uid() != rustix::process::geteuid().as_raw() {
                return Err(format!(
                    "{}: owned by uid {}, not by you",
                    path.display(),
                    meta.uid()
                ));
            }
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                return Err(format!(
                    "{}: mode {:o}, want owner-only (0600)",
                    path.display(),
                    mode & 0o777
                ));
            }
        }
        let mut text = String::new();
        f.read_to_string(&mut text).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}

/// `validate -> plan -> run` without the run: what the scheduler would do with `plan`, assuming
/// every step completes with a valid artifact. Per step the route candidates (the edges out, with
/// their conditions), Jev's decision under the current policy, every approval that would be asked
/// for, and the artifact contract (its form, who reads it, which routes test it). `waves` are the
/// steps that can run together under `max_parallel`. `Err` is why the plan would be refused.
pub fn dry_run(plan: &Value) -> Result<Value, Vec<String>> {
    let mut s = Scheduler::new(plan)?;
    let mut steps = Vec::new();
    for (i, n) in s.nodes.iter().enumerate() {
        let (decision, mut holds) = s.holds_for(i);
        // What a run would still need from a person, besides what the plan holds.
        let handoffs: Vec<String> = holds
            .iter()
            .filter(|h| h.starts_with("handoff:"))
            .cloned()
            .collect();
        holds.retain(|h| !h.starts_with("handoff:") && !h.starts_with("account:"));
        let out: Vec<Value> = s
            .edges
            .iter()
            .filter(|e| e.from == i)
            .map(|e| json!({ "to": s.nodes[e.to].key, "when": e.when, "back": e.back, "max_iterations": e.back.then_some(e.max) }))
            .collect();
        let readers: Vec<&str> = s
            .data
            .iter()
            .filter(|d| d.0 == i)
            .map(|d| s.nodes[d.1].key.as_str())
            .collect();
        let tests: Vec<&str> = s
            .edges
            .iter()
            .filter(|e| e.when.contains(&format!("{}.artifact_valid", n.key)))
            .map(|e| s.nodes[e.to].key.as_str())
            .collect();
        let output = plan["steps"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["key"] == n.key.as_str()))
            .map(|x| x["output"].clone())
            .unwrap_or(Value::Null);
        steps.push(json!({
            "key": n.key,
            "route_candidates": out,
            "candidates": n.request.candidates,
            "decision": decision,
            "approvals": holds,
            "handoffs": handoffs,
            "artifact": { "output": output, "read_by": readers, "validity_tested_by_routes_to": tests },
        }));
    }
    // Accounts are picked when a step runs, so the simulation stands one in that every check treats
    // as unknown (a handoff into it still asks, and it is answered yes like the rest).
    for n in s.nodes.iter_mut().filter(|n| n.account.is_none()) {
        let unknown = "resolved when the step runs".to_owned();
        n.account = Some((unknown.clone(), unknown));
    }
    let mut waves: Vec<Vec<String>> = Vec::new();
    let mut guard = 0;
    while !matches!(s.state, RunState::Completed | RunState::Failed) && guard < 1024 {
        guard += 1;
        let mut started = s.advance();
        // Approvals are answered yes: the dry run asks what a run would ask, it does not wait.
        let waiting: Vec<String> = s
            .nodes
            .iter()
            .filter(|n| n.status == Status::WaitingApproval)
            .map(|n| n.key.clone())
            .collect();
        for w in waiting {
            let _ = s.approve(&w, "dry-run");
        }
        if started.is_empty() {
            started = s.advance();
        }
        if started.is_empty() {
            break;
        }
        let keys: Vec<String> = started
            .iter()
            .filter_map(|a| match a {
                Action::Start { step, .. } => Some(step.clone()),
                Action::Stop { .. } => None,
            })
            .collect();
        for k in &keys {
            let _ = s.report(
                k,
                Outcome::Completed {
                    artifact_valid: true,
                },
            );
        }
        waves.push(keys);
    }
    Ok(json!({
        "kind": "aibus.workflow.dry_run",
        "plan_hash": s.plan_hash,
        "policy_revision": s.policy_revision,
        "envelope": { "max_parallel": s.max_parallel, "max_agent_runs": s.max_runs, "max_wall_seconds": s.max_wall },
        "steps": steps,
        "waves": waves,
    }))
}
