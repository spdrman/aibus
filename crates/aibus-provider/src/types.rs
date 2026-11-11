//! Provider-neutral value types shared by the trait, the engine and the fake (02 section 5).
//!
//! They live here because `aibus-core` depends on this crate and not the other way round.

use std::collections::BTreeMap;
use std::fmt;
use std::time::SystemTime;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use ulid::Ulid;

/// Stable provider id used in config: "anthropic", "openai", "fake".
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ProviderId(pub &'static str);

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The name of one login, "anthropic-work".
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AccountName(pub SmolStr);

impl fmt::Display for AccountName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A provider-native model id.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ModelId(pub SmolStr);

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ToolCallId(pub Ulid);

/// How an agent definition or the CLI names a model.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ModelRef {
    Alias(SmolStr),
    Full(ModelId),
    Inherit,
}

/// Per-account and global alias table: `sonnet = "claude-sonnet-4-5"`.
pub type AliasTable = BTreeMap<SmolStr, SmolStr>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Effort {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThinkingBudget {
    pub tokens: u32,
}

/// Where a cache breakpoint may go.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub enum CacheHint {
    #[default]
    None,
    AfterSystem,
    AfterTools,
    LastMessage,
}

#[derive(Clone, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(SmolStr),
}

/// Name, description and JSON schema of one tool, as the model sees it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: SmolStr,
    pub description: String,
    pub schema: serde_json::Value,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    /// `signature` is what the API needs back to accept the block in a later request; for a
    /// redacted block `text` carries the opaque payload instead.
    Thinking {
        text: String,
        redacted: bool,
        #[serde(default)]
        signature: Option<String>,
    },
    ToolUse {
        id: ToolCallId,
        name: SmolStr,
        input: serde_json::Value,
        /// Gemini's `thoughtSignature` for this call; the next request has to send it back unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolResult {
        id: ToolCallId,
        content: String,
        is_error: bool,
    },
    Image {
        media_type: SmolStr,
        data: Bytes,
    },
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    pub at: SystemTime,
}

impl Message {
    pub fn text(role: MessageRole, text: &str) -> Self {
        Self {
            role,
            blocks: vec![ContentBlock::Text(text.to_owned())],
            at: SystemTime::UNIX_EPOCH,
        }
    }

    /// Text blocks and tool results joined with newlines, for prompt matching and folding.
    pub fn plain_text(&self) -> String {
        let parts: Vec<&str> = self
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        parts.join("\n")
    }
}

/// Where a `cost_usd` came from, so a dashboard can mark an estimate differently from a bill.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// Tokens times the model table's price.
    #[default]
    Table,
    /// The gateway reported the cost in the response (OpenRouter's `usage.cost`).
    Provider,
}

#[derive(Clone, Copy, PartialEq, Default, Debug, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Part of `output_tokens` the model spent thinking; `None` when the provider does not say.
    /// Charged as output, which is what the providers do.
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    pub cost_usd: f64,
    /// Absent from stored usage and from `Table` usage, so old graphs and snapshots read and
    /// write as before.
    #[serde(default, skip_serializing_if = "CostSource::is_table")]
    pub cost_source: CostSource,
}

impl CostSource {
    fn is_table(&self) -> bool {
        *self == Self::Table
    }
}

/// Run and session ids for provider-side tracing. Never carries secrets.
#[derive(Clone, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub struct RequestMetadata {
    pub run_id: Option<String>,
    pub session_id: Option<String>,
}
