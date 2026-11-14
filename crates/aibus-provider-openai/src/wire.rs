//! Request bodies for Chat Completions and the Responses API.

use aibus_provider::presets::{CompatFlag, MaxTokensField, ReasoningField};
use aibus_provider::{ChatRequest, ContentBlock, Effort, Message, MessageRole, ToolCallId, ToolChoice};
use base64::Engine;
use serde_json::{json, Map, Value};

/// What differs between OpenAI, Kimi and a local server for one request: the model's own
/// facts, and the provider's compat flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Opts<'a> {
    /// The model reasons: it takes the effort parameter and refuses `temperature`.
    pub reasoning: bool,
    /// Send the run and session ids as `user`.
    pub user: bool,
    /// Ask for token logprobs.
    pub logprobs: bool,
    /// Ask the gateway to report what the request cost (`usage: {include: true}`, OpenRouter).
    pub include_cost: bool,
    pub compat: &'a [CompatFlag],
}

impl Opts<'_> {
    fn has(&self, f: CompatFlag) -> bool {
        self.compat.contains(&f)
    }
}

/// The id the API sees for a call, derived from ours so a result finds its call.
pub fn wire_id(id: ToolCallId) -> String {
    format!("call_{}", id.0)
}

fn effort(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Max => "high",
    }
}

fn data_url(media_type: &str, data: &[u8]) -> String {
    format!("data:{media_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(data))
}

fn user_id(req: &ChatRequest) -> Option<String> {
    let ids: Vec<&str> = [req.metadata.session_id.as_deref(), req.metadata.run_id.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    (!ids.is_empty()).then(|| format!("aibus:{}", ids.join(":")))
}

fn chat_messages(req: &ChatRequest, o: Opts<'_>) -> Vec<Value> {
    let mut out = Vec::new();
    if !req.system.is_empty() {
        out.push(json!({"role": "system", "content": req.system}));
    }
    for m in &req.messages {
        match m.role {
            MessageRole::Assistant => out.extend(chat_assistant(m, o)),
            MessageRole::User => out.extend(chat_user(m)),
            MessageRole::System => {
                let text = m.plain_text();
                if !text.is_empty() {
                    out.push(json!({"role": "system", "content": text}));
                }
            }
        }
    }
    out
}

/// Tool results are messages of their own, and come before the user's text in the turn.
fn chat_user(m: &Message) -> Vec<Value> {
    let mut out = Vec::new();
    let mut parts = Vec::new();
    for b in &m.blocks {
        match b {
            ContentBlock::ToolResult { id, content, .. } => {
                out.push(json!({"role": "tool", "tool_call_id": wire_id(*id), "content": content}));
            }
            ContentBlock::Text(t) if !t.is_empty() => parts.push(json!({"type": "text", "text": t})),
            ContentBlock::Image { media_type, data } => parts.push(
                json!({"type": "image_url", "image_url": {"url": data_url(media_type, data)}}),
            ),
            _ => {}
        }
    }
    let only_text = parts.iter().all(|p| p["type"] == "text");
    if only_text && !parts.is_empty() {
        let joined: Vec<&str> = parts.iter().filter_map(|p| p["text"].as_str()).collect();
        out.push(json!({"role": "user", "content": joined.join("\n")}));
    } else if !parts.is_empty() {
        out.push(json!({"role": "user", "content": parts}));
    }
    out
}

fn chat_assistant(m: &Message, o: Opts<'_>) -> Option<Value> {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls = Vec::new();
    for b in &m.blocks {
        match b {
            ContentBlock::Text(t) => text.push_str(t),
            ContentBlock::Thinking { text, redacted: false, .. } => thinking.push_str(text),
            ContentBlock::ToolUse { id, name, input, .. } => calls.push(json!({
                "id": wire_id(*id),
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()},
            })),
            _ => {}
        }
    }
    if text.is_empty() && calls.is_empty() {
        return None;
    }
    let mut v = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
    if !calls.is_empty() {
        v["tool_calls"] = Value::Array(calls);
    }
    if o.has(CompatFlag::ReasoningContentReplay) && !thinking.is_empty() {
        v["reasoning_content"] = json!(thinking);
    }
    Some(v)
}

/// `None` when the server takes no `tool_choice` at all. The shape of a named choice differs
/// between the two APIs, so the caller says it.
fn tool_choice(c: &ToolChoice, o: Opts<'_>, named: impl Fn(&str) -> Value) -> Option<Value> {
    if o.has(CompatFlag::NoToolChoice) {
        return None;
    }
    Some(match c {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named(_) if o.has(CompatFlag::NoNamedToolChoice) => json!("required"),
        ToolChoice::Named(n) => named(n),
    })
}

/// The body for `POST /chat/completions`.
pub fn chat_body(req: &ChatRequest, o: Opts<'_>) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.0));
    body.insert("messages".into(), Value::Array(chat_messages(req, o)));
    body.insert("stream".into(), json!(true));
    if !o.has(CompatFlag::NoUsageInStream) {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    if o.include_cost {
        body.insert("usage".into(), json!({"include": true}));
    }
    let limit = o.compat.iter().find_map(|f| match f {
        CompatFlag::MaxTokensField(m) => Some(*m),
        _ => None,
    });
    let key = match limit {
        Some(MaxTokensField::MaxCompletionTokens) => "max_completion_tokens",
        _ => "max_tokens",
    };
    body.insert(key.into(), json!(req.max_output_tokens));
    if o.reasoning {
        if let Some(e) = req.effort {
            let nested = o.compat.contains(&CompatFlag::ReasoningField(ReasoningField::Reasoning));
            match nested {
                true => body.insert("reasoning".into(), json!({"effort": effort(e)})),
                false => body.insert("reasoning_effort".into(), json!(effort(e))),
            };
        }
    } else if let (Some(t), false) = (req.temperature_f64(), o.has(CompatFlag::NoSamplingParams)) {
        body.insert("temperature".into(), json!(t));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        let named = |n: &str| json!({"type": "function", "function": {"name": n}});
        if let Some(c) = tool_choice(&req.tool_choice, o, named) {
            body.insert("tool_choice".into(), c);
        }
    }
    if o.logprobs {
        body.insert("logprobs".into(), json!(true));
        body.insert("top_logprobs".into(), json!(5));
    }
    if let (true, Some(u)) = (o.user, user_id(req)) {
        body.insert("user".into(), json!(u));
    }
    Value::Object(body)
}

/// The body for `POST /responses`. The conversation is sent whole each time (`store: false`),
/// so nothing depends on the server remembering it.
pub fn responses_body(req: &ChatRequest, o: Opts<'_>) -> Value {
    let mut input = Vec::new();
    for m in &req.messages {
        match m.role {
            MessageRole::Assistant => {
                let mut text = String::new();
                for b in &m.blocks {
                    match b {
                        ContentBlock::Text(t) => text.push_str(t),
                        ContentBlock::ToolUse { id, name, input: args, .. } => {
                            if !text.is_empty() {
                                input.push(json!({"role": "assistant", "content": [{"type": "output_text", "text": std::mem::take(&mut text)}]}));
                            }
                            input.push(json!({"type": "function_call", "call_id": wire_id(*id), "name": name, "arguments": args.to_string()}));
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    input.push(json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]}));
                }
            }
            MessageRole::User | MessageRole::System => {
                let role = match (m.role, o.has(CompatFlag::NoDeveloperRole)) {
                    (MessageRole::User, _) => "user",
                    (_, true) => "system",
                    (_, false) => "developer",
                };
                let mut parts = Vec::new();
                for b in &m.blocks {
                    match b {
                        ContentBlock::ToolResult { id, content, .. } => {
                            input.push(json!({"type": "function_call_output", "call_id": wire_id(*id), "output": content}));
                        }
                        ContentBlock::Text(t) if !t.is_empty() => parts.push(json!({"type": "input_text", "text": t})),
                        ContentBlock::Image { media_type, data } => {
                            parts.push(json!({"type": "input_image", "image_url": data_url(media_type, data)}));
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    input.push(json!({"role": role, "content": parts}));
                }
            }
        }
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.0));
    body.insert("input".into(), Value::Array(input));
    body.insert("stream".into(), json!(true));
    if !o.has(CompatFlag::NoStore) {
        body.insert("store".into(), json!(false));
    }
    body.insert("max_output_tokens".into(), json!(req.max_output_tokens));
    if !req.system.is_empty() {
        body.insert("instructions".into(), json!(req.system));
    }
    if o.reasoning {
        if let Some(e) = req.effort {
            body.insert("reasoning".into(), json!({"effort": effort(e)}));
        }
    } else if let (Some(t), false) = (req.temperature_f64(), o.has(CompatFlag::NoSamplingParams)) {
        body.insert("temperature".into(), json!(t));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.schema});
                if !o.has(CompatFlag::NoStrictTools) {
                    tool["strict"] = json!(false);
                }
                tool
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        let named = |n: &str| json!({"type": "function", "name": n});
        if let Some(c) = tool_choice(&req.tool_choice, o, named) {
            body.insert("tool_choice".into(), c);
        }
    }
    if o.logprobs {
        body.insert("top_logprobs".into(), json!(5));
        body.insert("include".into(), json!(["message.output_text.logprobs"]));
    }
    if let (true, Some(u)) = (o.user, user_id(req)) {
        body.insert("user".into(), json!(u));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_provider::{CacheHint, Capabilities, ModelId, RequestMetadata, SystemRole, ToolSpec};
    use ulid::Ulid;

    const PLAIN: Opts<'static> = Opts { reasoning: false, user: false, logprobs: false, include_cost: false, compat: &[] };

    fn with(compat: &'static [CompatFlag]) -> Opts<'static> {
        Opts { compat, ..PLAIN }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: ModelId("gpt-4.1".into()),
            system: "Be terse.".into(),
            messages: vec![Message::text(MessageRole::User, "hi")],
            tools: vec![ToolSpec { name: "read".into(), description: "Read".into(), schema: json!({"type": "object"}) }],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 256,
            temperature: Some(0.5),
            effort: Some(Effort::Max),
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata { run_id: Some("RUN".into()), session_id: Some("SES".into()) },
        }
    }

    fn history() -> ChatRequest {
        let id = ToolCallId(Ulid::from_parts(1, 2));
        let mut r = req();
        r.messages.push(Message {
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock::Thinking { text: "hmm".into(), redacted: false, signature: None },
                ContentBlock::Text("looking".into()),
                ContentBlock::ToolUse { id, name: "read".into(), input: json!({"p": 1}), signature: None },
            ],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        r.messages.push(Message {
            role: MessageRole::User,
            blocks: vec![
                ContentBlock::ToolResult { id, content: "contents".into(), is_error: false },
                ContentBlock::Text("and then?".into()),
            ],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        r
    }

    #[test]
    fn chat_body_has_system_tools_and_usage_streaming() {
        let b = chat_body(&req(), PLAIN);
        assert_eq!(b["messages"][0], json!({"role": "system", "content": "Be terse."}));
        assert_eq!(b["messages"][1], json!({"role": "user", "content": "hi"}));
        assert_eq!(b["tools"][0]["function"]["parameters"], json!({"type": "object"}));
        assert_eq!(b["tool_choice"], "auto");
        assert_eq!(b["stream_options"], json!({"include_usage": true}));
        assert_eq!(b["max_tokens"], 256);
        assert!(b.get("max_completion_tokens").is_none() && b.get("user").is_none() && b.get("logprobs").is_none());
        assert!((b["temperature"].as_f64().unwrap() - 0.5).abs() < 1e-6);
        assert!(b.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_reasoning_model_takes_effort_and_completion_tokens_but_no_temperature() {
        let o = Opts { reasoning: true, compat: &[CompatFlag::MaxTokensField(MaxTokensField::MaxCompletionTokens)], ..PLAIN };
        let b = chat_body(&req(), o);
        assert_eq!(b["max_completion_tokens"], 256);
        assert_eq!(b["reasoning_effort"], "high");
        assert!(b.get("temperature").is_none() && b.get("max_tokens").is_none());
    }

    #[test]
    fn logprobs_and_user_ids_are_sent_only_when_asked() {
        let o = Opts { logprobs: true, user: true, ..PLAIN };
        let b = chat_body(&req(), o);
        assert_eq!((b["logprobs"].clone(), b["top_logprobs"].clone()), (json!(true), json!(5)));
        assert_eq!(b["user"], "aibus:SES:RUN");
    }

    #[test]
    fn history_becomes_assistant_tool_calls_and_tool_messages_before_user_text() {
        let b = chat_body(&history(), PLAIN);
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["content"], "looking");
        assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"p\":1}");
        assert!(m[2].get("reasoning_content").is_none());
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["tool_call_id"], m[2]["tool_calls"][0]["id"]);
        assert_eq!(m[4], json!({"role": "user", "content": "and then?"}));

        let k = with(&[CompatFlag::ReasoningContentReplay]);
        assert_eq!(chat_body(&history(), k)["messages"][2]["reasoning_content"], "hmm");
    }

    #[test]
    fn an_assistant_turn_with_only_calls_has_null_content_and_an_empty_one_is_dropped() {
        let id = ToolCallId(Ulid::from_parts(1, 3));
        let mut r = req();
        r.messages.push(Message {
            role: MessageRole::Assistant,
            blocks: vec![ContentBlock::ToolUse { id, name: "read".into(), input: json!({}), signature: None }],
            at: std::time::SystemTime::UNIX_EPOCH,
        });
        r.messages.push(Message::text(MessageRole::Assistant, ""));
        let m = chat_body(&r, PLAIN)["messages"].clone();
        assert_eq!(m.as_array().unwrap().len(), 3);
        assert!(m[2]["content"].is_null());
    }

    #[test]
    fn images_make_the_user_content_an_array_of_parts() {
        let mut r = req();
        r.messages[0].blocks.push(ContentBlock::Image { media_type: "image/png".into(), data: bytes::Bytes::from_static(b"\x89PNG") });
        let c = &chat_body(&r, PLAIN)["messages"][1]["content"];
        assert_eq!(c[0]["type"], "text");
        assert_eq!(c[1]["image_url"]["url"], "data:image/png;base64,iVBORw==");
    }

    #[test]
    fn tool_choice_maps() {
        let mut r = req();
        r.tool_choice = ToolChoice::Named("read".into());
        assert_eq!(chat_body(&r, PLAIN)["tool_choice"]["function"]["name"], "read");
        assert_eq!(responses_body(&r, PLAIN)["tool_choice"], json!({"type": "function", "name": "read"}));
        r.tool_choice = ToolChoice::Required;
        assert_eq!(chat_body(&r, PLAIN)["tool_choice"], "required");
        r.tools.clear();
        assert!(chat_body(&r, PLAIN).get("tool_choice").is_none());
    }

    #[test]
    fn the_cost_is_only_asked_for_when_the_gateway_reports_it() {
        assert!(chat_body(&req(), PLAIN).get("usage").is_none());
        let o = Opts { include_cost: true, ..PLAIN };
        assert_eq!(chat_body(&req(), o)["usage"], json!({"include": true}));
    }

    #[test]
    fn responses_body_sends_items_instructions_and_no_server_state() {
        let o = Opts { reasoning: true, logprobs: true, ..PLAIN };
        let b = responses_body(&history(), o);
        assert_eq!(b["instructions"], "Be terse.");
        assert_eq!(b["store"], false);
        assert_eq!(b["max_output_tokens"], 256);
        assert_eq!(b["reasoning"], json!({"effort": "high"}));
        assert!(b.get("temperature").is_none());
        assert_eq!(b["include"][0], "message.output_text.logprobs");
        let i = b["input"].as_array().unwrap();
        assert_eq!(i[0], json!({"role": "user", "content": [{"type": "input_text", "text": "hi"}]}));
        assert_eq!(i[1]["content"][0], json!({"type": "output_text", "text": "looking"}));
        assert_eq!(i[2]["type"], "function_call");
        assert_eq!(i[3]["type"], "function_call_output");
        assert_eq!(i[2]["call_id"], i[3]["call_id"]);
        assert_eq!(i[4]["content"][0]["text"], "and then?");
        assert_eq!(b["tools"][0]["name"], "read");
    }

    // One test per compat flag, each asserting the body that goes out, not the answer.

    #[test]
    fn flag_no_strict_tools_leaves_strict_off_the_responses_tools() {
        assert_eq!(responses_body(&req(), PLAIN)["tools"][0]["strict"], false);
        assert!(responses_body(&req(), with(&[CompatFlag::NoStrictTools]))["tools"][0].get("strict").is_none());
    }

    #[test]
    fn flag_no_developer_role_sends_system_where_responses_would_say_developer() {
        let mut r = req();
        r.messages.push(Message::text(MessageRole::System, "note"));
        assert_eq!(responses_body(&r, PLAIN)["input"][1]["role"], "developer");
        assert_eq!(responses_body(&r, with(&[CompatFlag::NoDeveloperRole]))["input"][1]["role"], "system");
    }

    #[test]
    fn flag_max_tokens_field_names_the_limit_key() {
        let b = chat_body(&req(), with(&[CompatFlag::MaxTokensField(MaxTokensField::MaxCompletionTokens)]));
        assert_eq!(b["max_completion_tokens"], 256);
        assert!(b.get("max_tokens").is_none());
        let b = chat_body(&req(), with(&[CompatFlag::MaxTokensField(MaxTokensField::MaxTokens)]));
        assert_eq!(b["max_tokens"], 256);
    }

    #[test]
    fn flag_reasoning_field_picks_reasoning_effort_or_reasoning() {
        let on = Opts { reasoning: true, ..PLAIN };
        assert_eq!(chat_body(&req(), on)["reasoning_effort"], "high");
        let o = Opts { reasoning: true, compat: &[CompatFlag::ReasoningField(ReasoningField::Reasoning)], ..PLAIN };
        let b = chat_body(&req(), o);
        assert_eq!(b["reasoning"], json!({"effort": "high"}));
        assert!(b.get("reasoning_effort").is_none());
    }

    #[test]
    fn flag_reasoning_content_replay_sends_earlier_thinking_back() {
        let sent = |o| chat_body(&history(), o)["messages"][2].get("reasoning_content").cloned();
        assert_eq!(sent(PLAIN), None);
        assert_eq!(sent(with(&[CompatFlag::ReasoningContentReplay])), Some(json!("hmm")));
    }

    #[test]
    fn flag_no_usage_in_stream_drops_stream_options() {
        assert_eq!(chat_body(&req(), PLAIN)["stream_options"], json!({"include_usage": true}));
        assert!(chat_body(&req(), with(&[CompatFlag::NoUsageInStream])).get("stream_options").is_none());
    }

    #[test]
    fn flag_no_tool_choice_sends_tools_without_a_choice() {
        let b = chat_body(&req(), with(&[CompatFlag::NoToolChoice]));
        assert!(b.get("tools").is_some() && b.get("tool_choice").is_none());
        let b = responses_body(&req(), with(&[CompatFlag::NoToolChoice]));
        assert!(b.get("tools").is_some() && b.get("tool_choice").is_none());
    }

    #[test]
    fn flag_no_named_tool_choice_turns_a_named_choice_into_required() {
        let mut r = req();
        r.tool_choice = ToolChoice::Named("read".into());
        let o = with(&[CompatFlag::NoNamedToolChoice]);
        assert_eq!(chat_body(&r, o)["tool_choice"], "required");
        assert_eq!(responses_body(&r, o)["tool_choice"], "required");
        r.tool_choice = ToolChoice::Auto;
        assert_eq!(chat_body(&r, o)["tool_choice"], "auto", "other choices are untouched");
    }

    #[test]
    fn flag_no_store_leaves_store_out_of_the_responses_body() {
        assert_eq!(responses_body(&req(), PLAIN)["store"], false);
        assert!(responses_body(&req(), with(&[CompatFlag::NoStore])).get("store").is_none());
    }

    #[test]
    fn flag_no_sampling_params_leaves_temperature_out() {
        assert!(chat_body(&req(), PLAIN).get("temperature").is_some());
        assert!(chat_body(&req(), with(&[CompatFlag::NoSamplingParams])).get("temperature").is_none());
        assert!(responses_body(&req(), with(&[CompatFlag::NoSamplingParams])).get("temperature").is_none());
    }

    fn all_on() -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: false,
            prompt_cache: false,
            logprobs: true,
            effort: true,
            images: true,
            system_role: SystemRole::Separate,
            max_tools: None,
        }
    }

    #[test]
    fn flag_no_system_role_folds_the_prompt_into_the_first_user_message() {
        let c = all_on().with_compat(&[CompatFlag::NoSystemRole]);
        let b = chat_body(&c.degrade(req()), PLAIN);
        assert_eq!(b["messages"][0], json!({"role": "user", "content": "Be terse.\n\n\nhi"}));
        assert!(!b["messages"].as_array().unwrap().iter().any(|m| m["role"] == "system"));
    }

    #[test]
    fn flag_strip_images_sends_a_note_instead_of_the_picture() {
        let mut r = req();
        r.messages[0].blocks.push(ContentBlock::Image { media_type: "image/png".into(), data: bytes::Bytes::from_static(b"\x89PNG") });
        let c = all_on().with_compat(&[CompatFlag::StripImages]);
        let b = chat_body(&c.degrade(r), PLAIN);
        assert!(!b.to_string().contains("image_url"), "{b}");
        assert!(b.to_string().contains("[image omitted]"));
    }
}
