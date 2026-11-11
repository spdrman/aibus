//! The Chat Completions and Responses event streams, turned into `StreamChunk`s and the final
//! message. Both dialects feed one `Turn`.

use std::time::SystemTime;

use aibus_provider::{
    ChatResponse, ContentBlock, Message, MessageRole, Price, ProviderError, ProviderErrorKind,
    CostSource, StopReason, StreamChunk, ToolCallId, Usage,
};
use serde_json::Value;
use smol_str::SmolStr;
use ulid::Ulid;

struct Tool {
    /// The chat tool index or the responses item id.
    key: String,
    id: ToolCallId,
    name: SmolStr,
    json: String,
    ended: bool,
}

pub struct Turn {
    price: Option<Price>,
    /// Read `usage.cost`; when the response has it, it replaces the table's estimate.
    reported_cost: bool,
    text: String,
    thinking: String,
    tools: Vec<Tool>,
    usage: Usage,
    stop: Option<StopReason>,
    /// The server said it was finished: `[DONE]`, a finish reason or `response.completed`.
    complete: bool,
    pub response_id: Option<String>,
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unknown, format!("malformed SSE: {what}"))
}

/// The status the same failure would have had as an HTTP answer, so one table maps both.
fn status_of(code: &str) -> u16 {
    match code {
        "rate_limit_exceeded" | "rate_limit_error" | "insufficient_quota" => 429,
        "server_error" | "internal_error" => 500,
        "engine_overloaded_error" | "overloaded_error" | "service_unavailable" => 503,
        "context_length_exceeded" | "invalid_request_error" | "invalid_prompt" => 400,
        "invalid_api_key" | "authentication_error" => 401,
        "model_not_found" => 404,
        _ => 0,
    }
}

fn stream_error(e: &Value) -> ProviderError {
    let code = e["code"].as_str().or_else(|| e["type"].as_str()).unwrap_or_default();
    let status = match status_of(code) {
        0 => status_of(e["type"].as_str().unwrap_or_default()),
        s => s,
    };
    ProviderError::from_http(status, e["message"].as_str().unwrap_or(code), None, None, code)
}

impl Turn {
    pub fn new(price: Option<Price>, reported_cost: bool) -> Self {
        Self {
            price,
            reported_cost,
            text: String::new(),
            thinking: String::new(),
            tools: Vec::new(),
            usage: Usage::default(),
            stop: None,
            complete: false,
            response_id: None,
        }
    }

    fn take_usage(&mut self, u: &Value) {
        if !u.is_object() {
            return;
        }
        let n = |v: &Value| v.as_u64();
        let prompt = n(&u["prompt_tokens"]).or(n(&u["input_tokens"])).unwrap_or(0);
        let cached = n(&u["prompt_tokens_details"]["cached_tokens"])
            .or(n(&u["input_tokens_details"]["cached_tokens"]))
            .or(n(&u["cached_tokens"]))
            .unwrap_or(0);
        self.usage.input_tokens = prompt.saturating_sub(cached);
        self.usage.cache_read = cached;
        self.usage.output_tokens = n(&u["completion_tokens"]).or(n(&u["output_tokens"])).unwrap_or(0);
        // Chat says `completion_tokens_details`, Responses says `output_tokens_details`.
        self.usage.reasoning_tokens = n(&u["completion_tokens_details"]["reasoning_tokens"])
            .or(n(&u["output_tokens_details"]["reasoning_tokens"]));
        if let (true, Some(c)) = (self.reported_cost, u["cost"].as_f64().filter(|c| c.is_finite() && *c >= 0.0)) {
            self.usage.cost_usd = c;
            self.usage.cost_source = CostSource::Provider;
        }
    }

    fn start_tool(&mut self, key: String, name: &str, out: &mut Vec<StreamChunk>) -> ToolCallId {
        let id = ToolCallId(Ulid::new());
        let name: SmolStr = name.into();
        out.push(StreamChunk::ToolUseStart { id, name: name.clone() });
        self.tools.push(Tool { key, id, name, json: String::new(), ended: false });
        id
    }

    fn tool_mut(&mut self, key: &str) -> Option<&mut Tool> {
        self.tools.iter_mut().find(|t| t.key == key)
    }

    fn arguments(&mut self, key: &str, fragment: &str, out: &mut Vec<StreamChunk>) {
        if let (Some(t), false) = (self.tool_mut(key), fragment.is_empty()) {
            t.json.push_str(fragment);
            out.push(StreamChunk::ToolUseInputDelta { id: t.id, json_fragment: fragment.to_owned() });
        }
    }

    /// One Chat Completions chunk (`data:` of an event), or `[DONE]`.
    pub fn chat_event(&mut self, data: &str) -> Result<Vec<StreamChunk>, ProviderError> {
        if data.trim() == "[DONE]" {
            self.complete = true;
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| malformed(&e.to_string()))?;
        if v["error"].is_object() {
            return Err(stream_error(&v["error"]));
        }
        if let Some(id) = v["id"].as_str() {
            self.response_id.get_or_insert_with(|| id.to_owned());
        }
        let mut out = Vec::new();
        // The usage chunk has no choices; Moonshot also puts it inside the choice.
        self.take_usage(&v["usage"]);
        let choice = &v["choices"][0];
        self.take_usage(&choice["usage"]);
        let delta = &choice["delta"];
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            self.text.push_str(t);
            out.push(StreamChunk::TextDelta(t.to_owned()));
        }
        if let Some(t) = delta["reasoning_content"]
            .as_str()
            .or_else(|| delta["reasoning"].as_str())
            .filter(|t| !t.is_empty())
        {
            self.thinking.push_str(t);
            out.push(StreamChunk::ThinkingDelta(t.to_owned()));
        }
        for (pos, call) in delta["tool_calls"].as_array().into_iter().flatten().enumerate() {
            // Some local servers leave the index out; the position then stands in for it.
            let key = call["index"].as_u64().unwrap_or(pos as u64).to_string();
            let f = &call["function"];
            if self.tool_mut(&key).is_none() {
                self.start_tool(key.clone(), f["name"].as_str().unwrap_or_default(), &mut out);
            }
            self.arguments(&key, f["arguments"].as_str().unwrap_or_default(), &mut out);
        }
        for lp in choice["logprobs"]["content"].as_array().into_iter().flatten() {
            if let (Some(token), Some(logprob)) = (lp["token"].as_str(), lp["logprob"].as_f64()) {
                out.push(StreamChunk::Logprob { token: token.to_owned(), logprob: logprob as f32 });
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.complete = true;
            self.stop = Some(match reason {
                "stop" => StopReason::EndTurn,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::ContentFilter,
                other => StopReason::Other(other.into()),
            });
        }
        Ok(out)
    }

    /// One Responses event (`data:` of an event; the JSON carries its own `type`).
    pub fn responses_event(&mut self, data: &str) -> Result<Vec<StreamChunk>, ProviderError> {
        let v: Value = serde_json::from_str(data).map_err(|e| malformed(&e.to_string()))?;
        let mut out = Vec::new();
        let item = &v["item"];
        let key = v["item_id"].as_str().unwrap_or_default().to_owned();
        match v["type"].as_str().ok_or_else(|| malformed("event without a type"))? {
            "response.created" => self.response_id = v["response"]["id"].as_str().map(str::to_owned),
            "response.output_item.added" if item["type"] == "function_call" => {
                let key = item["id"].as_str().unwrap_or_default().to_owned();
                self.start_tool(key, item["name"].as_str().unwrap_or_default(), &mut out);
            }
            "response.output_text.delta" => {
                if let Some(t) = v["delta"].as_str().filter(|t| !t.is_empty()) {
                    self.text.push_str(t);
                    out.push(StreamChunk::TextDelta(t.to_owned()));
                }
                for lp in v["logprobs"].as_array().into_iter().flatten() {
                    if let (Some(token), Some(logprob)) = (lp["token"].as_str(), lp["logprob"].as_f64()) {
                        out.push(StreamChunk::Logprob { token: token.to_owned(), logprob: logprob as f32 });
                    }
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(t) = v["delta"].as_str().filter(|t| !t.is_empty()) {
                    self.thinking.push_str(t);
                    out.push(StreamChunk::ThinkingDelta(t.to_owned()));
                }
            }
            "response.function_call_arguments.delta" => {
                self.arguments(&key, v["delta"].as_str().unwrap_or_default(), &mut out);
            }
            "response.output_item.done" if item["type"] == "function_call" => {
                let key = item["id"].as_str().unwrap_or_default().to_owned();
                // The finished item carries the whole arguments, for a server that sent no deltas.
                if self.tool_mut(&key).is_some_and(|t| t.json.is_empty()) {
                    self.arguments(&key, item["arguments"].as_str().unwrap_or_default(), &mut out);
                }
                if let Some(t) = self.tool_mut(&key).filter(|t| !t.ended) {
                    t.ended = true;
                    out.push(StreamChunk::ToolUseEnd { id: t.id });
                }
            }
            "response.completed" | "response.incomplete" => {
                self.complete = true;
                self.take_usage(&v["response"]["usage"]);
                self.stop = Some(match v["response"]["incomplete_details"]["reason"].as_str() {
                    Some("max_output_tokens") => StopReason::MaxTokens,
                    Some("content_filter") => StopReason::ContentFilter,
                    Some(other) => StopReason::Other(other.into()),
                    None if self.tools.is_empty() => StopReason::EndTurn,
                    None => StopReason::ToolUse,
                });
            }
            "response.failed" => return Err(stream_error(&v["response"]["error"])),
            "error" => return Err(stream_error(&v)),
            _ => {}
        }
        Ok(out)
    }

    /// The stream is over: ends the tool calls still open and reports usage and the stop
    /// reason. A stream the server never marked complete was cut off, which is worth a retry.
    pub fn close(&mut self) -> Result<Vec<StreamChunk>, ProviderError> {
        if !self.complete {
            return Err(ProviderError::new(
                ProviderErrorKind::Network,
                "the stream ended before the reply was complete",
            ));
        }
        let mut out = Vec::new();
        for t in self.tools.iter_mut().filter(|t| !t.ended) {
            t.ended = true;
            out.push(StreamChunk::ToolUseEnd { id: t.id });
        }
        let stop = self.stop.clone().unwrap_or(StopReason::EndTurn);
        // Some servers say "stop" for a reply that called tools.
        let stop = if stop == StopReason::EndTurn && !self.tools.is_empty() { StopReason::ToolUse } else { stop };
        self.stop = Some(stop.clone());
        if self.usage.cost_source != CostSource::Provider {
            self.usage.cost_usd = self.price.and_then(|p| p.cost_usd(&self.usage)).unwrap_or(0.0);
        }
        out.push(StreamChunk::Usage(self.usage));
        out.push(StreamChunk::Done(stop));
        Ok(out)
    }

    /// The message, once `close` has run.
    pub fn into_response(self, request_id: Option<String>) -> Result<ChatResponse, ProviderError> {
        let mut blocks = Vec::new();
        if !self.thinking.is_empty() {
            blocks.push(ContentBlock::Thinking { text: self.thinking, redacted: false, signature: None });
        }
        if !self.text.is_empty() {
            blocks.push(ContentBlock::Text(self.text));
        }
        for t in self.tools {
            let input = if t.json.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&t.json).map_err(|e| malformed(&format!("tool input for {}: {e}", t.name)))?
            };
            blocks.push(ContentBlock::ToolUse { id: t.id, name: t.name, input });
        }
        Ok(ChatResponse {
            message: Message { role: MessageRole::Assistant, blocks, at: SystemTime::now() },
            usage: self.usage,
            stop: self.stop.unwrap_or(StopReason::EndTurn),
            request_id: request_id.or(self.response_id),
            rate_limit: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_tokens_come_from_either_dialect_and_stay_none_when_unsaid() {
        let mut t = Turn::new(None, false);
        t.take_usage(&serde_json::json!({
            "prompt_tokens": 10, "completion_tokens": 50,
            "completion_tokens_details": {"reasoning_tokens": 32}
        }));
        assert_eq!((t.usage.output_tokens, t.usage.reasoning_tokens), (50, Some(32)));

        let mut t = Turn::new(None, false);
        t.take_usage(&serde_json::json!({
            "input_tokens": 10, "output_tokens": 40,
            "output_tokens_details": {"reasoning_tokens": 7}
        }));
        assert_eq!(t.usage.reasoning_tokens, Some(7));

        let mut t = Turn::new(None, false);
        t.take_usage(&serde_json::json!({"prompt_tokens": 10, "completion_tokens": 5}));
        assert_eq!(t.usage.reasoning_tokens, None);
    }

    #[test]
    fn a_reported_cost_replaces_the_estimate_and_says_so() {
        let price = Price { input_per_mtok: 1.0, output_per_mtok: 1.0, cache_read_per_mtok: None, cache_write_per_mtok: None };
        let chunk = r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":1000,"cost":0.0123}}"#;
        let usage = |t: &mut Turn| {
            t.chat_event(r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#).unwrap();
            t.chat_event(chunk).unwrap();
            t.chat_event("[DONE]").unwrap();
            let out = t.close().unwrap();
            out.into_iter().find_map(|c| if let StreamChunk::Usage(u) = c { Some(u) } else { None }).unwrap()
        };
        let u = usage(&mut Turn::new(Some(price), true));
        assert_eq!((u.cost_usd, u.cost_source), (0.0123, CostSource::Provider));

        // Without `usage.cost` in the response the table's estimate stands.
        let mut t = Turn::new(Some(price), true);
        t.chat_event(r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#).unwrap();
        t.chat_event(r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":1000}}"#).unwrap();
        t.chat_event("[DONE]").unwrap();
        let u = t.close().unwrap().into_iter().find_map(|c| if let StreamChunk::Usage(u) = c { Some(u) } else { None }).unwrap();
        assert_eq!((u.cost_usd, u.cost_source), (0.002, CostSource::Table));

        // A server that is not a reporting gateway is not trusted for a cost.
        let u = usage(&mut Turn::new(Some(price), false));
        assert_eq!((u.cost_usd, u.cost_source), (0.002, CostSource::Table));
    }

    #[test]
    fn a_mid_stream_quota_error_is_quota_and_the_responses_filter_is_a_stop_reason() {
        let e = stream_error(&serde_json::json!({"code": "insufficient_quota", "message": "spent"}));
        assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Quota, false));

        let mut t = Turn::new(None, false);
        t.responses_event(r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"content_filter"}}}"#)
            .unwrap();
        assert_eq!(t.stop, Some(StopReason::ContentFilter));
    }
}
