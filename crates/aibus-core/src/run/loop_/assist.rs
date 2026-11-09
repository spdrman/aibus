//! The lead's dealings with the advisor and the router (01 section 7), kept out of the main
//! loop. `LeadAssist` holds what the lead run needs between turns; the loop calls it at five
//! points: before a request, after the model's reply, after the tool results, on a failed tool
//! call and when the turn would end.
//!
//! Every run (not just the lead) also keeps the session ledger, which is what the advisor reads.

use std::sync::PoisonError;
use std::time::Duration;

use aibus_provider::{
    CacheHint, ChatRequest, ContentBlock, Message, MessageRole, RequestMetadata, ToolChoice,
    ToolSpec,
};
use serde_json::{json, Map};
use smol_str::SmolStr;

use super::{last_text, Task};
use crate::advisor::Triggers;
use crate::assist::{complete, Assist};
use crate::agent::{PermissionMode, Role};
use crate::msg::{Checkpoint, Decision, HookEvent, Msg, RouterKind, RouterQuestion, Verdict};
use crate::router::{self, RETRY_CHOICES};
use crate::run::Conversation;
use crate::tool::ToolCx;
use crate::tools::effective_tools;

/// What `retry_or_stop` decided for a failed call.
pub(super) enum Route {
    Retry,
    Stop,
    Carry,
}

/// A pre-read file note carries this much of the file.
const PREREAD_BYTES: usize = 16 * 1024;
/// The advisor may look around with its read-only tools this many times before it answers.
const ADVISOR_ROUNDS: usize = 4;
const ADVICE_MAX_CHARS: usize = 4_000;

fn head(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

impl Task<'_> {
    /// The run starts: it gets its place in the ledger.
    pub(super) fn ledger_begin(&self, conv: &Conversation) {
        if let Some(a) = self.deps.assist.as_deref().filter(|a| a.advisor.is_some()) {
            let c = self.cfg;
            a.ledger.begin(c.run, &c.def.name, c.def.role, &c.prompt, conv.messages.len());
        }
    }

    /// Takes in what the conversation gained.
    pub(super) fn sync(&self, conv: &Conversation) {
        if let Some(a) = self.deps.assist.as_deref().filter(|a| a.advisor.is_some()) {
            a.ledger.sync(self.cfg.run, &conv.messages);
        }
    }
}

pub(super) struct LeadAssist<'a> {
    assist: &'a Assist,
    triggers: Option<Triggers>,
    /// System messages that go in before the next request: advice, and what the router said.
    notes: Vec<String>,
    /// Router questions asked in the current turn, and what they cost.
    used: u32,
    spent: Duration,
    /// Messages of the conversation `which_file` has looked at.
    seen: usize,
    /// `which_tool` nudged the lead at its last stop.
    nudged: bool,
}

impl<'a> LeadAssist<'a> {
    /// `None` unless this is the lead and there is an advisor or a router to consult.
    pub(super) fn new(t: &Task<'a>) -> Option<Self> {
        if t.cfg.def.role != Role::Lead {
            return None;
        }
        let deps = t.deps;
        let assist = deps.assist.as_deref()?;
        if assist.advisor.is_none() && assist.router.is_none() {
            return None;
        }
        let triggers = assist
            .advisor
            .as_ref()
            .map(|a| Triggers::new(&a.def.checkpoints, &a.enabled));
        Some(Self {
            assist,
            triggers,
            notes: Vec::new(),
            used: 0,
            spent: Duration::ZERO,
            seen: 0,
            nudged: false,
        })
    }

    /// `[label]`, the prefix of what the router says.
    fn tag(&self) -> String {
        let label = self.assist.router.as_ref().map_or("router", |r| r.label.as_str());
        format!("[{}]", label.to_lowercase())
    }

    /// One router question. `None` when the router is off, the turn's cap is used up, or it
    /// could not answer (the lead then carries on without it).
    async fn consult(&mut self, t: &Task<'_>, q: RouterQuestion) -> Option<crate::msg::RouterAnswer> {
        let router = self.assist.router.as_ref()?;
        if self.used >= router.max_per_turn {
            return None;
        }
        self.used += 1;
        let fork = self.assist.next_fork();
        match router::ask(router, &q, t.cancel).await {
            Ok(answer) => {
                self.spent += answer.latency;
                let _span = tracing::trace_span!(
                    target: "aibus_trace", "router",
                    kind = q.kind.as_str(),
                    latency_ms = answer.latency.as_millis() as u64,
                    turn_ms = self.spent.as_millis() as u64,
                );
                t.send(Msg::RouterAnswered { fork, question: q, answer: answer.clone() }).await;
                Some(answer)
            }
            Err(error) => {
                let context = format!("router {}", q.kind.as_str());
                t.send(Msg::IoError { context, error }).await;
                None
            }
        }
    }

    /// The turn is about to start: `which_file`, then the notes that are waiting go in.
    pub(super) async fn before_request(&mut self, t: &Task<'_>, conv: &mut Conversation) {
        self.used = 0;
        self.spent = Duration::ZERO;
        self.which_file(t, conv).await;
        for note in self.notes.drain(..) {
            conv.messages.push(Message::text(MessageRole::System, &note));
        }
    }

    /// `which_file`: the newest message names a symbol that more than one file has.
    async fn which_file(&mut self, t: &Task<'_>, conv: &Conversation) {
        if self.assist.router.is_none() || conv.messages.len() <= self.seen {
            return;
        }
        self.seen = conv.messages.len();
        let Some(last) = conv.messages.last().filter(|m| m.role == MessageRole::User) else {
            return;
        };
        // The task or the user's words, and what failed.
        let mut text = last
            .blocks
            .iter()
            .rev()
            .find_map(|b| match b {
                ContentBlock::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_default();
        for b in &last.blocks {
            if let ContentBlock::ToolResult { content, is_error: true, .. } = b {
                text.push('\n');
                text.push_str(content);
            }
        }
        let text = head(&text, 4_096).to_owned();
        if text.is_empty() {
            return;
        }
        let cwd = t.cfg.cwd.clone();
        let asked = text.clone();
        let found = tokio::task::spawn_blocking(move || router::which_file_candidates(&cwd, &asked))
            .await
            .ok()
            .flatten();
        let Some((symbol, files)) = found else {
            return;
        };
        let q = RouterQuestion {
            kind: RouterKind::WhichFile,
            context: format!(
                "The request mentions `{symbol}`, which is in more than one file.\nRequest: {}",
                head(&text, 300)
            ),
            choices: files.clone(),
        };
        let Some(a) = self.consult(t, q).await else {
            return;
        };
        let tag = self.tag();
        let note = match a.verdict {
            Verdict::Sharp => {
                let body = router::read_head(&t.cfg.cwd, &a.choice, PREREAD_BYTES);
                let mut note = format!(
                    "{tag} which_file: {} for `{symbol}` (p={:.2}, sharp).",
                    a.choice, a.p
                );
                if let Some(body) = body {
                    note.push_str(&format!("\nI read it for you:\n{body}"));
                }
                note
            }
            Verdict::Split => format!(
                "{tag} which_file: `{symbol}` is in {}; my best guess is {} (p={:.2}, split). \
                 You decide.",
                files.join(", "),
                a.choice,
                a.p
            ),
        };
        self.notes.push(note);
    }

    /// A tool call failed. Sharp `retry once` and `stop + ask` are for the loop to carry out.
    pub(super) async fn retry_or_stop(&mut self, t: &Task<'_>, tool: &str, error: &str) -> Route {
        let q = RouterQuestion {
            kind: RouterKind::RetryOrStop,
            context: format!("The tool `{tool}` failed with:\n{}", head(error, 300)),
            choices: RETRY_CHOICES.map(String::from).to_vec(),
        };
        let Some(a) = self.consult(t, q).await else {
            return Route::Carry;
        };
        let tag = self.tag();
        match (a.verdict, a.choice.as_str()) {
            (Verdict::Sharp, "retry once") => Route::Retry,
            (Verdict::Sharp, "stop + ask") => Route::Stop,
            (Verdict::Sharp, _) => {
                self.notes.push(format!(
                    "{tag} retry_or_stop: {} (p={:.2}, sharp). Try another tool for what `{tool}` was doing.",
                    a.choice, a.p
                ));
                Route::Carry
            }
            (Verdict::Split, _) => {
                self.notes.push(format!(
                    "{tag} retry_or_stop: `{tool}` failed; my best guess is {} (p={:.2}, split). \
                     You decide.",
                    a.choice, a.p
                ));
                Route::Carry
            }
        }
    }

    /// `which_tool`: the lead ended on an intention with no tool call. Sharp pre-fills the
    /// tool choice of the next request, split puts the guess in front of the lead. Either way
    /// the lead gets another turn.
    async fn which_tool(&mut self, t: &Task<'_>, conv: &Conversation, tools: &[ToolSpec]) -> bool {
        if self.nudged || self.assist.router.is_none() || tools.is_empty() {
            return false;
        }
        let said = last_text(conv);
        if !router::ends_with_intention(&said) {
            return false;
        }
        let tail: String = said.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
        let q = RouterQuestion {
            kind: RouterKind::WhichTool,
            context: format!("The lead said, and then called no tool:\n{tail}"),
            choices: tools.iter().map(|s| s.name.to_string()).collect(),
        };
        let Some(a) = self.consult(t, q).await else {
            return false;
        };
        let tag = self.tag();
        match a.verdict {
            Verdict::Sharp => {
                *t.hint.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(SmolStr::new(&a.choice));
                self.notes.push(format!(
                    "{tag} which_tool: call {} next (p={:.2}, sharp).",
                    a.choice, a.p
                ));
            }
            Verdict::Split => self.notes.push(format!(
                "{tag} which_tool: you ended on an intention without a tool call; my best guess \
                 is {} (p={:.2}, split). You decide.",
                a.choice, a.p
            )),
        }
        self.nudged = true;
        true
    }

    /// The model replied: `before_plan`.
    pub(super) async fn after_reply(&mut self, t: &Task<'_>, conv: &Conversation, mode: PermissionMode) {
        let Some(msg) = conv.messages.last() else {
            return;
        };
        if msg.blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })) {
            self.nudged = false;
        }
        let hit = self.triggers.as_mut().and_then(|tr| tr.on_assistant(msg));
        if let Some((cp, detail)) = hit {
            self.checkpoint(t, conv, mode, cp, detail).await;
        }
    }

    /// The results are in: `same_error_x2` and the custom checkpoints. `done` is each call's
    /// tool, content and whether it failed.
    pub(super) async fn after_results(
        &mut self,
        t: &Task<'_>,
        conv: &Conversation,
        mode: PermissionMode,
        done: &[(SmolStr, String, bool)],
    ) {
        for (tool, content, failed) in done {
            let hits = match self.triggers.as_mut() {
                Some(tr) => tr.on_result(tool, content, *failed),
                None => return,
            };
            for (cp, detail) in hits {
                self.checkpoint(t, conv, mode, cp, detail).await;
            }
        }
    }

    /// The turn would end. `true` when the lead should go round once more: the router nudged
    /// it, or an advisor note is waiting for it.
    pub(super) async fn on_idle(
        &mut self,
        t: &Task<'_>,
        conv: &Conversation,
        mode: PermissionMode,
        tools: &[ToolSpec],
    ) -> bool {
        if self.which_tool(t, conv, tools).await {
            return true;
        }
        let workers = self.assist.ledger.completed_workers();
        let hit = self.triggers.as_mut().and_then(|tr| tr.on_idle(workers));
        if let Some((cp, detail)) = hit {
            self.checkpoint(t, conv, mode, cp, detail).await;
        }
        !self.notes.is_empty()
    }

    /// A checkpoint fired. The `checkpoint` hook can skip it (exit 2); otherwise the advisor
    /// reads the session and its advice waits in `notes`.
    async fn checkpoint(
        &mut self,
        t: &Task<'_>,
        conv: &Conversation,
        mode: PermissionMode,
        cp: Checkpoint,
        detail: String,
    ) {
        let Some(adv) = &self.assist.advisor else {
            return;
        };
        let extra = Map::from_iter([
            ("checkpoint".to_owned(), json!(cp.name())),
            ("detail".to_owned(), json!(detail)),
        ]);
        let fired = t.fire(HookEvent::Checkpoint, Some(cp.name()), mode, extra).await;
        if fired.block.is_some() {
            return;
        }
        t.send(Msg::CheckpointHit { checkpoint: cp.clone(), detail: detail.clone() }).await;
        t.sync(conv);
        let packet = self.assist.ledger.packet(adv.packet_max_tokens);
        let text = t.deps.hooks.redact(&packet.text).into_owned();
        let prompt = format!(
            "Checkpoint: {} ({detail}).\n\nThe session so far:\n\n{text}\nGive the lead short, \
             concrete advice. Say nothing if there is nothing worth saying.",
            cp.name()
        );
        let advice = match advise(t, adv, &prompt).await {
            Ok(advice) => advice,
            Err(error) => {
                t.send(Msg::IoError { context: "advisor".into(), error }).await;
                String::new()
            }
        };
        t.send(Msg::AdvisorSpoke {
            checkpoint: cp,
            advice: advice.clone(),
            tokens_read: packet.tokens,
        })
        .await;
        if !advice.is_empty() {
            self.notes.push(format!("[advisor] {advice}"));
        }
    }
}

/// The advisor's turn: one request on its own model and account, with its read-only tools
/// (never `delegate`, `edit`, `write` or `bash`, whatever its definition lists).
async fn advise(
    t: &Task<'_>,
    adv: &crate::assist::AdvisorSeat,
    prompt: &str,
) -> Result<String, String> {
    let eff = effective_tools(&adv.def, Role::Advisor, 0, true, 0);
    let specs = t.deps.tools.specs(&eff);
    let mut messages = vec![Message::text(MessageRole::User, prompt)];
    let cx = ToolCx {
        run: t.cfg.run,
        cwd: t.cfg.cwd.clone(),
        cancel: t.cancel.clone(),
        limits: t.cfg.limits,
        tx: t.deps.tx.clone(),
        memory: None,
    };
    let mut advice = String::new();
    for _ in 0..ADVISOR_ROUNDS {
        let req = ChatRequest {
            model: adv.seat.model.clone(),
            system: adv.def.system_prompt.clone(),
            messages: messages.clone(),
            tools: specs.clone(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 1_024,
            temperature: None,
            effort: adv.def.effort,
            thinking: None,
            cache_hint: CacheHint::AfterSystem,
            logprobs: false,
            metadata: RequestMetadata::default(),
        };
        let (resp, _) = complete(&adv.seat, req, t.cancel).await.map_err(|e| e.message)?;
        advice = resp
            .message
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let calls: Vec<_> = resp
            .message
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => Some((*id, name.clone(), input.clone())),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            break;
        }
        messages.push(resp.message);
        let mut results = Vec::new();
        for (id, name, input) in calls {
            let out = t
                .deps
                .tools
                .call_gated(&eff, &cx, adv.def.permission_mode, &name, input, |_| async {
                    Decision::Deny
                })
                .await;
            let (content, is_error) = match out {
                Ok(o) => (o.content, false),
                Err(e) => (e.to_string(), true),
            };
            results.push(ContentBlock::ToolResult { id, content, is_error });
        }
        messages.push(Message {
            role: MessageRole::User,
            blocks: results,
            at: std::time::SystemTime::UNIX_EPOCH,
        });
    }
    Ok(head(advice.trim(), ADVICE_MAX_CHARS).to_owned())
}
