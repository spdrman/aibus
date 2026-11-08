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
