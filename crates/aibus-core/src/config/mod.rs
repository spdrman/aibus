//! The config file (07 section 3): every table with its default, so an empty file is valid.
//! Loading, merging and validating live in `load`.

mod load;

pub use load::{find_line, load, parse_file, ConfigFileError, Loaded};

use std::collections::BTreeMap;

use aibus_provider::{AliasTable, RateLimits};
use smol_str::SmolStr;

use crate::agent::MemoryScope;
use crate::credentials::{KeyRef, KEYCHAIN_SERVICE};
use crate::ids::AccountName;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub defaults: Defaults,
    pub models: AliasTable,
    #[serde(rename = "account")]
    pub accounts: Vec<Account>,
    /// `[[provider]]`: a server the presets do not know, or a preset's fields overridden. User
    /// file only, like `[[account]]`: a project must not be able to redirect a key.
    #[serde(rename = "provider")]
    pub providers: Vec<aibus_provider::ProviderDef>,
    pub agents: Agents,
    pub limits: Limits,
    pub router: Router,
    pub advisor: Advisor,
    pub continuity: Continuity,
    pub memory: Memory,
    pub tools: Tools,
    pub hooks: Hooks,
    /// `[mcp.<server>]`: MCP servers started over stdio (01 section 9).
    pub mcp: BTreeMap<String, McpServer>,
    pub persist: Persist,
    pub ui: Ui,
    /// Action name to key, overriding the defaults (05 section 4).
    pub keys: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub account: Option<String>,
    pub model: Option<String>,
    pub advisor_model: Option<String>,
    pub router_model: Option<String>,
    pub swarm_model: Option<String>,
    pub permission_mode: String,
    pub effort: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            account: None,
            model: None,
            advisor_model: None,
            router_model: None,
            swarm_model: None,
            permission_mode: "default".into(),
            effort: "high".into(),
        }
    }
}

/// `[[account]]`. There is no field for a key: `auth` names where the credential lives.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub name: String,
    pub provider: String,
    pub label: Option<String>,
    pub base_url: Option<String>,
    /// The wire dialect for providers with more than one: `openai` takes "chat" (the default)
    /// or "responses".
    pub api: Option<String>,
    pub default_model: Option<String>,
    /// `sonnet = "claude-sonnet-4-5"`, tried after the global `[models]` table.
    pub aliases: AliasTable,
    pub rate: RateLimits,
    /// "cheap", "eu": what `prefer_tags` in a selection matches.
    pub tags: Vec<SmolStr>,
    pub enabled: bool,
    pub auth: Auth,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    /// The OS keychain.
    pub keyring: bool,
    /// The environment variable that holds the key.
    pub env: Option<String>,
    /// A `0600` file that holds the key.
    pub file: Option<String>,
    /// A local server that needs no key.
    pub none: bool,
    /// OAuth tokens in the keychain, refreshed before use. Off unless asked for.
    pub oauth: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Agents {
    pub extra_dirs: Vec<String>,
    pub trusted_dirs: Vec<String>,
    pub reload_on_change: bool,
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            extra_dirs: Vec::new(),
            trusted_dirs: Vec::new(),
            reload_on_change: true,
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_turns: u32,
    pub max_tokens_total: u64,
    pub max_cost_usd: f64,
    pub max_spawn_depth: u8,
    pub max_concurrent_runs: u32,
    pub max_runs_per_session: u32,
    pub result_max_bytes: usize,
    pub transcript_result_max_bytes: usize,
    pub auto_compact_pct: u8,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_turns: 50,
            max_tokens_total: 2_000_000,
            max_cost_usd: 20.0,
            max_spawn_depth: 3,
            max_concurrent_runs: 8,
            max_runs_per_session: 500,
            result_max_bytes: 32_768,
            transcript_result_max_bytes: 65_536,
            auto_compact_pct: 80,
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Router {
    pub enabled: bool,
    pub threshold: f64,
    pub max_per_turn: u32,
    pub label: String,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.85,
            max_per_turn: 10,
            label: "ROUTER".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Advisor {
    pub enabled: bool,
    pub checkpoints: Vec<String>,
    pub packet_max_tokens: u64,
}

impl Default for Advisor {
    fn default() -> Self {
        Self {
            enabled: true,
            checkpoints: vec![
                "before_plan".into(),
                "same_error_x2".into(),
                "before_done".into(),
            ],
            packet_max_tokens: 400_000,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Continuity {
    pub dir: String,
    pub auto_handoff_on_compact: bool,
    pub handoff_on_end: bool,
    pub extract_learnings: bool,
    pub ask_outcome: bool,
    /// Where the session's topic comes from: "newest" (the most recently modified ledger) or
    /// "branch" (the git branch), Q10. `--topic` beats both.
    pub topic: String,
}

impl Default for Continuity {
    fn default() -> Self {
        Self {
            dir: "thoughts".into(),
            auto_handoff_on_compact: true,
            handoff_on_end: true,
            extract_learnings: true,
            ask_outcome: true,
            topic: "newest".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub scopes: Vec<String>,
    pub recall_k: u32,
    /// Extract learnings into `LEARNINGS.md` when a session ends (Q11).
    pub extract_on_end: bool,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            scopes: vec!["user".into(), "project".into()],
            recall_k: 5,
            extract_on_end: true,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub web: bool,
    pub bash_timeout_secs: u32,
    pub bash_max_output_bytes: usize,
    pub read_max_lines: u32,
    /// Patterns (`bash(cargo *)`) that skip the permission prompt, like `--allow`.
    pub allow: Vec<String>,
    /// Globs added to `tools/shell_deny.txt`; a match is refused before any prompt.
    pub deny_patterns: Vec<String>,
    /// OS sandbox for `bash` (09 section 2). Off unless asked for.
    pub sandbox: SandboxKind,
    /// With a sandbox on: `false` cuts the network inside it.
    pub sandbox_network: bool,
}

/// `[tools] sandbox`: what `bash` runs inside, if anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SandboxKind {
    #[default]
    #[serde(rename = "none")]
    None,
    /// `bubblewrap`, Linux.
    #[serde(rename = "bwrap")]
    Bwrap,
    /// `sandbox-exec`, macOS.
    #[serde(rename = "sandbox-exec")]
    SandboxExec,
}

impl SandboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxKind::None => "none",
            SandboxKind::Bwrap => "bwrap",
            SandboxKind::SandboxExec => "sandbox-exec",
        }
    }
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            web: false,
            bash_timeout_secs: 120,
            bash_max_output_bytes: 262_144,
            read_max_lines: 2000,
            allow: Vec::new(),
            deny_patterns: Vec::new(),
            sandbox: SandboxKind::None,
            sandbox_network: true,
        }
    }
}

/// `[mcp.<server>]`: a program that speaks MCP on its stdin and stdout. Its tools show up as
/// `mcp:<server>:<tool>`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
    /// Added to the scrubbed environment the server starts with. The config holds no secrets, so
    /// a key goes in through `pass_env` instead.
    pub env: BTreeMap<String, String>,
    /// Variables of aibus's own environment the server may see although they look secret.
    pub pass_env: Vec<String>,
    /// How long a call may take; 60 s when unset.
    pub timeout_secs: Option<u32>,
}

/// `[hooks]`: the one `pass_env` list, and every other key is an event name (01 section 6.1).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hooks {
    /// Variables that stay in a hook's (and `bash`'s) environment although their names look
    /// secret (09 section 1).
    pub pass_env: Vec<String>,
    /// Event name to its hooks. Project hooks come after the user's.
    #[serde(flatten)]
    pub events: BTreeMap<String, Vec<Hook>>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hook {
    pub matcher: Option<String>,
    /// The command line of a `command` hook.
    pub command: String,
    pub timeout: Option<u32>,
    /// `type = "command"` (the default), `"prompt"` or `"http"`.
    #[serde(rename = "type")]
    pub kind: crate::agent::HookKind,
    /// What a `prompt` hook asks the model.
    pub prompt: Option<String>,
    /// Where an `http` hook POSTs.
    pub url: Option<String>,
}

impl Hook {
    /// The runnable form: the text that matters for the hook's kind. `Err` says what is missing.
    pub fn spec(&self) -> Result<crate::agent::HookSpec, &'static str> {
        use crate::agent::HookKind;
        let text = match self.kind {
            HookKind::Command => Some(&self.command),
            HookKind::Prompt => self.prompt.as_ref(),
            HookKind::Http => self.url.as_ref(),
        };
        match (text.filter(|t| !t.trim().is_empty()), self.kind) {
            (Some(t), HookKind::Http) if !(t.starts_with("http://") || t.starts_with("https://")) => {
                Err("an http hook needs an http:// or https:// url")
            }
            (Some(t), _) => Ok(crate::agent::HookSpec {
                matcher: self.matcher.clone(),
                command: t.clone(),
                timeout_secs: self.timeout,
                kind: self.kind,
            }),
            (None, HookKind::Command) => Err("a command hook needs a command"),
            (None, HookKind::Prompt) => Err("a prompt hook needs a prompt"),
            (None, HookKind::Http) => Err("an http hook needs a url"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Persist {
    pub snapshot_every_commits: u32,
    pub snapshot_every_secs: u32,
    pub commit_history_cap: u32,
    pub session_retention_days: u32,
    pub log_files_kept: u32,
    /// Thinking blocks go to transcripts only when this is on (09 section 4).
    pub store_thinking: bool,
}

impl Default for Persist {
    fn default() -> Self {
        Self {
            snapshot_every_commits: 64,
            snapshot_every_secs: 30,
            commit_history_cap: 4096,
            session_retention_days: 30,
            log_files_kept: 7,
            store_thinking: false,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub theme: String,
    pub ascii: bool,
    pub mouse: bool,
    pub hints: bool,
    pub log_rows: u16,
    pub tick_hz: u32,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "recording".into(),
            ascii: false,
            mouse: true,
            hints: true,
            log_rows: 4,
            tick_hz: 30,
        }
    }
}

impl Default for Account {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: String::new(),
            label: None,
            base_url: None,
            api: None,
            default_model: None,
            aliases: AliasTable::new(),
            rate: RateLimits::default(),
            tags: Vec::new(),
            enabled: true,
            auth: Auth::default(),
        }
    }
}

impl Account {
    pub fn account_name(&self) -> AccountName {
        AccountName(self.name.as_str().into())
    }

    /// Where the key lives, or `None` for a server that needs none. With no `auth` at all the
    /// key is read from `AIBUS_ACCOUNT_<NAME>_KEY` (06 section 7).
    pub fn key_ref(&self) -> Option<KeyRef> {
        let a = &self.auth;
        if a.none {
            None
        } else if a.keyring || a.oauth {
            Some(KeyRef::Keyring {
                service: KEYCHAIN_SERVICE.into(),
                name: self.account_name(),
            })
        } else if let Some(path) = &a.file {
            Some(KeyRef::File { path: path.into() })
        } else if let Some(var) = &a.env {
            Some(KeyRef::Env { var: var.clone() })
        } else {
            let name = self.name.to_uppercase().replace('-', "_");
            Some(KeyRef::Env {
                var: format!("AIBUS_ACCOUNT_{name}_KEY"),
            })
        }
    }
}

impl Config {
    /// What is wrong with the values, as `(dotted path, message)`; the first is the one the user
    /// sees. A deserialised file can still hold nonsense (a threshold of 7, two accounts with one
    /// name).
    pub fn problems(&self) -> Vec<(String, String)> {
        const PERMISSION_MODES: [&str; 5] =
            ["default", "accept-edits", "dont-ask", "bypass", "plan"];
        const EFFORTS: [&str; 4] = ["low", "medium", "high", "max"];
        let mut out = Vec::new();
        let mut bad = |path: &str, msg: String| out.push((path.to_owned(), msg));
        if !PERMISSION_MODES.contains(&self.defaults.permission_mode.as_str()) {
            bad(
                "defaults.permission_mode",
                format!("expected one of {}", PERMISSION_MODES.join(", ")),
            );
        }
        if !EFFORTS.contains(&self.defaults.effort.as_str()) {
            bad(
                "defaults.effort",
                format!("expected one of {}", EFFORTS.join(", ")),
            );
        }
        if !(0.0..=1.0).contains(&self.router.threshold) {
            bad("router.threshold", "expected a number from 0 to 1".into());
        }
        if self.limits.auto_compact_pct == 0 || self.limits.auto_compact_pct > 100 {
            bad("limits.auto_compact_pct", "expected 1 to 100".into());
        }
        if self.limits.max_turns == 0 {
            bad("limits.max_turns", "must be at least 1".into());
        }
        for event in self.hooks.events.keys() {
            if crate::msg::HookEvent::parse(event).is_none() {
                bad(&format!("hooks.{event}"), "not a hook event".into());
            }
        }
        for (event, list) in &self.hooks.events {
            for (i, h) in list.iter().enumerate() {
                if let Err(why) = h.spec() {
                    bad(&format!("hooks.{event}[{}]", i + 1), why.into());
                }
            }
        }
        for (name, server) in &self.mcp {
            let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !ok {
                bad(&format!("mcp.{name}"), "a server name is letters, digits, `_` and `-`".into());
            } else if server.command.trim().is_empty() {
                bad(&format!("mcp.{name}.command"), "a server needs a command".into());
            }
        }
        if !["newest", "branch"].contains(&self.continuity.topic.as_str()) {
            bad("continuity.topic", "expected newest or branch".into());
        }
        if let Some(s) = self.memory.scopes.iter().find(|s| MemoryScope::parse(s).is_none()) {
            bad("memory.scopes", format!("{s}: expected user, project or local"));
        }
        if self.memory.recall_k == 0 {
            bad("memory.recall_k", "must be at least 1".into());
        }
        if self.ui.tick_hz == 0 {
            bad("ui.tick_hz", "must be at least 1".into());
        }
        let mut defined = std::collections::BTreeSet::new();
        for d in &self.providers {
            if !defined.insert(d.id.as_str()) {
                bad("provider.id", format!("provider {} is defined twice", d.id));
            } else if let Err(why) = aibus_provider::presets::resolve(&d.id, Some(d)) {
                bad(if why.contains(": headers:") { "provider.headers" } else { "provider" }, why);
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for a in &self.accounts {
            if a.name.is_empty() || a.provider.is_empty() {
                bad("account.name", "every account needs a name and a provider".into());
            } else if !seen.insert(a.name.as_str()) {
                bad("account.name", format!("account {} is defined twice", a.name));
            }
            let sources = [
                a.auth.keyring,
                a.auth.env.is_some(),
                a.auth.file.is_some(),
                a.auth.none,
            ];
            if sources.into_iter().filter(|s| *s).count() > 1 {
                bad(
                    "account.auth",
                    format!("account {}: pick one of keyring, env, file, none", a.name),
                );
            }
            if a.rate.max_concurrent == 0 {
                bad("account.rate", format!("account {}: max_concurrent must be at least 1", a.name));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_document_is_the_default_config() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
        assert!(c.problems().is_empty());
    }

    #[test]
    fn the_defaults_are_the_ones_in_the_spec() {
        let c = Config::default();
        assert_eq!(c.limits.max_turns, 50);
        assert_eq!(c.limits.max_tokens_total, 2_000_000);
        assert_eq!(c.limits.result_max_bytes, 32768);
        assert_eq!(c.router.threshold, 0.85);
        assert_eq!(c.advisor.packet_max_tokens, 400_000);
        assert_eq!(c.continuity.dir, "thoughts");
        assert_eq!(c.memory.recall_k, 5);
        assert_eq!(c.tools.bash_timeout_secs, 120);
        assert_eq!(c.persist.snapshot_every_commits, 64);
        assert_eq!(c.ui.tick_hz, 30);
        assert!(!c.persist.store_thinking);
        // The sandbox is never on by default in v1.
        assert_eq!(c.tools.sandbox, SandboxKind::None);
        assert!(c.tools.sandbox_network);
    }

    #[test]
    fn the_sandbox_names_are_the_three_spellings_and_nothing_else() {
        for (text, want) in [("none", SandboxKind::None), ("bwrap", SandboxKind::Bwrap), ("sandbox-exec", SandboxKind::SandboxExec)] {
            let t: Tools = toml::from_str(&format!("sandbox = \"{text}\"")).unwrap();
            assert_eq!((t.sandbox, t.sandbox.as_str()), (want, text));
        }
        assert!(toml::from_str::<Tools>("sandbox = \"docker\"").is_err());
    }

    #[test]
    fn a_provider_table_takes_the_closed_compat_list_and_default_capabilities() {
        use aibus_provider::presets::{CompatFlag, MaxTokensField};
        let c: Config = toml::from_str(
            r#"
[[provider]]
id = "gw"
api = "openai-chat"
base_url = "http://10.0.0.5:8000/v1"
compat = ["no_store", { max_tokens_field = "max_completion_tokens" }]
default_capabilities = { logprobs = false, max_tools = 16 }
"#,
        )
        .unwrap();
        let d = &c.providers[0];
        assert_eq!(d.compat, Some(vec![CompatFlag::NoStore, CompatFlag::MaxTokensField(MaxTokensField::MaxCompletionTokens)]));
        assert_eq!((d.default_capabilities.logprobs, d.default_capabilities.max_tools), (Some(false), Some(16)));
        assert!(c.problems().is_empty());
        // A flag nobody named is a parse error, so a typo cannot pass for a quirk.
        assert!(toml::from_str::<Config>("[[provider]]\nid = \"gw\"\ncompat = [\"no_such_quirk\"]\n").is_err());
    }
}
