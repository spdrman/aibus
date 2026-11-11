//! The `generateContent` request body, and the schema cleaner it needs.

use aibus_provider::{ChatRequest, ContentBlock, MessageRole, ToolChoice};
use base64::Engine;
use serde_json::{json, Map, Value};

/// Keywords Gemini answers with a 400 when a function's schema carries them.
const REJECTED: [&str; 3] = ["$schema", "additionalProperties", "default"];

/// `schema` without the keywords Gemini rejects, at any depth. Names under `properties` are
/// the tool's own and stay (a parameter may be called `default`), and the values of `enum`,
/// `const` and `examples` are data, so they are copied as they are. `None` when what is left
/// is an object with no properties, which Gemini refuses too: the tool then takes no
/// parameters and is declared without any.
pub fn clean_schema(schema: &Value) -> Option<Value> {
    fn clean(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, val) in map {
                    if REJECTED.contains(&k.as_str()) {
                        continue;
                    }
                    let kept = match (k.as_str(), val) {
                        ("enum" | "const" | "examples", _) => val.clone(),
                        ("properties", Value::Object(props)) => {
                            Value::Object(props.iter().map(|(name, p)| (name.clone(), clean(p))).collect())
                        }
                        _ => clean(val),
                    };
                    out.insert(k.clone(), kept);
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(clean).collect()),
            other => other.clone(),
        }
    }
    let cleaned = clean(schema);
    let empty_object = cleaned["type"] == "object" && cleaned["properties"].as_object().is_none_or(Map::is_empty);
    (!empty_object).then_some(cleaned)
}

/// Model-visible text of a tool call's id: the name the call was made under, which Gemini
/// wants on the `functionResponse` instead of an id.
fn call_name(req: &ChatRequest, id: aibus_provider::ToolCallId) -> String {
    req.messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            ContentBlock::ToolUse { id: x, name, .. } if *x == id => Some(name.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn parts(req: &ChatRequest, blocks: &[ContentBlock]) -> Vec<Value> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text(t) if !t.is_empty() => out.push(json!({"text": t})),
            ContentBlock::ToolUse { name, input, signature, .. } => {
                let mut part = json!({"functionCall": {"name": name, "args": input}});
                // Sent back exactly as it arrived, or Gemini 3 refuses the turn.
                if let Some(sig) = signature {
                    part["thoughtSignature"] = json!(sig);
                }
                out.push(part);
            }
            ContentBlock::ToolResult { id, content, is_error } => {
                let key = if *is_error { "error" } else { "output" };
                out.push(json!({"functionResponse": {"name": call_name(req, *id), "response": {key: content}}}));
            }
            ContentBlock::Image { media_type, data } => out.push(json!({
                "inlineData": {"mimeType": media_type, "data": base64::engine::general_purpose::STANDARD.encode(data)}
            })),
            // Thought summaries are not sent back; only the call's signature is.
            _ => {}
        }
    }
    out
}

/// The body of `generateContent`, `streamGenerateContent` and (inside `generateContentRequest`)
/// `countTokens`. `reasoning` says the model takes `thinkingConfig`.
pub fn body(req: &ChatRequest, reasoning: bool) -> Value {
    let mut system = vec![req.system.clone()];
    let mut contents = Vec::new();
    for m in &req.messages {
        if m.role == MessageRole::System {
            system.push(m.plain_text());
            continue;
        }
        let p = parts(req, &m.blocks);
        if !p.is_empty() {
            let role = if m.role == MessageRole::Assistant { "model" } else { "user" };
            contents.push(json!({"role": role, "parts": p}));
        }
    }
    let mut v = json!({"contents": contents});
    let system: Vec<Value> = system.into_iter().filter(|s| !s.is_empty()).map(|s| json!({"text": s})).collect();
    if !system.is_empty() {
        v["systemInstruction"] = json!({"parts": system});
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut d = json!({"name": t.name, "description": t.description});
                if let Some(params) = clean_schema(&t.schema) {
                    d["parameters"] = params;
                }
                d
            })
            .collect();
        v["tools"] = json!([{"functionDeclarations": decls}]);
        let config = match &req.tool_choice {
            ToolChoice::Auto => json!({"mode": "AUTO"}),
            ToolChoice::None => json!({"mode": "NONE"}),
            ToolChoice::Required => json!({"mode": "ANY"}),
            ToolChoice::Named(n) => json!({"mode": "ANY", "allowedFunctionNames": [n]}),
        };
        v["toolConfig"] = json!({"functionCallingConfig": config});
    }
    let mut generation = json!({"maxOutputTokens": req.max_output_tokens});
    if let Some(t) = req.temperature {
        generation["temperature"] = json!(t);
    }
    if let (true, Some(budget)) = (reasoning, req.thinking) {
        generation["thinkingConfig"] = json!({"thinkingBudget": budget.tokens, "includeThoughts": true});
    }
    v["generationConfig"] = generation;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_provider::{CacheHint, Message, ModelId, RequestMetadata, ThinkingBudget, ToolCallId, ToolSpec};

    fn req(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: ModelId("gemini-2.5-pro".into()),
            system: "Be terse.".into(),
            messages,
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 256,
            temperature: Some(0.5),
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        }
    }

    fn msg(role: MessageRole, blocks: Vec<ContentBlock>) -> Message {
        Message { role, blocks, at: std::time::SystemTime::UNIX_EPOCH }
    }

    #[test]
    fn the_cleaner_drops_what_gemini_rejects_at_every_depth() {
        let cleaned = clean_schema(&json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "default": "."},
                "opts": {"type": "object", "additionalProperties": true, "properties": {"deep": {"type": "integer", "default": 1}}},
                "list": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"n": {"type": "string"}}}},
                "pick": {"anyOf": [{"type": "string", "default": "a"}, {"type": "null"}]}
            },
            "required": ["path"]
        }))
        .unwrap();
        let text = cleaned.to_string();
        for word in REJECTED {
            assert!(!text.contains(word), "{word} survived: {text}");
        }
        assert_eq!(cleaned["required"], json!(["path"]));
        assert_eq!(cleaned["properties"]["opts"]["properties"]["deep"], json!({"type": "integer"}));
        assert_eq!(cleaned["properties"]["list"]["items"]["properties"]["n"], json!({"type": "string"}));
        assert_eq!(cleaned["properties"]["pick"]["anyOf"][0], json!({"type": "string"}));
    }

    #[test]
    fn a_parameter_called_default_and_enum_data_are_left_alone() {
        let cleaned = clean_schema(&json!({
            "type": "object",
            "properties": {
                "default": {"type": "string", "default": "x"},
                "mode": {"enum": ["a", {"default": 1}]}
            }
        }))
        .unwrap();
        assert_eq!(cleaned["properties"]["default"], json!({"type": "string"}));
        assert_eq!(cleaned["properties"]["mode"]["enum"], json!(["a", {"default": 1}]));
    }

    #[test]
    fn a_tool_without_parameters_is_declared_without_a_schema() {
        assert_eq!(clean_schema(&json!({"type": "object"})), None);
        assert_eq!(clean_schema(&json!({"type": "object", "properties": {}, "additionalProperties": false})), None);
        let mut r = req(vec![Message::text(MessageRole::User, "hi")]);
        r.tools = vec![ToolSpec { name: "now".into(), description: "Time".into(), schema: json!({"type": "object"}) }];
        let b = body(&r, false);
        assert_eq!(b["tools"][0]["functionDeclarations"][0], json!({"name": "now", "description": "Time"}));
    }

    #[test]
    fn the_request_has_the_shape_gemini_documents() {
        let id = ToolCallId(ulid::Ulid::new());
        let mut r = req(vec![
            Message::text(MessageRole::User, "read it"),
            msg(MessageRole::Assistant, vec![ContentBlock::ToolUse {
                id,
                name: "read_file".into(),
                input: json!({"path": "a"}),
                signature: Some("c2ln".into()),
            }]),
            msg(MessageRole::User, vec![ContentBlock::ToolResult { id, content: "data".into(), is_error: false }]),
        ]);
        r.tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "Read".into(),
            schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        r.tool_choice = ToolChoice::Named("read_file".into());
        r.thinking = Some(ThinkingBudget { tokens: 2048 });
        let b = body(&r, true);
        assert_eq!(b["systemInstruction"], json!({"parts": [{"text": "Be terse."}]}));
        assert_eq!(b["contents"][0], json!({"role": "user", "parts": [{"text": "read it"}]}));
        assert_eq!(b["contents"][1]["role"], "model");
        assert_eq!(b["contents"][1]["parts"][0], json!({
            "functionCall": {"name": "read_file", "args": {"path": "a"}},
            "thoughtSignature": "c2ln"
        }));
        assert_eq!(b["contents"][2]["parts"][0], json!({
            "functionResponse": {"name": "read_file", "response": {"output": "data"}}
        }));
        assert_eq!(b["toolConfig"], json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["read_file"]}}));
        assert_eq!(b["generationConfig"], json!({
            "maxOutputTokens": 256,
            "temperature": 0.5,
            "thinkingConfig": {"thinkingBudget": 2048, "includeThoughts": true}
        }));
    }

    #[test]
    fn tool_choice_maps_to_the_calling_modes_and_a_model_without_thinking_gets_no_config() {
        let mut r = req(vec![Message::text(MessageRole::User, "x")]);
        r.tools = vec![ToolSpec { name: "t".into(), description: String::new(), schema: json!({"type": "object"}) }];
        r.thinking = Some(ThinkingBudget { tokens: 1 });
        for (choice, mode) in [(ToolChoice::Auto, "AUTO"), (ToolChoice::None, "NONE"), (ToolChoice::Required, "ANY")] {
            r.tool_choice = choice;
            let b = body(&r, false);
            assert_eq!(b["toolConfig"]["functionCallingConfig"]["mode"], mode);
            assert!(b["generationConfig"].get("thinkingConfig").is_none());
        }
    }

    #[test]
    fn a_failed_tool_result_says_error_and_a_system_message_joins_the_instruction() {
        let id = ToolCallId(ulid::Ulid::new());
        let r = req(vec![
            msg(MessageRole::Assistant, vec![ContentBlock::ToolUse { id, name: "run".into(), input: json!({}), signature: None }]),
            msg(MessageRole::User, vec![ContentBlock::ToolResult { id, content: "boom".into(), is_error: true }]),
            Message::text(MessageRole::System, "Extra rule."),
        ]);
        let b = body(&r, false);
        assert_eq!(b["contents"][1]["parts"][0]["functionResponse"]["response"], json!({"error": "boom"}));
        assert_eq!(b["systemInstruction"]["parts"][1], json!({"text": "Extra rule."}));
        assert!(b["contents"][0]["parts"][0].get("thoughtSignature").is_none());
    }
}
