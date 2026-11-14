//! Compaction (01 section 5.5, Q9): the run's own model summarises its conversation, and the
//! summary stands in for everything before it.
//!
//! The conversation is sent as one block of text rather than as the original messages. That way
//! the request needs no tool definitions (a provider rejects `tool_use` blocks without them), and
//! the part that does not fit the model's window can be left out from the front.

use std::fmt::Write;
use std::sync::Arc;

use aibus_provider::{
    CacheHint, ChatRequest, Provider, RequestMetadata, StreamChunk, ToolChoice,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use std::path::PathBuf;
use std::time::SystemTime;

use super::loop_::{Compactor, STREAM_CAPACITY};
use super::{ContentBlock, Conversation, Message, MessageRole, ModelId, Usage};
use crate::continuity::handoff::{self, Front, Handoff};
use crate::continuity::{ledger, Layout};
use crate::credentials::CredentialSource;
use crate::persist::time::date;
use crate::redact::Redactor;

/// Characters of a tool call's input or a tool's result that go into the text to summarise.
const TOOL_CHARS: usize = 1_000;
/// Characters per token for fitting the text into the window; low on purpose.
const CHARS_PER_TOKEN: usize = 3;

const INSTRUCTION: &str = "Summarise the conversation below so that the work can go on from your summary alone. \
Keep the user's goal and instructions, the decisions made, the files looked at or changed, what was found, \
the errors met, and what is still to do. Write plain text, no preamble.";

/// Summarises with `model` on `provider`, the same ones the run talks to.
pub struct ModelCompactor {
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<dyn CredentialSource>,
    pub model: ModelId,
    /// The lead's compactor writes an auto-handoff before it summarises (01 section 5.5); a
    /// worker's has none.
    pub handoff: Option<AutoHandoff>,
}

/// Where an auto-handoff goes, and the redactor its text passes through, since it quotes the
/// conversation.
pub struct AutoHandoff {
    pub layout: Layout,
    pub redactor: Redactor,
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((end, _)) => format!("{}... [{} more characters]", &s[..end], s[end..].chars().count()),
        None => s.to_owned(),
    }
}

/// One message as text. Thinking is left out, images are named.
fn render(m: &Message) -> String {
    let who = if m.role == MessageRole::User { "user" } else { "assistant" };
    let mut out = format!("{who}:");
    for b in &m.blocks {
        let _ = match b {
            ContentBlock::Text(t) => write!(out, "\n{t}"),
            ContentBlock::Thinking { .. } => Ok(()),
            ContentBlock::ToolUse { name, input, .. } => {
                write!(out, "\n[call {name} {}]", clip(&input.to_string(), TOOL_CHARS))
            }
            ContentBlock::ToolResult {
                content, is_error, ..
            } => write!(
                out,
                "\n[{}{}]",
                if *is_error { "error: " } else { "result: " },
                clip(content, TOOL_CHARS)
            ),
            ContentBlock::Image { .. } => write!(out, "\n[image]"),
        };
    }
    out
}

/// The conversation as text, newest messages first until `budget` characters are used, then in
/// order again. Whatever was left out from the front is said so.
pub(crate) fn flatten(conv: &Conversation, budget: usize) -> String {
    let mut kept = Vec::new();
    let mut used = 0;
    for m in conv.messages.iter().rev() {
        let text = render(m);
        used += text.len();
        if used > budget && !kept.is_empty() {
            break;
        }
        kept.push(text);
    }
    let cut = conv.messages.len() - kept.len();
    kept.reverse();
    let mut out = String::new();
    if cut > 0 {
        let _ = writeln!(out, "[the first {cut} messages are left out]\n");
    }
    out.push_str(&kept.join("\n\n"));
    out
}

impl ModelCompactor {
    /// One request in a fresh context: `instruction` and then `conv` as text, with no tools.
    /// The model's text and what the call used and cost.
    pub(crate) async fn ask(
        &self,
        system: &str,
        instruction: &str,
        conv: &Conversation,
        cancel: &CancellationToken,
    ) -> Result<(String, Usage), String> {
        let info = self.provider.models().into_iter().find(|m| m.id == self.model);
        let window = info.as_ref().map_or(100_000, |m| m.context_window as usize);
        let max_output = info.as_ref().map_or(4_096, |m| m.max_output.min(4_096));
        let budget = (window / 2).saturating_mul(CHARS_PER_TOKEN);
        let body = flatten(conv, budget);
        let req = ChatRequest {
            model: self.model.clone(),
            system: system.into(),
            messages: vec![Message::text(
                MessageRole::User,
                &format!("{instruction}\n\n<conversation>\n{body}\n</conversation>"),
            )],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: max_output,
            temperature: None,
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        };
        let creds = self.creds.credential().map_err(|e| e.to_string())?;
        let (out, mut chunks) = mpsc::channel::<StreamChunk>(STREAM_CAPACITY);
        let call = self.provider.stream(&creds, req, out, cancel.clone());
        tokio::pin!(call);
        let resp = loop {
            tokio::select! {
                r = &mut call => break r.map_err(|e| e.message)?,
                // Nobody needs the chunks, but a full channel would stall the provider.
                _ = chunks.recv() => {}
            }
        };
        let text: String = resp
            .message
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        if text.trim().is_empty() {
            return Err("the model returned an empty answer".into());
        }
        let mut usage = resp.usage;
        if usage.cost_usd <= 0.0 {
            usage.cost_usd = self
                .provider
                .price(&self.model)
                .and_then(|p| p.cost_usd(&usage))
                .unwrap_or(0.0);
        }
        Ok((text, usage))
    }
}

#[async_trait::async_trait]
impl Compactor for ModelCompactor {
    async fn compact(
        &self,
        conv: &mut Conversation,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let system = "You summarise conversations for an engineering assistant.";
        let (summary, u) = self.ask(system, INSTRUCTION, conv, cancel).await?;
        conv.usage.input_tokens += u.input_tokens;
        conv.usage.output_tokens += u.output_tokens;
        conv.usage.cache_read += u.cache_read;
        conv.usage.cache_write += u.cache_write;
        conv.usage.cost_usd += u.cost_usd;
        conv.messages = vec![Message::text(
            MessageRole::User,
            &format!("Summary of the conversation so far, written when the context was compacted:\n\n{}", summary.trim()),
        )];
        conv.compact_boundary = Some(0);
        Ok(())
    }

    /// The ledger's goal and first open item, or the conversation's first and last words when
    /// there is no ledger, as `auto-handoff-<ts>.yaml` (01 section 8.2). Nothing is asked of the
    /// model: this runs right before a compaction, when the context is already full.
    async fn handoff(&self, conv: &Conversation) -> Result<Option<PathBuf>, String> {
        let Some(auto) = &self.handoff else {
            return Ok(None);
        };
        let ledger = ledger::read(&auto.layout).map_err(|e| e.to_string())?;
        let mut body = serde_json::Value::Object(handoff::summarise(ledger.as_ref(), Some(conv)));
        auto.redactor.redact_json(&mut body);
        let now = SystemTime::now();
        let front = Front {
            session: &auto.layout.topic,
            date: &date(now),
            status: "partial",
            outcome: None,
        };
        let h = Handoff::build(&front, body.as_object().ok_or("no handoff body")?)?;
        handoff::write(&auto.layout, &h, true, now)
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(texts: &[&str]) -> Conversation {
        Conversation {
            messages: texts
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let role = if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant };
                    Message::text(role, t)
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn what_does_not_fit_is_cut_from_the_front_and_said_so() {
        let c = conv(&["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc", "dddddddddd"]);
        let text = flatten(&c, 40);
        assert!(text.starts_with("[the first 2 messages are left out]"), "{text}");
        assert!(text.contains("cccccccccc") && text.contains("dddddddddd"));
        assert!(!text.contains("aaaaaaaaaa"));
        let all = flatten(&c, 10_000);
        assert!(!all.contains("left out") && all.starts_with("user:\naaaaaaaaaa"));
    }

    #[test]
    fn a_single_message_over_the_budget_is_still_kept() {
        let c = conv(&["x".repeat(500).as_str()]);
        assert!(flatten(&c, 10).contains("xxxx"));
    }

    #[test]
    fn tool_traffic_is_clipped() {
        let long = "r".repeat(5_000);
        let m = Message {
            role: MessageRole::User,
            blocks: vec![ContentBlock::ToolResult {
                id: aibus_provider::ToolCallId(ulid::Ulid::new()),
                content: long,
                is_error: false,
            }],
            at: std::time::SystemTime::UNIX_EPOCH,
        };
        let text = render(&m);
        assert!(text.len() < 1_200 && text.contains("4000 more characters"), "{}", text.len());
    }
}
