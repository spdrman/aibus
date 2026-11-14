//! The `streamGenerateContent?alt=sse` event stream, turned into `StreamChunk`s and the final
//! message. Every event is a whole `GenerateContentResponse`; there is no `[DONE]`.

use std::time::SystemTime;

use aibus_provider::{
    ChatResponse, ContentBlock, Message, MessageRole, Price, ProviderError, ProviderErrorKind,
    StopReason, StreamChunk, ToolCallId, Usage,
};
use serde_json::Value;
use smol_str::SmolStr;
use ulid::Ulid;

pub struct Turn {
    price: Option<Price>,
    text: String,
    thinking: String,
    /// Gemini sends a call whole, so a call is complete when it is pushed here.
    tools: Vec<(ToolCallId, SmolStr, Value, Option<String>)>,
    usage: Usage,
    stop: Option<StopReason>,
    pub response_id: Option<String>,
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unknown, format!("malformed SSE: {what}"))
}

/// The HTTP status of Google's `error.status` name, so `from_http` has one table for both.
fn status_of(name: &str) -> u16 {
    match name {
        "INVALID_ARGUMENT" | "FAILED_PRECONDITION" | "OUT_OF_RANGE" => 400,
        "UNAUTHENTICATED" => 401,
        "PERMISSION_DENIED" => 403,
        "NOT_FOUND" => 404,
        "RESOURCE_EXHAUSTED" => 429,
        "INTERNAL" | "UNKNOWN" => 500,
        "UNAVAILABLE" => 503,
        "DEADLINE_EXCEEDED" => 504,
        _ => 0,
    }
}

/// Google's `{ code, status, message }`, as it arrives inside a stream.
fn stream_error(e: &Value) -> ProviderError {
    let status = match e["code"].as_u64() {
        Some(c) => c as u16,
        None => status_of(e["status"].as_str().unwrap_or_default()),
    };
    ProviderError::from_http(
        status,
        e["message"].as_str().unwrap_or_default(),
        None,
        None,
        "",
    )
}

impl Turn {
    pub fn new(price: Option<Price>) -> Self {
        Self {
            price,
            text: String::new(),
            thinking: String::new(),
            tools: Vec::new(),
            usage: Usage::default(),
            stop: None,
            response_id: None,
        }
    }

    /// `usageMetadata` is cumulative, so the last one seen is the count.
    fn take_usage(&mut self, u: &Value) {
        if !u.is_object() {
            return;
        }
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        let (cached, thoughts) = (n("cachedContentTokenCount"), n("thoughtsTokenCount"));
        self.usage.input_tokens = n("promptTokenCount").saturating_sub(cached);
        self.usage.cache_read = cached;
        self.usage.output_tokens = n("candidatesTokenCount") + thoughts;
        self.usage.reasoning_tokens = u["thoughtsTokenCount"].as_u64();
    }

    /// One event's `data:`.
    pub fn event(&mut self, data: &str) -> Result<Vec<StreamChunk>, ProviderError> {
        let v: Value = serde_json::from_str(data).map_err(|e| malformed(&e.to_string()))?;
        if v["error"].is_object() {
            return Err(stream_error(&v["error"]));
        }
        if let Some(id) = v["responseId"].as_str() {
            self.response_id.get_or_insert_with(|| id.to_owned());
        }
        self.take_usage(&v["usageMetadata"]);
        let mut out = Vec::new();
        let candidate = &v["candidates"][0];
        for part in candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(call) = part["functionCall"].as_object() {
                let id = ToolCallId(Ulid::new());
                let name: SmolStr = call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into();
                let args = call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                out.push(StreamChunk::ToolUseStart {
                    id,
                    name: name.clone(),
                });
                out.push(StreamChunk::ToolUseInputDelta {
                    id,
                    json_fragment: args.to_string(),
                });
                out.push(StreamChunk::ToolUseEnd { id });
                let signature = part["thoughtSignature"].as_str().map(str::to_owned);
                self.tools.push((id, name, args, signature));
            } else if let Some(t) = part["text"].as_str().filter(|t| !t.is_empty()) {
                if part["thought"].as_bool() == Some(true) {
                    self.thinking.push_str(t);
                    out.push(StreamChunk::ThinkingDelta(t.to_owned()));
                } else {
                    self.text.push_str(t);
                    out.push(StreamChunk::TextDelta(t.to_owned()));
                }
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            self.stop = Some(match reason {
                "STOP" => StopReason::EndTurn,
                "MAX_TOKENS" => StopReason::MaxTokens,
                "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII"
                | "IMAGE_SAFETY" => StopReason::ContentFilter,
                "MALFORMED_FUNCTION_CALL" => {
                    let raw = candidate["finishMessage"]
                        .as_str()
                        .unwrap_or("no detail given");
                    return Err(ProviderError::new(
                        ProviderErrorKind::InvalidRequest,
                        format!("the model made a malformed function call: {raw}"),
                    ));
                }
                other => StopReason::Other(other.into()),
            });
        } else if v["promptFeedback"]["blockReason"].is_string() && self.stop.is_none() {
            // A prompt the filter refused comes back with no candidate at all.
            self.stop = Some(StopReason::ContentFilter);
        }
        Ok(out)
    }

    /// The stream is over: reports usage and the stop reason. A stream that never said why it
    /// stopped was cut off, which is worth a retry.
    pub fn close(&mut self) -> Result<Vec<StreamChunk>, ProviderError> {
        let Some(stop) = self.stop.clone() else {
            return Err(ProviderError::new(
                ProviderErrorKind::Network,
                "the stream ended before the reply was complete",
            ));
        };
        // Gemini says STOP for a reply that called tools.
        let stop = if stop == StopReason::EndTurn && !self.tools.is_empty() {
            StopReason::ToolUse
        } else {
            stop
        };
        self.stop = Some(stop.clone());
        self.usage.cost_usd = self
            .price
            .and_then(|p| p.cost_usd(&self.usage))
            .unwrap_or(0.0);
        Ok(vec![
            StreamChunk::Usage(self.usage),
            StreamChunk::Done(stop),
        ])
    }

    /// The message, once `close` has run.
    pub fn into_response(self, request_id: Option<String>) -> ChatResponse {
        let mut blocks = Vec::new();
        if !self.thinking.is_empty() {
            blocks.push(ContentBlock::Thinking {
                text: self.thinking,
                redacted: false,
                signature: None,
            });
        }
        if !self.text.is_empty() {
            blocks.push(ContentBlock::Text(self.text));
        }
        for (id, name, input, signature) in self.tools {
            blocks.push(ContentBlock::ToolUse {
                id,
                name,
                input,
                signature,
            });
        }
        ChatResponse {
            message: Message {
                role: MessageRole::Assistant,
                blocks,
                at: SystemTime::now(),
            },
            usage: self.usage,
            stop: self.stop.unwrap_or(StopReason::EndTurn),
            request_id: request_id.or(self.response_id),
            rate_limit: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_subtracts_the_cache_and_counts_thoughts_as_output() {
        let mut t = Turn::new(None);
        t.take_usage(&serde_json::json!({
            "promptTokenCount": 125, "cachedContentTokenCount": 100,
            "candidatesTokenCount": 15, "thoughtsTokenCount": 30, "totalTokenCount": 170
        }));
        assert_eq!((t.usage.input_tokens, t.usage.cache_read), (25, 100));
        assert_eq!(
            (t.usage.output_tokens, t.usage.reasoning_tokens),
            (45, Some(30))
        );

        let mut t = Turn::new(None);
        t.take_usage(&serde_json::json!({"promptTokenCount": 10, "candidatesTokenCount": 5}));
        assert_eq!((t.usage.output_tokens, t.usage.reasoning_tokens), (5, None));
    }

    #[test]
    fn finish_reasons_map_and_a_malformed_call_carries_the_raw_text() {
        let stop = |reason: &str| {
            let mut t = Turn::new(None);
            t.event(&format!(
                r#"{{"candidates":[{{"finishReason":"{reason}"}}]}}"#
            ))
            .unwrap();
            t.stop.unwrap()
        };
        assert_eq!(stop("STOP"), StopReason::EndTurn);
        assert_eq!(stop("MAX_TOKENS"), StopReason::MaxTokens);
        for r in ["SAFETY", "RECITATION", "PROHIBITED_CONTENT"] {
            assert_eq!(stop(r), StopReason::ContentFilter, "{r}");
        }
        assert_eq!(stop("OTHER"), StopReason::Other("OTHER".into()));

        let mut t = Turn::new(None);
        let e = t
            .event(r#"{"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL","finishMessage":"print(read_file(path=1))"}]}"#)
            .unwrap_err();
        assert_eq!(
            (e.kind, e.retryable),
            (ProviderErrorKind::InvalidRequest, false)
        );
        assert!(e.message.contains("print(read_file(path=1))"));
    }

    #[test]
    fn a_blocked_prompt_with_no_candidate_is_a_content_filter_stop() {
        let mut t = Turn::new(None);
        t.event(r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#)
            .unwrap();
        assert_eq!(
            t.close().unwrap().last(),
            Some(&StreamChunk::Done(StopReason::ContentFilter))
        );
    }

    #[test]
    fn a_mid_stream_error_maps_by_its_code_or_its_status_name() {
        let e = stream_error(
            &serde_json::json!({"code": 429, "status": "RESOURCE_EXHAUSTED", "message": "slow"}),
        );
        assert_eq!((e.kind, e.retryable), (ProviderErrorKind::RateLimit, true));
        let e = stream_error(&serde_json::json!({"status": "UNAUTHENTICATED", "message": "no"}));
        assert_eq!(e.kind, ProviderErrorKind::Auth);
        let e = stream_error(
            &serde_json::json!({"code": 404, "status": "NOT_FOUND", "message": "models/x is not found"}),
        );
        assert_eq!(e.kind, ProviderErrorKind::ModelNotFound);
    }
}
