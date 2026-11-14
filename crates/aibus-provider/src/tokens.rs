//! The token-count heuristic for providers without a counting endpoint (Q20): 4 characters per
//! token, plus a 10 % margin so `auto_compact_at` fires early rather than late.

use crate::provider::ChatRequest;
use crate::types::ContentBlock;

pub const CHARS_PER_TOKEN: f64 = 4.0;

/// A rough charge for one image, whatever its size.
const IMAGE_TOKENS: u64 = 1_600;

/// The request's estimated size in tokens, margin included. `chars_per_token` is the model
/// family's override from `providers/<id>/models.json`, or `CHARS_PER_TOKEN`.
pub fn estimate(req: &ChatRequest, chars_per_token: f64) -> u64 {
    let chars = |s: &str| s.chars().count();
    let mut n = chars(&req.system);
    let mut images = 0u64;
    for t in &req.tools {
        n += chars(&t.name) + chars(&t.description) + chars(&t.schema.to_string());
    }
    for b in req.messages.iter().flat_map(|m| &m.blocks) {
        n += match b {
            ContentBlock::Text(t) => chars(t),
            ContentBlock::Thinking { text, .. } => chars(text),
            ContentBlock::ToolUse { name, input, .. } => chars(name) + chars(&input.to_string()),
            ContentBlock::ToolResult { content, .. } => chars(content),
            ContentBlock::Image { .. } => {
                images += 1;
                0
            }
        };
    }
    let per = if chars_per_token > 0.0 {
        chars_per_token
    } else {
        CHARS_PER_TOKEN
    };
    ((n as f64 * 11.0 / (per * 10.0)).ceil() as u64) + images * IMAGE_TOKENS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn req(text: &str) -> ChatRequest {
        ChatRequest {
            model: ModelId("m".into()),
            system: String::new(),
            messages: vec![Message::text(MessageRole::User, text)],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 16,
            temperature: None,
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        }
    }

    #[test]
    fn four_characters_a_token_plus_ten_percent() {
        // 400 chars = 100 tokens, 110 with the margin.
        assert_eq!(estimate(&req(&"a".repeat(400)), CHARS_PER_TOKEN), 110);
    }

    #[test]
    fn a_family_override_changes_the_ratio() {
        assert_eq!(estimate(&req(&"a".repeat(400)), 2.0), 220);
        assert_eq!(estimate(&req(&"a".repeat(400)), 0.0), 110, "a bad override falls back");
    }

    #[test]
    fn characters_are_counted_not_bytes() {
        assert_eq!(estimate(&req(&"\u{e9}".repeat(400)), CHARS_PER_TOKEN), 110);
    }

    #[test]
    fn system_tools_and_images_count_too() {
        let mut r = req("");
        r.system = "s".repeat(40);
        r.tools = vec![ToolSpec {
            name: "t".into(),
            description: "d".repeat(36),
            schema: serde_json::json!({}),
        }];
        r.messages[0].blocks.push(ContentBlock::Image {
            media_type: "image/png".into(),
            data: bytes::Bytes::new(),
        });
        // 40 + 1 + 36 + 2 = 79 chars -> ceil(79 / 4 * 1.1) = 22.
        assert_eq!(estimate(&r, CHARS_PER_TOKEN), 22 + IMAGE_TOKENS);
    }
}
