//! Degrading a request to what a provider can do, instead of letting the API reject it.

use tracing::debug;

use crate::provider::{Capabilities, ChatRequest, SystemRole};
use crate::types::{CacheHint, ContentBlock, Message, MessageRole, ToolChoice};

impl Capabilities {
    /// Drops what this provider does not support and says so at debug level. The only
    /// structural change is `SystemRole::FirstMessage`, which folds `system` into the first
    /// user message (or a new one in front when the conversation starts with something else).
    pub fn degrade(&self, mut req: ChatRequest) -> ChatRequest {
        if !self.effort && req.effort.take().is_some() {
            debug!("capability: effort not supported, dropped");
        }
        if !self.thinking && req.thinking.take().is_some() {
            debug!("capability: thinking not supported, dropped");
        }
        if !self.logprobs && std::mem::take(&mut req.logprobs) {
            debug!("capability: logprobs not supported, dropped");
        }
        if !self.prompt_cache && req.cache_hint != CacheHint::None {
            debug!("capability: prompt cache not supported, hint dropped");
            req.cache_hint = CacheHint::None;
        }
        if !self.tools && !req.tools.is_empty() {
            debug!(
                count = req.tools.len(),
                "capability: tools not supported, dropped"
            );
            req.tools.clear();
            req.tool_choice = ToolChoice::Auto;
        }
        if let Some(max) = self
            .max_tools
            .map(|m| m as usize)
            .filter(|m| req.tools.len() > *m)
        {
            debug!(
                kept = max,
                had = req.tools.len(),
                "capability: too many tools, truncated"
            );
            req.tools.truncate(max);
        }
        if !self.images {
            for block in req.messages.iter_mut().flat_map(|m| m.blocks.iter_mut()) {
                if matches!(block, ContentBlock::Image { .. }) {
                    debug!("capability: images not supported, replaced with a note");
                    *block = ContentBlock::Text("[image omitted]".to_owned());
                }
            }
        }
        if self.system_role == SystemRole::FirstMessage && !req.system.is_empty() {
            debug!("capability: no system role, folded into the first message");
            let system = std::mem::take(&mut req.system);
            match req.messages.first_mut() {
                Some(first) if first.role == MessageRole::User => {
                    first
                        .blocks
                        .insert(0, ContentBlock::Text(format!("{system}\n\n")));
                }
                _ => req
                    .messages
                    .insert(0, Message::text(MessageRole::User, &system)),
            }
        }
        req
    }
}

#[cfg(test)]
mod tests {
    use crate::presets::CompatFlag;
    use crate::provider::{Capabilities, CapabilityOverrides, ChatRequest, SystemRole};
    use crate::types::*;

    fn caps() -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: true,
            prompt_cache: true,
            logprobs: true,
            effort: true,
            images: true,
            system_role: SystemRole::Separate,
            max_tools: None,
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: ModelId("m".into()),
            system: "Be terse.".into(),
            messages: vec![Message {
                role: MessageRole::User,
                blocks: vec![
                    ContentBlock::Text("look".into()),
                    ContentBlock::Image {
                        media_type: "image/png".into(),
                        data: bytes::Bytes::from_static(b"x"),
                    },
                ],
                at: std::time::SystemTime::UNIX_EPOCH,
            }],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 10,
            temperature: None,
            effort: Some(Effort::High),
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        }
    }

    #[test]
    fn degrade_runs_on_the_resolved_per_model_capabilities() {
        let resolved = caps().with(&CapabilityOverrides {
            effort: Some(false),
            images: Some(false),
            ..CapabilityOverrides::default()
        });
        let out = resolved.degrade(req());
        assert_eq!(
            out.effort, None,
            "this model takes no effort even though the provider does"
        );
        assert_eq!(
            out.messages[0].blocks[1],
            ContentBlock::Text("[image omitted]".into())
        );
    }

    #[test]
    fn the_system_role_and_strip_images_flags_become_degrade_steps() {
        let c = caps().with_compat(&[CompatFlag::NoSystemRole, CompatFlag::StripImages]);
        assert_eq!((c.system_role, c.images), (SystemRole::FirstMessage, false));
        let out = c.degrade(req());
        assert!(out.system.is_empty());
        assert_eq!(
            out.messages[0].blocks[0],
            ContentBlock::Text("Be terse.\n\n".into())
        );
        assert!(!out.messages[0]
            .blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. })));
        // Flags that are not about capabilities leave them alone.
        assert_eq!(
            caps().with_compat(&[CompatFlag::NoStore, CompatFlag::NoToolChoice]),
            caps()
        );
    }
}
