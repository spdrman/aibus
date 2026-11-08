//! The config file (07 section 3): every table with its default, so an empty file is valid.
//! Loading, merging and validating live in `load`.

mod load;

pub use load::{find_line, load, parse_file, ConfigFileError, Loaded};

use std::collections::BTreeMap;

use aibus_provider::AliasTable;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub defaults: Defaults,
    pub models: AliasTable,
    #[serde(rename = "account")]
    pub accounts: Vec<Account>,
    pub agents: Agents,
    pub limits: Limits,
    pub router: Router,
    pub advisor: Advisor,
    pub continuity: Continuity,
    pub memory: Memory,
    pub tools: Tools,
    /// Event name to its hooks. Project hooks come after the user's.
    pub hooks: BTreeMap<String, Vec<Hook>>,
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
    pub default_model: Option<String>,
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
}

impl Default for Continuity {
    fn default() -> Self {
        Self {
            dir: "thoughts".into(),
            auto_handoff_on_compact: true,
            handoff_on_end: true,
            extract_learnings: true,
            ask_outcome: true,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub scopes: Vec<String>,
    pub recall_k: u32,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            scopes: vec!["user".into(), "project".into()],
            recall_k: 5,
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
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            web: false,
            bash_timeout_secs: 120,
            bash_max_output_bytes: 262_144,
            read_max_lines: 2000,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hook {
    pub matcher: Option<String>,
    pub command: String,
    pub timeout: Option<u32>,
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
            default_model: None,
            enabled: true,
            auth: Auth::default(),
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
        if self.ui.tick_hz == 0 {
            bad("ui.tick_hz", "must be at least 1".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for a in &self.accounts {
            if a.name.is_empty() || a.provider.is_empty() {
                bad("account.name", "every account needs a name and a provider".into());
            } else if !seen.insert(a.name.as_str()) {
                bad("account.name", format!("account {} is defined twice", a.name));
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
    }
}
