//! An agent session that is already open, attached to this aibus session (#322, Phase 1 spike).
//!
//! Two directions share one attachment. The agent controls aibus through the tools #274 added
//! plus a read-only `present`, and aibus runs a step on the agent: the step is queued here, the
//! agent pulls it (`next_step`) and answers (`step_result`). Nothing here pushes: the pull leg is
//! the one every MCP client has, and a hook at turn end only reminds the agent to pull.
//!
//! Every rule that is not about drawing lives here so one test file can hold it: the session has
//! to opt in, the keyboard accepts the attachment and the first panel with a code a client cannot
//! type, one attachment, one step in flight, a deadline, a late or reused token is refused, the
//! agent may decline, a client that goes away fails its steps, and a session that descends from
//! this one cannot attach back. Time is passed in, never read.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use ulid::Ulid;

/// How long a request waits for the keyboard.
pub const ACCEPT_WITHIN: Duration = Duration::from_secs(60);
/// A lineage this long (or longer) cannot attach: an aibus session may be driven by an agent
/// that is itself driven by none, so a chain has depth 1 at most.
pub const MAX_LINEAGE: usize = 1;
/// The most a panel's title, message, table and cells hold.
const TITLE_MAX: usize = 80;
const MESSAGE_MAX: usize = 4096;
const ROWS_MAX: usize = 30;
const COLS_MAX: usize = 8;
const CELL_MAX: usize = 80;
/// How long a step has to be answered when nobody says otherwise (the keyboard's `/attach step`
/// and a workflow's attached step).
pub const STEP_WITHIN: Duration = Duration::from_secs(10 * 60);
/// The most a step's summary keeps.
const SUMMARY_MAX: usize = 16 * 1024;
/// Where a cancel got to for a step the session had already pulled (#346). The pull leg cannot
/// push, so the interrupt waits for the session's next call and is shown to it once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    None,
    Pending,
    Seen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Queued,
    Delivered,
    Done,
    Declined,
    Failed,
    Cancelled,
    TimedOut,
    Unavailable,
}

impl StepState {
    pub fn as_str(self) -> &'static str {
        match self {
            StepState::Queued => "queued",
            StepState::Delivered => "delivered",
            StepState::Done => "done",
            StepState::Declined => "declined",
            StepState::Failed => "failed",
            StepState::Cancelled => "cancelled",
            StepState::TimedOut => "timed_out",
            StepState::Unavailable => "unavailable",
        }
    }

    fn open(self) -> bool {
        matches!(self, StepState::Queued | StepState::Delivered)
    }
}

/// One step aibus asked the attached session to carry out.
#[derive(Clone, Debug)]
pub struct Step {
    pub token: String,
    pub name: String,
    pub prompt: String,
    pub state: StepState,
    pub summary: String,
    queued: Instant,
    delivered: Option<Instant>,
    /// When the session said it began (`step_ack`).
    started: Option<Instant>,
    /// Tool calls the session reported against this step's token, by name (capped), and how many.
    calls: Vec<String>,
    call_count: u32,
    pub interrupt: Interrupt,
    ended: Option<Instant>,
    deadline: Instant,
}

impl Step {
    /// What aibus knows about the step and how well, with the EPIC A labels: `visibility` is
    /// observed, correlated, inferred or unavailable; a number also says its `quality`.
    pub fn evidence(&self) -> Value {
        let ms = |a: Instant, b: Instant| b.saturating_duration_since(a).as_millis() as u64;
        let unavailable = |why: &str| json!({"visibility": "unavailable", "why": why});
        let mut ev = json!({
            "dispatched": {"visibility": "observed", "source": "aibus"},
            "received": match self.delivered {
                Some(_) => json!({"visibility": "observed", "source": "next_step answered"}),
                None => unavailable("the session never pulled it"),
            },
            "started": match self.started {
                Some(at) => json!({"visibility": "observed", "source": "step_ack", "after_ms": ms(self.queued, at)}),
                None => unavailable("the session sent no step_ack"),
            },
            "tool_calls": if self.call_count == 0 {
                unavailable("the session reported none with step_tool_call")
            } else {
                json!({"visibility": "correlated", "source": "step_tool_call by step token, reported by the session", "count": self.call_count, "names": self.calls})
            },
            "summary": {"visibility": "inferred", "source": "agent reported"},
            "tokens": {"visibility": "unavailable", "quality": "unknown"},
            "cost": {"visibility": "unavailable", "quality": "unknown"},
        });
        if self.interrupt != Interrupt::None {
            ev["interrupt"] = json!({"visibility": "observed", "source": "aibus", "shown_to_session": self.interrupt == Interrupt::Seen});
        }
        if let Some(end) = self.ended {
            ev["duration_ms"] = json!({"visibility": "observed", "quality": "derived", "value": ms(self.queued, end)});
        }
        ev
    }

    fn row(&self) -> Value {
        json!({"token": self.token, "name": self.name, "state": self.state.as_str()})
    }
}

/// What a panel shows: plain text, already cut and cleaned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presented {
    pub client: String,
    pub title: String,
    pub message: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug)]
pub enum Shown {
    Now(Presented),
    /// The first panel of a client waits for the keyboard.
    Pending {
        code: String,
    },
}

#[derive(Debug)]
pub enum Accepted {
    Attached { id: String, client: String },
    Present(Presented),
}

enum Wants {
    Attach {
        label: String,
        pid: Option<u32>,
        cwd: String,
    },
    Present(Presented),
}

struct Pending {
    code: String,
    client: String,
    since: Instant,
    wants: Wants,
}

struct Attachment {
    id: String,
    client: String,
}

/// What a workflow run asks the session's loop, which owns the registry (#353).
pub enum Dispatch {
    /// Is a session attached and free? Asked before a workflow with an attached step starts.
    Check(tokio::sync::oneshot::Sender<Result<(), String>>),
    /// One step for the attached session. The reply comes when the step ends, whichever way.
    Step {
        name: String,
        prompt: String,
        within: Duration,
        reply: tokio::sync::oneshot::Sender<Result<StepEnd, String>>,
    },
}

/// How a step the workflow handed to the attached session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepEnd {
    pub token: String,
    pub state: StepState,
    pub summary: String,
}

pub struct Attachments {
    attach_on: bool,
    present_on: bool,
    lineage: String,
    pending: Option<Pending>,
    attachment: Option<Attachment>,
    /// The client whose first panel the keyboard allowed.
    presenter: Option<String>,
    steps: Vec<Step>,
    /// Workflow steps waiting for their end, by step token.
    waiting: Vec<(
        String,
        tokio::sync::oneshot::Sender<Result<StepEnd, String>>,
    )>,
}

/// Printable text only: no control character (an escape sequence cannot be drawn), no bidi or
/// zero-width mark, cut to `max` characters. `lines` keeps newlines.
pub fn clean(text: &str, max: usize, lines: bool) -> String {
    text.chars()
        .filter(|c| {
            (*c == '\n' && lines)
                || !(c.is_control()
                    || matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'))
        })
        .take(max)
        .collect()
}

fn code() -> String {
    Ulid::new().to_string().chars().skip(22).take(4).collect()
}

impl Attachments {
    /// `attach` and `present` are `--control-attach` and `--control-present`.
    pub fn new(attach: bool, present: bool) -> Self {
        Self {
            attach_on: attach,
            present_on: present,
            lineage: format!("lin-{}", Ulid::new()),
            pending: None,
            attachment: None,
            presenter: None,
            steps: Vec::new(),
            waiting: Vec::new(),
        }
    }

    /// A workflow run asks for a check or hands over a step. The returned line is for the log.
    pub fn handle(&mut self, ask: Dispatch, now: Instant) -> Option<String> {
        match ask {
            Dispatch::Check(reply) => {
                self.expire(now);
                let free = match (&self.attachment, self.steps.iter().find(|s| s.state.open())) {
                    _ if !self.attach_on => {
                        Err("this session was not started with --control-attach".to_owned())
                    }
                    (None, _) => Err(
                        "no attached session: a session attaches with aibus_attach and the keyboard accepts it"
                            .to_owned(),
                    ),
                    (_, Some(s)) => {
                        Err(format!("busy: step {} is in flight on the attached session", s.token))
                    }
                    _ => Ok(()),
                };
                let _ = reply.send(free);
                None
            }
            Dispatch::Step { reply, .. } if reply.is_closed() => {
                // The workflow was cancelled while the step waited in the channel.
                None
            }
            Dispatch::Step {
                name,
                prompt,
                within,
                reply,
            } => match self.dispatch(&name, &prompt, within, now) {
                Ok(token) => {
                    self.waiting.push((token.clone(), reply));
                    Some(format!(
                        "workflow step {token} ({}) queued for the attached session",
                        clean(&name, 40, false)
                    ))
                }
                Err(why) => {
                    let _ = reply.send(Err(why));
                    None
                }
            },
        }
    }

    /// Ends what ran out of time, then answers every workflow step that has ended. A step whose
    /// workflow stopped waiting (it was cancelled) is cancelled here, and queues an interrupt
    /// for it. The lines are for the log.
    pub fn settle(&mut self, now: Instant) -> Vec<String> {
        let mut notes = self.expire(now);
        let mut i = 0;
        while i < self.waiting.len() {
            let Some(step) = self.steps.iter_mut().find(|s| s.token == self.waiting[i].0) else {
                self.waiting.remove(i);
                continue;
            };
            if step.state.open() && self.waiting[i].1.is_closed() {
                let said = Self::interrupt_step(step, now);
                notes.push(format!("{said} (its workflow was cancelled)"));
            }
            if step.state.open() {
                i += 1;
                continue;
            }
            let end = StepEnd {
                token: step.token.clone(),
                state: step.state,
                summary: step.summary.clone(),
            };
            let _ = self.waiting.remove(i).1.send(Ok(end));
        }
        notes
    }

    /// This session's token in the chain of sessions that drive each other.
    pub fn lineage(&self) -> &str {
        &self.lineage
    }

    /// `(attachment id, client)` while a session is attached.
    pub fn attached(&self) -> Option<(&str, &str)> {
        self.attachment
            .as_ref()
            .map(|a| (a.id.as_str(), a.client.as_str()))
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// A client asks to attach. The answer carries the code the user has to type.
    pub fn request_attach(
        &mut self,
        client: &str,
        label: Option<&str>,
        lineage: &[String],
        who: (Option<u32>, Option<&str>),
        now: Instant,
    ) -> Result<Value, String> {
        if !self.attach_on {
            return Err(
                "attaching is off: this session was not started with --control-attach".into(),
            );
        }
        if lineage.contains(&self.lineage) {
            return Err("would loop: that session is driven by this one".into());
        }
        if lineage.len() >= MAX_LINEAGE {
            return Err(format!("would loop: a session {} levels deep cannot attach (the depth is bounded at {MAX_LINEAGE})", lineage.len()));
        }
        self.expire(now);
        if let Some(a) = &self.attachment {
            return if a.client == client {
                Ok(json!({"status": "attached", "attachment": a.id}))
            } else {
                Err(format!(
                    "already attached: {} holds this session, one at a time",
                    a.client
                ))
            };
        }
        if let Some(p) = &self.pending {
            if p.client != client {
                return Err(format!("busy: {} is waiting for the keyboard", p.client));
            }
            if matches!(p.wants, Wants::Attach { .. }) {
                return Ok(json!({"status": "pending", "code": p.code}));
            }
        }
        let code = code();
        self.pending = Some(Pending {
            code: code.clone(),
            client: client.to_owned(),
            since: now,
            wants: Wants::Attach {
                label: clean(label.unwrap_or(""), 40, false),
                pid: who.0,
                cwd: clean(who.1.unwrap_or(""), 120, false),
            },
        });
        Ok(json!({"status": "pending", "code": code, "expires_in": ACCEPT_WITHIN.as_secs()}))
    }

    /// The line the screen shows for the request that waits for the keyboard, if one does.
    pub fn banner(&self) -> Option<String> {
        let p = self.pending.as_ref()?;
        Some(match &p.wants {
            // The code comes first: a long directory would push it off a log row otherwise.
            Wants::Attach { label, pid, cwd } => {
                let mut who = Vec::new();
                if !label.is_empty() {
                    who.push(format!("as {label}"));
                }
                if let Some(pid) = pid {
                    who.push(format!("pid {pid}"));
                    who.push(cwd.clone());
                }
                let who = if who.is_empty() { String::new() } else { format!(" ({})", who.join(", ")) };
                format!(
                    "{} wants to attach, code {}: type /attach accept {} to allow, /attach deny to refuse{who}",
                    p.client, p.code, p.code
                )
            }
            Wants::Present(_) => format!(
                "{} wants to show a panel, code {}: type /attach accept {} to allow it and later ones, /attach deny to refuse",
                p.client, p.code, p.code
            ),
        })
    }

    /// The keyboard accepts what waits, with its code.
    pub fn accept(&mut self, code: &str, now: Instant) -> Result<Accepted, String> {
        self.expire(now);
        let Some(p) = self
            .pending
            .take_if(|p| p.code.eq_ignore_ascii_case(code.trim()))
        else {
            return Err(if self.pending.is_some() {
                "that is not the code on screen".into()
            } else {
                "nothing is waiting to be accepted".into()
            });
        };
        Ok(match p.wants {
            Wants::Attach { .. } => {
                let id = format!("att-{}", Ulid::new());
                self.attachment = Some(Attachment {
                    id: id.clone(),
                    client: p.client.clone(),
                });
                Accepted::Attached {
                    id,
                    client: p.client,
                }
            }
            Wants::Present(panel) => {
                self.presenter = Some(p.client);
                Accepted::Present(panel)
            }
        })
    }

    /// The keyboard refuses what waits. False when nothing did.
    pub fn deny(&mut self) -> bool {
        self.pending.take().is_some()
    }

    /// Queues one step for the attached session.
    pub fn dispatch(
        &mut self,
        name: &str,
        prompt: &str,
        within: Duration,
        now: Instant,
    ) -> Result<String, String> {
        self.expire(now);
        if self.attachment.is_none() {
            return Err("no attached session: a session attaches with aibus_attach and the keyboard accepts it".into());
        }
        if let Some(s) = self.steps.iter().find(|s| s.state.open()) {
            return Err(format!(
                "busy: step {} is in flight on the attached session",
                s.token
            ));
        }
        let token = format!("st-{}", Ulid::new());
        self.steps.push(Step {
            token: token.clone(),
            name: clean(name, 40, false),
            prompt: prompt.to_owned(),
            state: StepState::Queued,
            summary: String::new(),
            queued: now,
            delivered: None,
            started: None,
            calls: Vec::new(),
            call_count: 0,
            interrupt: Interrupt::None,
            ended: None,
            deadline: now + within,
        });
        Ok(token)
    }

    fn holds(&self, client: &str) -> Result<(), String> {
        match &self.attachment {
            Some(a) if a.client == client => Ok(()),
            Some(a) => Err(format!(
                "refused: {} holds this session, not {client}",
                a.client
            )),
            None => Err("not attached: call aibus_attach and wait for the user to accept".into()),
        }
    }

    /// The attached session pulls the next step, if there is one.
    pub fn next_step(&mut self, client: &str, now: Instant) -> Result<Value, String> {
        self.holds(client)?;
        self.expire(now);
        let lineage = self.lineage.clone();
        if let Some(s) = self
            .steps
            .iter_mut()
            .find(|s| s.interrupt == Interrupt::Pending)
        {
            s.interrupt = Interrupt::Seen;
            return Ok(
                json!({"step": null, "interrupt": {"token": s.token, "why": "cancelled at the aibus keyboard or with its workflow"}}),
            );
        }
        let Some(s) = self.steps.iter_mut().find(|s| s.state == StepState::Queued) else {
            return Ok(json!({"step": null}));
        };
        s.state = StepState::Delivered;
        s.delivered = Some(now);
        let secs = s.deadline.saturating_duration_since(now).as_secs();
        Ok(json!({"step": {
            "token": s.token, "name": s.name, "prompt": s.prompt, "deadline_secs": secs, "lineage": [lineage],
        }}))
    }

    /// The attached session answers. `status` is `done`, `declined` or `failed`.
    pub fn step_result(
        &mut self,
        client: &str,
        token: &str,
        status: &str,
        summary: &str,
        now: Instant,
    ) -> Result<Value, String> {
        self.holds(client)?;
        self.expire(now);
        let state = match status {
            "done" => StepState::Done,
            "declined" => StepState::Declined,
            "failed" => StepState::Failed,
            other => {
                return Err(format!(
                    "status {:?}: want done, declined or failed",
                    clean(other, 20, false)
                ))
            }
        };
        let Some(s) = self.steps.iter_mut().find(|s| s.token == token) else {
            return Err(format!("no step {}", clean(token, 40, false)));
        };
        if !s.state.open() {
            let why = match s.state {
                StepState::Done | StepState::Declined | StepState::Failed => {
                    "already answered".to_owned()
                }
                other => format!("it is {}", other.as_str()),
            };
            return Err(format!("refused: step {} {why}", s.token));
        }
        if s.state == StepState::Queued {
            return Err(format!(
                "refused: step {} was never pulled with next_step",
                s.token
            ));
        }
        s.state = state;
        s.summary = clean(summary, SUMMARY_MAX, true);
        s.ended = Some(now);
        Ok(json!({"token": s.token, "state": s.state.as_str(), "evidence": s.evidence()}))
    }

    /// The keyboard cancels the step in flight. A step the session already pulled is interrupted
    /// on the pull leg: the session is told on its next `step_ack`, `step_tool_call` or
    /// `next_step`, because nothing here can push to it. A late result is refused either way.
    pub fn cancel(&mut self, now: Instant) -> Result<String, String> {
        let Some(s) = self.steps.iter_mut().find(|s| s.state.open()) else {
            return Err("no step is in flight".into());
        };
        Ok(Self::interrupt_step(s, now))
    }

    fn interrupt_step(s: &mut Step, now: Instant) -> String {
        let pulled = s.state == StepState::Delivered;
        s.state = StepState::Cancelled;
        s.ended = Some(now);
        if pulled {
            s.interrupt = Interrupt::Pending;
            format!("step {} cancelled; interrupt queued, the session sees it on its next step_ack, step_tool_call or next_step (it cannot be pushed), and a late result is refused", s.token)
        } else {
            format!(
                "step {} cancelled before the session pulled it; a late result is refused",
                s.token
            )
        }
    }

    /// The attached session says it has begun the step it pulled. Once only. A step that was
    /// cancelled answers with the interrupt (once) instead of an error.
    pub fn step_ack(&mut self, client: &str, token: &str, now: Instant) -> Result<Value, String> {
        self.holds(client)?;
        self.expire(now);
        let s = self.step_for_report(token)?;
        if let Some(seen) = Self::seen_interrupt(s) {
            return Ok(seen);
        }
        if s.state != StepState::Delivered {
            return Err(format!(
                "refused: step {} is {}, only a step that was pulled and is open takes an ack",
                s.token,
                s.state.as_str()
            ));
        }
        if s.started.is_some() {
            return Err(format!(
                "refused: step {} was already acknowledged",
                s.token
            ));
        }
        s.started = Some(now);
        Ok(json!({"token": s.token, "state": "started"}))
    }

    /// The attached session reports one tool call it made for the step it holds, by name only (no
    /// arguments, no output). The name is cleaned and capped, the count is capped, and what comes
    /// back is labelled `correlated`: aibus matched it to the step by token, it did not see the call.
    pub fn tool_call(
        &mut self,
        client: &str,
        token: &str,
        name: &str,
        now: Instant,
    ) -> Result<Value, String> {
        /// Names a step keeps (the count goes on), a name's length, and the most calls counted.
        const KEPT: usize = 100;
        const NAME_MAX: usize = 40;
        const COUNT_MAX: u32 = 10_000;
        self.holds(client)?;
        self.expire(now);
        let s = self.step_for_report(token)?;
        if let Some(seen) = Self::seen_interrupt(s) {
            return Ok(seen);
        }
        if s.state != StepState::Delivered {
            return Err(format!("refused: step {} is {}", s.token, s.state.as_str()));
        }
        if s.call_count >= COUNT_MAX {
            return Err(format!(
                "refused: step {} already holds {COUNT_MAX} reported calls",
                s.token
            ));
        }
        s.call_count += 1;
        if s.calls.len() < KEPT {
            s.calls.push(clean(name, NAME_MAX, false));
        }
        Ok(json!({"token": s.token, "calls": s.call_count}))
    }

    fn step_for_report(&mut self, token: &str) -> Result<&mut Step, String> {
        self.steps
            .iter_mut()
            .find(|s| s.token == token)
            .ok_or_else(|| format!("no step {}", clean(token, 40, false)))
    }

    /// The interrupt, shown once, to a session that reports on a step that was cancelled.
    fn seen_interrupt(s: &mut Step) -> Option<Value> {
        match s.interrupt {
            Interrupt::Pending => {
                s.interrupt = Interrupt::Seen;
                Some(
                    json!({"token": s.token, "interrupt": true, "why": "cancelled at the aibus keyboard or with its workflow; stop work on this step"}),
                )
            }
            _ => None,
        }
    }

    /// Ends what ran out of time. The notes are rows for the log.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut notes = Vec::new();
        if self
            .pending
            .as_ref()
            .is_some_and(|p| now.saturating_duration_since(p.since) > ACCEPT_WITHIN)
        {
            if let Some(p) = self.pending.take() {
                notes.push(format!(
                    "{} request timed out waiting for the keyboard; nothing changed",
                    p.client
                ));
            }
        }
        for s in self
            .steps
            .iter_mut()
            .filter(|s| s.state.open() && now > s.deadline)
        {
            s.state = StepState::TimedOut;
            s.ended = Some(now);
            notes.push(format!("step {} timed_out", s.token));
        }
        notes
    }

    /// The client is gone: the attachment ends and its open steps are unavailable.
    pub fn detach(&mut self, client: &str) -> Vec<String> {
        let mut notes = Vec::new();
        if self.pending.as_ref().is_some_and(|p| p.client == client) {
            self.pending = None;
        }
        if self.presenter.as_deref() == Some(client) {
            self.presenter = None;
        }
        if self.attachment.as_ref().is_some_and(|a| a.client == client) {
            self.attachment = None;
            // An interrupt the client never saw dies with it: the next client must not get it.
            for s in self
                .steps
                .iter_mut()
                .filter(|s| s.interrupt == Interrupt::Pending)
            {
                s.interrupt = Interrupt::Seen;
            }
            for s in self.steps.iter_mut().filter(|s| s.state.open()) {
                s.state = StepState::Unavailable;
                notes.push(format!(
                    "step {} unavailable: the attached session went away",
                    s.token
                ));
            }
            notes.push(format!("{client} detached"));
        }
        notes
    }

    /// The step this client holds, if any: while it does, the client may not start more work.
    pub fn in_flight(&self, client: &str) -> Option<&Step> {
        self.attachment.as_ref().filter(|a| a.client == client)?;
        self.steps.iter().find(|s| s.state.open())
    }

    /// A client asks for a panel. The first one of a client waits for the keyboard.
    pub fn present(
        &mut self,
        client: &str,
        title: &str,
        message: &str,
        columns: &[String],
        rows: &[Vec<String>],
        now: Instant,
    ) -> Result<Shown, String> {
        if !self.present_on {
            return Err(
                "panels are off: this session was not started with --control-present".into(),
            );
        }
        self.expire(now);
        let panel = Presented {
            client: client.to_owned(),
            title: clean(title, TITLE_MAX, false),
            message: clean(message, MESSAGE_MAX, true),
            columns: columns
                .iter()
                .take(COLS_MAX)
                .map(|c| clean(c, CELL_MAX, false))
                .collect(),
            rows: rows
                .iter()
                .take(ROWS_MAX)
                .map(|r| {
                    r.iter()
                        .take(COLS_MAX)
                        .map(|c| clean(c, CELL_MAX, false))
                        .collect()
                })
                .collect(),
        };
        if self.presenter.as_deref() == Some(client) {
            return Ok(Shown::Now(panel));
        }
        if let Some(p) = &self.pending {
            return Err(format!("busy: {} is waiting for the keyboard", p.client));
        }
        let code = code();
        self.pending = Some(Pending {
            code: code.clone(),
            client: client.to_owned(),
            since: now,
            wants: Wants::Present(panel),
        });
        Ok(Shown::Pending { code })
    }

    /// What `/attach status` and the status line say.
    pub fn status(&self) -> Value {
        json!({
            "lineage": self.lineage,
            "attached": self.attached().map(|(id, client)| json!({"id": id, "client": client})),
            "waiting": self.pending.as_ref().map(|p| p.client.as_str()),
            "steps": self.steps.iter().rev().take(5).map(Step::row).collect::<Vec<_>>(),
        })
    }
}
