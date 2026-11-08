//! The Messages API event stream, turned into `StreamChunk`s and the final message.

use std::collections::BTreeMap;
use std::time::SystemTime;

use aibus_provider::sse::SseEvent;
use aibus_provider::{
    ChatResponse, ContentBlock, Message, MessageRole, Price, ProviderError, ProviderErrorKind,
    StopReason, StreamChunk, ToolCallId, Usage,
};
use serde_json::Value;
use smol_str::SmolStr;
use ulid::Ulid;

/// A content block that has started and not stopped.
enum Open {
    Text(String),
    Thinking { text: String, signature: Option<String> },
    Redacted(String),
    Tool { id: ToolCallId, name: SmolStr, json: String },
}

pub struct Turn {
    price: Option<Price>,
    open: BTreeMap<u64, Open>,
    blocks: Vec<ContentBlock>,
    usage: Usage,
    stop: Option<StopReason>,
    pub message_id: Option<String>,
    done: bool,
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unknown, format!("malformed SSE: {what}"))
}

/// The status the same failure would have had as an HTTP answer, so one table maps both.
fn status_of(error_type: &str) -> u16 {
    match error_type {
        "invalid_request_error" => 400,
        "authentication_error" => 401,
        "permission_error" => 403,
        "not_found_error" => 404,
        "request_too_large" => 413,
        "rate_limit_error" => 429,
        "overloaded_error" => 529,
        "timeout_error" => 504,
        "api_error" => 500,
        _ => 0,
    }
}

fn stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.into()),
    }
}

impl Turn {
    pub fn new(price: Option<Price>) -> Self {
        Self {
            price,
            open: BTreeMap::new(),
            blocks: Vec::new(),
            usage: Usage::default(),
            stop: None,
            message_id: None,
            done: false,
        }
    }

    /// Token counts: a later event replaces an earlier one, since the API sends running totals.
    fn take_usage(&mut self, u: &Value) {
        for (key, slot) in [
            ("input_tokens", &mut self.usage.input_tokens),
            ("output_tokens", &mut self.usage.output_tokens),
            ("cache_read_input_tokens", &mut self.usage.cache_read),
            ("cache_creation_input_tokens", &mut self.usage.cache_write),
        ] {
            if let Some(n) = u[key].as_u64() {
                *slot = n;
            }
        }
    }

    /// One event in, the chunks it produces out. An unknown event type is skipped, so a new
    /// kind of event does not break an older aibus.
    pub fn event(&mut self, ev: &SseEvent) -> Result<Vec<StreamChunk>, ProviderError> {
        let v: Value = serde_json::from_str(&ev.data).map_err(|e| malformed(&e.to_string()))?;
        let index = v["index"].as_u64().unwrap_or(0);
        let mut out = Vec::new();
        match v["type"].as_str().ok_or_else(|| malformed("event without a type"))? {
            "message_start" => {
                self.message_id = v["message"]["id"].as_str().map(str::to_owned);
                self.take_usage(&v["message"]["usage"]);
            }
            "content_block_start" => {
                let b = &v["content_block"];
                let open = match b["type"].as_str() {
                    Some("text") => {
                        let t = b["text"].as_str().unwrap_or_default();
                        if !t.is_empty() {
                            out.push(StreamChunk::TextDelta(t.to_owned()));
                        }
                        Open::Text(t.to_owned())
                    }
                    Some("thinking") => Open::Thinking { text: String::new(), signature: None },
                    Some("redacted_thinking") => {
                        Open::Redacted(b["data"].as_str().unwrap_or_default().to_owned())
                    }
                    Some("tool_use") => {
                        let name: SmolStr = b["name"].as_str().ok_or_else(|| malformed("tool_use without a name"))?.into();
                        let id = ToolCallId(Ulid::new());
                        out.push(StreamChunk::ToolUseStart { id, name: name.clone() });
                        Open::Tool { id, name, json: String::new() }
                    }
                    // Server-side tools and whatever comes next: not ours to show.
                    _ => return Ok(out),
                };
                self.open.insert(index, open);
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let Some(open) = self.open.get_mut(&index) else {
                    return Ok(out);
                };
                match (d["type"].as_str(), open) {
                    (Some("text_delta"), Open::Text(t)) => {
                        let piece = d["text"].as_str().unwrap_or_default();
                        t.push_str(piece);
                        out.push(StreamChunk::TextDelta(piece.to_owned()));
                    }
                    (Some("thinking_delta"), Open::Thinking { text, .. }) => {
                        let piece = d["thinking"].as_str().unwrap_or_default();
                        text.push_str(piece);
                        out.push(StreamChunk::ThinkingDelta(piece.to_owned()));
                    }
                    (Some("signature_delta"), Open::Thinking { signature, .. }) => {
                        signature.get_or_insert_with(String::new).push_str(d["signature"].as_str().unwrap_or_default());
                    }
                    (Some("input_json_delta"), Open::Tool { id, json, .. }) => {
                        let piece = d["partial_json"].as_str().unwrap_or_default();
                        json.push_str(piece);
                        out.push(StreamChunk::ToolUseInputDelta { id: *id, json_fragment: piece.to_owned() });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => match self.open.remove(&index) {
                Some(Open::Text(t)) => self.blocks.push(ContentBlock::Text(t)),
                Some(Open::Thinking { text, signature }) => {
                    self.blocks.push(ContentBlock::Thinking { text, redacted: false, signature });
                }
                Some(Open::Redacted(data)) => {
                    self.blocks.push(ContentBlock::Thinking { text: data, redacted: true, signature: None });
                }
                Some(Open::Tool { id, name, json }) => {
                    // A tool with no arguments streams no JSON at all.
                    let input = if json.trim().is_empty() {
                        Value::Object(Default::default())
                    } else {
                        serde_json::from_str(&json).map_err(|e| malformed(&format!("tool input: {e}")))?
                    };
                    self.blocks.push(ContentBlock::ToolUse { id, name, input });
                    out.push(StreamChunk::ToolUseEnd { id });
                }
                None => {}
            },
            "message_delta" => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(stop_reason(s));
                }
                self.take_usage(&v["usage"]);
            }
            "message_stop" => {
                self.done = true;
                let stop = self.stop.clone().unwrap_or(StopReason::EndTurn);
                out.push(StreamChunk::Usage(self.usage()));
                out.push(StreamChunk::Done(stop));
            }
            "error" => {
                let e = &v["error"];
                let kind = e["type"].as_str().unwrap_or_default();
                return Err(ProviderError::from_http(
                    status_of(kind),
                    e["message"].as_str().unwrap_or(kind),
                    None,
                    self.message_id.clone(),
                ));
            }
            _ => {}
        }
        Ok(out)
    }

    fn usage(&self) -> Usage {
        let mut u = self.usage;
        u.cost_usd = self.price.and_then(|p| p.cost_usd(&u)).unwrap_or(0.0);
        u
    }

    /// The message once the stream is over. A stream that stops before `message_stop` was cut
    /// off, which is worth a retry.
    pub fn finish(self, request_id: Option<String>) -> Result<ChatResponse, ProviderError> {
        if !self.done {
            return Err(ProviderError::new(
                ProviderErrorKind::Network,
                "the stream ended before the message was complete",
            ));
        }
        Ok(ChatResponse {
            usage: self.usage(),
            stop: self.stop.unwrap_or(StopReason::EndTurn),
            request_id: request_id.or(self.message_id),
            message: Message { role: MessageRole::Assistant, blocks: self.blocks, at: SystemTime::now() },
        })
    }
}
