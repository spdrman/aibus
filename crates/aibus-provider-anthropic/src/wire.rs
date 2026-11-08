//! The request body for the Messages API (and `count_tokens`, which takes most of it).
//!
//! Keys serialise in sorted order, so two requests with the same system prompt, tools and
//! first messages produce the same bytes for that prefix. A fork's first request relies on it
//! to hit the cache entry its parent wrote.

use aibus_provider::{
    CacheHint, ChatRequest, ContentBlock, Effort, Message, MessageRole, ToolCallId, ToolChoice,
};
use base64::Engine;
use serde_json::{json, Map, Value};

/// The id the API sees for a call. The API accepts any id as long as the result carries the
/// same one, so it is derived from ours and survives a round trip through the transcript.
pub fn wire_id(id: ToolCallId) -> String {
    format!("toolu_{}", id.0)
}

fn block(b: &ContentBlock) -> Option<Value> {
    Some(match b {
        // The API rejects empty text blocks.
        ContentBlock::Text(t) if t.is_empty() => return None,
        ContentBlock::Text(t) => json!({"type": "text", "text": t}),
        ContentBlock::Thinking { text, redacted: true, .. } => {
            json!({"type": "redacted_thinking", "data": text})
        }
        ContentBlock::Thinking { text, signature: Some(sig), .. } => {
            json!({"type": "thinking", "thinking": text, "signature": sig})
        }
        // Without its signature the API would refuse the block, so it stays out.
        ContentBlock::Thinking { .. } => return None,
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": wire_id(*id), "name": name, "input": input})
        }
        ContentBlock::ToolResult { id, content, is_error } => {
            let mut v = json!({"type": "tool_result", "tool_use_id": wire_id(*id), "content": content});
            if *is_error {
                v["is_error"] = json!(true);
            }
            v
        }
        ContentBlock::Image { media_type, data } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            },
        }),
    })
}

/// The API has two roles, so a system message in the middle of a conversation (an advisor's
/// note) goes in as user text. Neighbours with one role become one message.
fn messages(msgs: &[Message]) -> Vec<Value> {
    let mut out: Vec<(&str, Vec<Value>)> = Vec::new();
    for m in msgs {
        let role = if m.role == MessageRole::Assistant { "assistant" } else { "user" };
        let blocks: Vec<Value> = m.blocks.iter().filter_map(block).collect();
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((r, content)) if *r == role => content.extend(blocks),
            _ => out.push((role, blocks)),
        }
    }
    out.into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

fn mark(v: &mut Value) {
    v["cache_control"] = json!({"type": "ephemeral"});
}

/// The body for `POST /v1/messages`. `effort` says the model takes the effort parameter.
pub fn messages_body(req: &ChatRequest, effort_ok: bool) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.0));
    body.insert("max_tokens".into(), json!(req.max_output_tokens));
    body.insert("stream".into(), json!(true));

    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
        .collect();
    let mut system = Vec::new();
    if !req.system.is_empty() {
        system.push(json!({"type": "text", "text": req.system}));
    }
    let mut msgs = messages(&req.messages);

    // One breakpoint, at the hint. A hint with nothing to mark (no tools, no system prompt)
    // marks nothing: there is no stable prefix to cache.
    match req.cache_hint {
        CacheHint::None => {}
        CacheHint::AfterTools => {
            if let Some(v) = tools.last_mut() {
                mark(v);
            }
        }
        CacheHint::AfterSystem => {
            if let Some(v) = system.last_mut().or(tools.last_mut()) {
                mark(v);
            }
        }
        CacheHint::LastMessage => {
            let last = msgs.last_mut().and_then(|m| m["content"].as_array_mut());
            if let Some(v) = last.and_then(|c| c.iter_mut().rev().find(|b| {
                // Every block type but thinking can carry a breakpoint.
                !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking"))
            })) {
                mark(v);
            }
        }
    }

    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    if !system.is_empty() {
        body.insert("system".into(), Value::Array(system));
    }
    body.insert("messages".into(), Value::Array(msgs));

    // Thinking needs at least 1,024 tokens and room left for the answer.
    let thinking = req
        .thinking
        .map(|t| t.tokens.min(req.max_output_tokens.saturating_sub(1)))
        .filter(|b| *b >= 1024);
    if let Some(budget) = thinking {
        body.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": budget}));
    } else if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !req.tools.is_empty() {
        // Forcing a tool is not allowed while thinking.
        let choice = match (&req.tool_choice, thinking) {
            (ToolChoice::Auto, _) | (_, Some(_)) => json!({"type": "auto"}),
            (ToolChoice::None, _) => json!({"type": "none"}),
            (ToolChoice::Required, _) => json!({"type": "any"}),
            (ToolChoice::Named(n), _) => json!({"type": "tool", "name": n}),
        };
        body.insert("tool_choice".into(), choice);
    }
    if let (Some(e), true) = (req.effort, effort_ok) {
        body.insert("output_config".into(), json!({"effort": match e {
            Effort::Low => "low",
            Effort::Medium => "medium",
            // The API stops at high.
            Effort::High | Effort::Max => "high",
        }}));
    }
    let ids: Vec<&str> = [req.metadata.session_id.as_deref(), req.metadata.run_id.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if !ids.is_empty() {
        body.insert("metadata".into(), json!({"user_id": format!("aibus:{}", ids.join(":"))}));
    }
    Value::Object(body)
}

/// The body for `POST /v1/messages/count_tokens`: what counts, without the sampling settings.
pub fn count_body(req: &ChatRequest) -> Value {
    let mut body = messages_body(req, false);
    if let Some(m) = body.as_object_mut() {
        for k in ["max_tokens", "stream", "temperature", "metadata"] {
            m.remove(k);
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_provider::{ModelId, RequestMetadata, ThinkingBudget, ToolSpec};
    use ulid::Ulid;

    fn req() -> ChatRequest {
        ChatRequest {
            model: ModelId("claude-sonnet-4-5".into()),
            system: "You are terse.".into(),
            messages: vec![Message::text(MessageRole::User, "hi")],
            tools: vec![
                ToolSpec { name: "read".into(), description: "Read a file".into(), schema: json!({"type": "object"}) },
                ToolSpec { name: "bash".into(), description: "Run".into(), schema: json!({"type": "object"}) },
            ],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 4096,
            temperature: Some(0.2),
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        }
    }

    fn count_marks(v: &Value) -> usize {
        match v {
            Value::Object(m) => usize::from(m.contains_key("cache_control")) + m.values().map(count_marks).sum::<usize>(),
            Value::Array(a) => a.iter().map(count_marks).sum(),
            _ => 0,
        }
    }

    #[test]
    fn a_plain_request_has_the_fields_the_api_needs() {
        let b = messages_body(&req(), false);
        assert_eq!(b["model"], "claude-sonnet-4-5");
        assert_eq!(b["max_tokens"], 4096);
        assert_eq!(b["stream"], true);
        assert_eq!(b["system"][0]["text"], "You are terse.");
        assert_eq!(b["tools"][1]["input_schema"], json!({"type": "object"}));
        assert_eq!(b["tool_choice"], json!({"type": "auto"}));
        assert_eq!(b["messages"][0], json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}));
        assert!((b["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(count_marks(&b), 0);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none() && b.get("metadata").is_none());
    }

    #[test]
    fn each_hint_puts_exactly_one_breakpoint_in_its_place() {
        let mut r = req();
        r.cache_hint = CacheHint::AfterTools;
        let b = messages_body(&r, false);
        assert_eq!(count_marks(&b), 1);
        assert!(b["tools"][1].get("cache_control").is_some() && b["tools"][0].get("cache_control").is_none());

        r.cache_hint = CacheHint::AfterSystem;
        let b = messages_body(&r, false);
        assert_eq!((count_marks(&b), b["system"][0]["cache_control"]["type"].as_str()), (1, Some("ephemeral")));

        r.cache_hint = CacheHint::LastMessage;
        r.messages.push(Message {
            role: MessageRole::Assistant,
            blocks: vec![ContentBlock::Text("a".into()), ContentBlock::Thinking { text: "t".into(), redacted: false, signature: Some("s".into()) }],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        let b = messages_body(&r, false);
        assert_eq!(count_marks(&b), 1);
        // The last block that can carry it: the text, not the thinking block after it.
        assert!(b["messages"][1]["content"][0].get("cache_control").is_some());
    }

    #[test]
    fn a_hint_with_nothing_to_mark_marks_nothing() {
        let mut r = req();
        r.tools.clear();
        r.system.clear();
        r.cache_hint = CacheHint::AfterSystem;
        assert_eq!(count_marks(&messages_body(&r, false)), 0);
    }

    #[test]
    fn a_fork_repeats_its_parents_prefix_byte_for_byte() {
        let parent = req();
        let mut fork = req();
        fork.messages.push(Message::text(MessageRole::Assistant, "ok"));
        fork.messages.push(Message::text(MessageRole::User, "now do the other thing"));
        for hint in [CacheHint::AfterTools, CacheHint::AfterSystem] {
            let (mut p, mut f) = (parent.clone(), fork.clone());
            p.cache_hint = hint;
            f.cache_hint = hint;
            let (pb, fb) = (messages_body(&p, false), messages_body(&f, false));
            for key in ["tools", "system"] {
                assert_eq!(pb[key].to_string(), fb[key].to_string(), "{key} under {hint:?}");
            }
            assert_eq!(pb["messages"][0].to_string(), fb["messages"][0].to_string());
        }
    }

    #[test]
    fn tool_calls_and_results_share_one_wire_id() {
        let id = ToolCallId(Ulid::from_parts(5, 9));
        let mut r = req();
        r.messages.push(Message {
            role: MessageRole::Assistant,
            blocks: vec![ContentBlock::ToolUse { id, name: "read".into(), input: json!({"p": 1}) }],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        r.messages.push(Message {
            role: MessageRole::User,
            blocks: vec![ContentBlock::ToolResult { id, content: "boom".into(), is_error: true }],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        let b = messages_body(&r, false);
        let call = &b["messages"][1]["content"][0];
        let result = &b["messages"][2]["content"][0];
        assert_eq!(call["id"], result["tool_use_id"]);
        assert!(call["id"].as_str().unwrap().starts_with("toolu_"));
        assert_eq!(result["is_error"], true);
    }

    #[test]
    fn neighbouring_messages_of_one_role_merge_and_system_notes_become_user_text() {
        let mut r = req();
        r.messages.push(Message::text(MessageRole::System, "advice"));
        r.messages.push(Message::text(MessageRole::Assistant, ""));
        let b = messages_body(&r, false);
        assert_eq!(b["messages"].as_array().unwrap().len(), 1);
        assert_eq!(b["messages"][0]["content"][1]["text"], "advice");
    }

    #[test]
    fn thinking_needs_room_and_turns_off_temperature_and_forced_tools() {
        let mut r = req();
        r.thinking = Some(ThinkingBudget { tokens: 2048 });
        r.tool_choice = ToolChoice::Required;
        let b = messages_body(&r, false);
        assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": 2048}));
        assert!(b.get("temperature").is_none());
        assert_eq!(b["tool_choice"], json!({"type": "auto"}));

        r.max_output_tokens = 1500;
        assert_eq!(messages_body(&r, false)["thinking"]["budget_tokens"], 1499);
        r.max_output_tokens = 1000;
        let b = messages_body(&r, false);
        assert!(b.get("thinking").is_none() && b.get("temperature").is_some());
    }

    #[test]
    fn tool_choice_maps_and_effort_needs_a_model_that_takes_it() {
        let mut r = req();
        r.effort = Some(Effort::Max);
        r.tool_choice = ToolChoice::Named("read".into());
        assert_eq!(messages_body(&r, true)["tool_choice"], json!({"type": "tool", "name": "read"}));
        assert_eq!(messages_body(&r, true)["output_config"], json!({"effort": "high"}));
        assert!(messages_body(&r, false).get("output_config").is_none());
        r.tool_choice = ToolChoice::None;
        assert_eq!(messages_body(&r, false)["tool_choice"], json!({"type": "none"}));
        r.tools.clear();
        assert!(messages_body(&r, false).get("tool_choice").is_none());
    }

    #[test]
    fn thinking_comes_back_with_its_signature_or_not_at_all() {
        let mut r = req();
        r.messages.push(Message {
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock::Thinking { text: "a".into(), redacted: false, signature: Some("sig".into()) },
                ContentBlock::Thinking { text: "unsigned".into(), redacted: false, signature: None },
                ContentBlock::Thinking { text: "opaque".into(), redacted: true, signature: None },
                ContentBlock::Text("x".into()),
            ],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        let c = &messages_body(&r, false)["messages"][1]["content"];
        assert_eq!(c[0], json!({"type": "thinking", "thinking": "a", "signature": "sig"}));
        assert_eq!(c[1], json!({"type": "redacted_thinking", "data": "opaque"}));
        assert_eq!(c.as_array().unwrap().len(), 3);
    }

    #[test]
    fn metadata_carries_ids_only_and_count_tokens_drops_sampling() {
        let mut r = req();
        r.metadata = RequestMetadata { run_id: Some("RUN".into()), session_id: Some("SES".into()) };
        assert_eq!(messages_body(&r, false)["metadata"]["user_id"], "aibus:SES:RUN");
        let c = count_body(&r);
        for k in ["max_tokens", "stream", "temperature", "metadata"] {
            assert!(c.get(k).is_none(), "{k}");
        }
        assert!(c.get("messages").is_some() && c.get("tools").is_some() && c.get("model").is_some());
    }

    #[test]
    fn an_image_goes_in_as_base64() {
        let mut r = req();
        r.messages[0].blocks.push(ContentBlock::Image {
            media_type: "image/png".into(),
            data: bytes::Bytes::from_static(b"\x89PNG"),
        });
        let img = &messages_body(&r, false)["messages"][0]["content"][1];
        assert_eq!(img["source"]["data"], "iVBORw==");
        assert_eq!(img["source"]["media_type"], "image/png");
    }
}
