//! The manifest contract (`aibus-extension.toml`, schema version 1). Strict: an unknown key, a
//! wrong type or a value outside the rules below is a refusal, never a guess. The JSON schema in
//! `docs/schemas/extension/manifest-v1.json` says the same in the schema's own language.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "aibus-extension.toml";
/// A manifest larger than this is not read.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// The only schema version this host reads.
pub const SCHEMA_VERSION: &str = "1";

/// The capability vector of spec 3, plus `hook_channel` (A1.5 amendment E5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    LoginStatus,
    OneShot,
    StructuredEvents,
    NativeChildLineage,
    ToolEvents,
    UsageReport,
    SessionResume,
    InteractiveApproval,
    Cancel,
    WorkspaceControl,
    Headless,
    SupportedOs,
    HookChannel,
}

impl Capability {
    pub const ALL: [Capability; 13] = [
        Self::LoginStatus,
        Self::OneShot,
        Self::StructuredEvents,
        Self::NativeChildLineage,
        Self::ToolEvents,
        Self::UsageReport,
        Self::SessionResume,
        Self::InteractiveApproval,
        Self::Cancel,
        Self::WorkspaceControl,
        Self::Headless,
        Self::SupportedOs,
        Self::HookChannel,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    /// Decided at install and at launch from what the real CLI's `--help` lists.
    ProbeRequired,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageQuality {
    Reported,
    Estimated,
    Unknown,
}

/// Who reads the CLI's events. The built-in engine is the host's own Claude parser (stateful: it
/// keeps tool-use to agent maps, dedupes across channels and has a `finish` step, which a
/// declarative manifest cannot express: amendment E1). The declarative engine is for CLIs whose
/// events are one line, one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    #[serde(rename = "builtin:claude-stream-json")]
    ClaudeStreamJson,
    #[serde(rename = "declarative-jsonl")]
    DeclarativeJsonl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    MessageChunk,
    ToolStarted,
    ToolCompleted,
    ToolFailed,
    UsageReported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// JSON pointer to a string in the line (`/type`).
    pub pointer: String,
    pub equals: String,
    pub kind: RuleKind,
    /// Pointer to the tool use id, for the tool kinds.
    pub id_pointer: Option<String>,
    /// Pointers to token counts, for `usage_reported`. A missing number stays `unknown`.
    pub tokens_in_pointer: Option<String>,
    pub tokens_out_pointer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Normalizer {
    pub engine: Engine,
    #[serde(default)]
    pub rule: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compat {
    /// The SDK protocol versions this extension speaks, a range such as `>=1.0.0, <2.0.0`.
    pub protocol: String,
    pub os: Vec<String>,
    /// The CLI versions it was written for, a range.
    pub cli_versions: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cli {
    /// File names only (`claude`), never paths. The host finds them on its own search path.
    pub executable_candidates: Vec<String>,
    /// Where the vendor documents the CLI, for provenance.
    pub vendor_urls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// `vendor_cli`: the CLI owns its sign-in.
    pub owner: String,
    /// `forbidden`: aibus never reads, copies or stores the CLI's credentials.
    pub credential_access: String,
    pub login_argv: Vec<String>,
    pub status_argv: Vec<String>,
}

/// A flag added only when the probe found it and the capability it serves is wanted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionalFlag {
    pub flag: String,
    pub capability: Capability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    /// Static tokens after the executable. No placeholder: the prompt goes on stdin (amendment E2).
    pub argv: Vec<String>,
    /// `stdin`, the only way a prompt reaches the CLI.
    pub prompt: String,
    /// `jsonl`.
    pub stdout: String,
    /// Environment names passed through from the host's environment, on top of the host's own
    /// allowlist (amendment E3). Credential-shaped names are refused.
    #[serde(default)]
    pub env_allow: Vec<String>,
    #[serde(default)]
    pub optional: Vec<OptionalFlag>,
}

/// What the extension asks of the host's limits. The host's ceiling wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requested {
    pub wall_time_secs: Option<u64>,
    pub max_stdout_bytes: Option<u64>,
    pub max_line_bytes: Option<u64>,
    pub max_stderr_bytes: Option<u64>,
    pub lines_per_second: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub capability: Capability,
    /// Flags `--help` must list.
    pub flags: Vec<String>,
    /// The CLI version from which the capability exists.
    pub min_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// The extension's evidence is better with hook payloads. The host has no live way to receive
    /// them yet (A1.5 amendment E4), so a negotiation says `hook_channel` is unsupported and the
    /// stream is the only evidence.
    pub needs_delivery: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub usage_quality: UsageQuality,
}

/// The permission declaration. Every field has one acceptable value; a manifest that asks for
/// more is refused, so a reviewer reads one block and a later version that changes it is an
/// escalation the user has to accept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    pub spawn_only_declared_cli: bool,
    pub read_provider_credentials: bool,
    pub external_network_from_parser: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInfo {
    pub owner: String,
    /// `YYYY-MM-DD`.
    pub review_date: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixtures {
    /// Recorded stdout streams, each with a sibling `<name>.expected.json`.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    pub upgrade: String,
    pub rollback: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub name: String,
    /// `official_cli`.
    pub kind: String,
    pub provider: String,
    pub license: String,
    pub source_url: String,
    pub publisher: Publisher,
    pub compat: Compat,
    pub cli: Cli,
    pub auth: Auth,
    pub execution: Execution,
    pub limits: Option<Requested>,
    pub normalizer: Normalizer,
    pub capabilities: BTreeMap<Capability, Support>,
    #[serde(default)]
    pub probe: Vec<Probe>,
    pub hooks: Option<Hooks>,
    pub runtime: Runtime,
    pub permissions: Permissions,
    pub policy: PolicyInfo,
    pub fixtures: Fixtures,
    pub notes: Notes,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the manifest is {0} bytes, the most read is {MAX_MANIFEST_BYTES}")]
    TooLarge(usize),
    #[error("the manifest is not valid TOML for schema 1: {0}")]
    Syntax(String),
    #[error("{field}: {why}")]
    Invalid { field: String, why: String },
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        todo!("{text}")
    }
}

/// What `new` asks for that `old` did not: a new executable name, environment name, optional flag,
/// argv token, capability, engine, or larger requested limit. An update that returns a non-empty
/// list is a capability escalation: the user has to accept it by name, and nothing changes
/// until then.
pub fn escalations(old: &Manifest, new: &Manifest) -> Vec<String> {
    todo!("{old:?} {new:?}")
}
