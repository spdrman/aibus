//! Model tables shared by the real providers (04 section 2, Q20, Q25): a built-in table, the
//! list the API last returned (`~/.cache/aibus/models/<provider>.json`, kept for 7 days) and the
//! user's overrides (`~/.config/aibus/providers/<provider>/models.json`), later layers winning.
//!
//! All three files have one shape:
//!
//! ```json
//! { "fetched_at": 1730000000,
//!   "models": [{ "id": "x", "context_window": 1, "max_output": 1, "family": "f",
//!                "deprecated": false, "price": { "input_per_mtok": 1.0, "output_per_mtok": 2.0 } }],
//!   "families": { "f": 3.5 } }
//! ```
//!
//! `families` maps a model family to its characters per token. A model without `price` has
//! none: its cost shows as `n/a`, it is never guessed.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::error::ConfigError;
use crate::provider::{ModelInfo, Price, ProviderConfig};
use crate::tokens::CHARS_PER_TOKEN;
use crate::types::{AliasTable, ModelId, ModelRef};

/// How long a refreshed list stays good.
pub const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ModelEntry {
    #[serde(flatten)]
    pub info: ModelInfo,
    #[serde(default)]
    pub price: Option<Price>,
    /// The model accepts the `effort` parameter.
    #[serde(default)]
    pub effort: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fetched_at: Option<u64>,
    #[serde(default)]
    models: Vec<ModelEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    families: BTreeMap<SmolStr, f64>,
}

pub struct ModelTable {
    entries: Vec<ModelEntry>,
    families: BTreeMap<SmolStr, f64>,
    /// A full id with one of these prefixes resolves even when the table has not heard of it,
    /// so a model released after the table was written still works (without a price).
    prefixes: Vec<String>,
    /// An alias target resolves whatever it is: a local server names its own models.
    open_aliases: bool,
}

fn read(path: &Path) -> Result<Option<File>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl ModelTable {
    /// The built-in `builtin` table, then the cache, then the user's overrides. A broken
    /// override is a config error that names the file; a broken cache is ignored, since the
    /// next refresh rewrites it.
    pub fn load(
        provider: &str,
        builtin: &str,
        cfg: &ProviderConfig,
        prefixes: &[&str],
        open_aliases: bool,
    ) -> Result<Self, ConfigError> {
        let bad = |e: String| ConfigError::Invalid(provider.to_owned(), e);
        let base: File = serde_json::from_str(builtin).map_err(|e| bad(format!("built-in table: {e}")))?;
        let mut t = Self {
            entries: Vec::new(),
            families: BTreeMap::new(),
            prefixes: prefixes.iter().map(|p| (*p).to_owned()).collect(),
            open_aliases,
        };
        t.layer(base);
        if let Some(dir) = &cfg.cache_dir {
            if let Ok(Some(cached)) = read(&dir.join(format!("{provider}.json"))) {
                t.layer(cached);
            }
        }
        if let Some(dir) = &cfg.overrides_dir {
            if let Some(over) = read(&dir.join(provider).join("models.json")).map_err(bad)? {
                t.layer(over);
            }
        }
        Ok(t)
    }

    fn layer(&mut self, f: File) {
        for e in f.models {
            match self.entries.iter_mut().find(|x| x.info.id == e.info.id) {
                Some(slot) => *slot = e,
                None => self.entries.push(e),
            }
        }
        self.families.extend(f.families);
    }

    pub fn models(&self) -> Vec<ModelInfo> {
        self.entries.iter().map(|e| e.info.clone()).collect()
    }

    /// The entry for `id`, or for the model a dated snapshot of it (`x-20250929`,
    /// `x-2025-09-29`) is a copy of.
    pub fn entry(&self, id: &ModelId) -> Option<&ModelEntry> {
        // `name-20250929` and `name-2025-09-29` are snapshots of `name`.
        fn undated(id: &str) -> &str {
            let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
            if let Some((head, d)) = id.rsplit_once('-') {
                if digits(d, 8) {
                    return head;
                }
            }
            let mut it = id.rsplitn(4, '-');
            if let (Some(d), Some(m), Some(y), Some(head)) = (it.next(), it.next(), it.next(), it.next()) {
                if digits(d, 2) && digits(m, 2) && digits(y, 4) {
                    return head;
                }
            }
            id
        }
        let find = |s: &str| self.entries.iter().find(|e| e.info.id.0 == s);
        find(&id.0).or_else(|| find(undated(&id.0)))
    }

    pub fn price(&self, id: &ModelId) -> Option<Price> {
        self.entry(id)?.price
    }

    /// Characters per token for this model's family (Q20).
    pub fn chars_per_token(&self, id: &ModelId) -> f64 {
        self.entry(id)
            .and_then(|e| self.families.get(&e.info.family))
            .copied()
            .unwrap_or(CHARS_PER_TOKEN)
    }

    /// The native id for `r`, or `None` when this provider does not serve it.
    pub fn resolve(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId> {
        let (id, aliased) = match r {
            ModelRef::Alias(a) => (aliases.get(a)?.clone(), true),
            ModelRef::Full(id) => (id.0.clone(), false),
            ModelRef::Inherit => return None,
        };
        let id = ModelId(id);
        let known = self.entry(&id).is_some()
            || self.prefixes.iter().any(|p| id.0.starts_with(p.as_str()))
            || (aliased && self.open_aliases);
        known.then_some(id)
    }

    /// Takes the list the API returned: models the table knows keep their sizes and prices,
    /// new ones get `defaults` for the sizes and no price. Writes the cache when `cache_dir`
    /// is set (a failed write only loses the cache) and returns the new list.
    pub fn refreshed(
        &mut self,
        provider: &str,
        listed: Vec<ModelId>,
        defaults: impl Fn(&ModelId) -> ModelInfo,
        cache_dir: Option<&Path>,
        now: SystemTime,
    ) -> Vec<ModelInfo> {
        let entries: Vec<ModelEntry> = listed
            .iter()
            .map(|id| {
                self.entry(id).cloned().unwrap_or_else(|| ModelEntry {
                    info: defaults(id),
                    price: None,
                    effort: false,
                })
            })
            .collect();
        self.layer(File {
            models: entries.clone(),
            ..File::default()
        });
        if let Some(dir) = cache_dir {
            let file = File {
                fetched_at: now.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()),
                models: entries.clone(),
                families: BTreeMap::new(),
            };
            // Atomic enough: written beside, then renamed over.
            let path = dir.join(format!("{provider}.json"));
            let tmp = dir.join(format!(".{provider}.json.tmp"));
            let wrote = std::fs::create_dir_all(dir)
                .and_then(|()| std::fs::write(&tmp, serde_json::to_vec_pretty(&file).unwrap_or_default()))
                .and_then(|()| std::fs::rename(&tmp, &path));
            if let Err(e) = wrote {
                tracing::debug!(provider, error = %e, "model cache not written");
            }
        }
        entries.into_iter().map(|e| e.info).collect()
    }
}

/// How old `<cache_dir>/<provider>.json` is, or `None` when there is no readable cache.
pub fn cache_age(cache_dir: &Path, provider: &str, now: SystemTime) -> Option<Duration> {
    let file = read(&cache_dir.join(format!("{provider}.json"))).ok()??;
    let at = UNIX_EPOCH + Duration::from_secs(file.fetched_at?);
    Some(now.duration_since(at).unwrap_or_default())
}

/// A missing, unreadable or older-than-`CACHE_TTL` cache wants a refresh.
pub fn cache_is_stale(cache_dir: &Path, provider: &str, now: SystemTime) -> bool {
    cache_age(cache_dir, provider, now).is_none_or(|age| age > CACHE_TTL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Capabilities, CapabilityOverrides, SystemRole};

    const BUILTIN: &str = r#"{
        "models": [
          {"id": "big", "context_window": 1000, "max_output": 100, "family": "f", "deprecated": false,
           "price": {"input_per_mtok": 3.0, "output_per_mtok": 15.0, "cache_read_per_mtok": 0.3}},
          {"id": "old", "context_window": 10, "max_output": 5, "family": "g", "deprecated": true}
        ],
        "families": {"f": 3.0}
    }"#;

    fn table(cfg: &ProviderConfig) -> ModelTable {
        ModelTable::load("p", BUILTIN, cfg, &["pre-"], false).unwrap()
    }

    fn id(s: &str) -> ModelId {
        ModelId(s.into())
    }

    #[test]
    fn the_builtin_table_gives_sizes_prices_and_family_ratios() {
        let t = table(&ProviderConfig::default());
        assert_eq!(t.models().len(), 2);
        assert_eq!(t.price(&id("big")).unwrap().output_per_mtok, 15.0);
        assert_eq!(t.price(&id("old")), None, "no price listed means none, not zero");
        assert_eq!(t.chars_per_token(&id("big")), 3.0);
        assert_eq!(t.chars_per_token(&id("old")), 4.0);
        assert_eq!(t.chars_per_token(&id("nope")), 4.0);
    }

    #[test]
    fn a_dated_snapshot_shares_its_models_entry() {
        let t = table(&ProviderConfig::default());
        assert_eq!(t.price(&id("big-20250929")).unwrap().input_per_mtok, 3.0);
        assert_eq!(t.price(&id("big-2025-09-29")).unwrap().input_per_mtok, 3.0);
        assert_eq!(t.price(&id("big-2025")), None);
        assert_eq!(t.price(&id("other-20250929")), None);
    }

    #[test]
    fn overrides_win_over_the_cache_which_wins_over_the_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, over) = (dir.path().join("cache"), dir.path().join("providers"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(over.join("p")).unwrap();
        std::fs::write(
            cache.join("p.json"),
            r#"{"fetched_at": 1, "models": [{"id": "big", "context_window": 2000, "max_output": 100, "family": "f", "deprecated": false}]}"#,
        )
        .unwrap();
        let cfg = ProviderConfig {
            cache_dir: Some(cache),
            overrides_dir: Some(over.clone()),
            ..ProviderConfig::default()
        };
        assert_eq!(table(&cfg).models()[0].context_window, 2000);

        std::fs::write(
            over.join("p/models.json"),
            r#"{"models": [{"id": "big", "context_window": 3000, "max_output": 100, "family": "f", "deprecated": false}], "families": {"f": 2.5}}"#,
        )
        .unwrap();
        let t = table(&cfg);
        assert_eq!(t.models()[0].context_window, 3000);
        assert_eq!(t.chars_per_token(&id("big")), 2.5);
        assert_eq!(t.models().len(), 2);
    }

    #[test]
    fn a_broken_override_names_its_file_and_a_broken_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("p.json"), "{ not json").unwrap();
        let cfg = ProviderConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..ProviderConfig::default()
        };
        assert_eq!(table(&cfg).models().len(), 2);

        std::fs::create_dir_all(dir.path().join("p")).unwrap();
        std::fs::write(dir.path().join("p/models.json"), "{ not json").unwrap();
        let cfg = ProviderConfig {
            overrides_dir: Some(dir.path().to_owned()),
            ..ProviderConfig::default()
        };
        let Err(ConfigError::Invalid(p, msg)) = ModelTable::load("p", BUILTIN, &cfg, &[], false) else {
            panic!("expected a config error");
        };
        assert_eq!(p, "p");
        assert!(msg.contains("models.json"), "{msg}");
    }

    #[test]
    fn resolve_takes_known_ids_prefixes_and_open_aliases() {
        let t = table(&ProviderConfig::default());
        let aliases: AliasTable = [("fast".into(), "big".into()), ("odd".into(), "weird".into())].into();
        let full = |s: &str| ModelRef::Full(id(s));
        assert_eq!(t.resolve(&full("big"), &aliases), Some(id("big")));
        assert_eq!(t.resolve(&full("pre-new"), &aliases), Some(id("pre-new")));
        assert_eq!(t.resolve(&full("other"), &aliases), None);
        assert_eq!(t.resolve(&ModelRef::Alias("fast".into()), &aliases), Some(id("big")));
        assert_eq!(t.resolve(&ModelRef::Alias("odd".into()), &aliases), None);
        assert_eq!(t.resolve(&ModelRef::Alias("missing".into()), &aliases), None);
        assert_eq!(t.resolve(&ModelRef::Inherit, &aliases), None);

        let open = ModelTable::load("p", BUILTIN, &ProviderConfig::default(), &[], true).unwrap();
        assert_eq!(open.resolve(&ModelRef::Alias("odd".into()), &aliases), Some(id("weird")));
        assert_eq!(open.resolve(&full("weird"), &aliases), None, "a bare id still has to be known");
    }

    #[test]
    fn a_refresh_keeps_known_prices_marks_new_models_unpriced_and_writes_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ProviderConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..ProviderConfig::default()
        };
        let mut t = table(&cfg);
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let listed = t.refreshed(
            "p",
            vec![id("big"), id("fresh")],
            |m| ModelInfo {
                id: m.clone(),
                context_window: 7,
                max_output: 7,
                family: "new".into(),
                deprecated: false,
            },
            Some(dir.path()),
            now,
        );
        assert_eq!(listed.len(), 2);
        assert_eq!(t.price(&id("big")).unwrap().input_per_mtok, 3.0);
        assert_eq!(t.price(&id("fresh")), None);

        assert_eq!(cache_age(dir.path(), "p", now + Duration::from_secs(60)), Some(Duration::from_secs(60)));
        assert!(!cache_is_stale(dir.path(), "p", now + CACHE_TTL));
        assert!(cache_is_stale(dir.path(), "p", now + CACHE_TTL + Duration::from_secs(1)));
        assert!(cache_is_stale(dir.path(), "other", now), "no file is stale");
        // The next process starts from the cache.
        assert_eq!(table(&cfg).models().len(), 3);
    }

    fn base_caps() -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: false,
            prompt_cache: false,
            logprobs: true,
            effort: false,
            images: false,
            system_role: SystemRole::Separate,
            max_tools: None,
        }
    }

    #[test]
    fn an_entry_parses_capabilities_and_reasoning_and_defaults_both() {
        let f: File = serde_json::from_str(
            r#"{"models": [
              {"id": "a", "context_window": 1, "max_output": 1, "family": "f", "deprecated": false},
              {"id": "b", "context_window": 1, "max_output": 1, "family": "f", "deprecated": false,
               "reasoning": true,
               "capabilities": {"logprobs": false, "max_tools": 8, "system_role": "first_message"}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(f.models[0].capabilities, CapabilityOverrides::default());
        assert_eq!(f.models[0].reasoning, None);
        assert_eq!(f.models[1].reasoning, Some(true));
        assert_eq!(
            f.models[1].capabilities,
            CapabilityOverrides {
                logprobs: Some(false),
                max_tools: Some(8),
                system_role: Some(SystemRole::FirstMessage),
                ..CapabilityOverrides::default()
            }
        );
    }

    #[test]
    fn a_0_1_0_override_file_loads_unchanged_and_keeps_what_the_builtin_said() {
        // No capabilities, no reasoning: the shape every 0.1.0 `models.json` has.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("p")).unwrap();
        std::fs::write(
            dir.path().join("p/models.json"),
            r#"{"models": [{"id": "caps", "context_window": 5, "max_output": 5, "family": "f", "deprecated": false}]}"#,
        )
        .unwrap();
        let cfg = ProviderConfig { overrides_dir: Some(dir.path().to_owned()), ..ProviderConfig::default() };
        let t = ModelTable::load("p", CAPS, &cfg, &[], false).unwrap();
        assert_eq!(t.models().iter().find(|m| m.id.0 == "caps").unwrap().context_window, 5);
        assert!(!t.capabilities_for(base_caps(), &id("caps")).logprobs, "the built-in's claim survives a size-only override");
    }

    const CAPS: &str = r#"{"models": [
      {"id": "caps", "context_window": 1, "max_output": 1, "family": "f", "deprecated": false,
       "capabilities": {"logprobs": false, "images": true}},
      {"id": "legacy", "context_window": 1, "max_output": 1, "family": "f", "deprecated": false, "effort": true}
    ]}"#;

    #[test]
    fn capabilities_for_resolves_default_then_builtin_then_cache_then_override() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, over) = (dir.path().join("cache"), dir.path().join("providers"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(over.join("p")).unwrap();
        let cfg = ProviderConfig { cache_dir: Some(cache.clone()), overrides_dir: Some(over.clone()), ..ProviderConfig::default() };

        // Preset default only: the model is not in any table.
        let t = ModelTable::load("p", CAPS, &cfg, &[], false).unwrap();
        assert_eq!(t.capabilities_for(base_caps(), &id("nope")), base_caps());

        // Built-in names logprobs=false and images=true; the rest is the preset's.
        let c = t.capabilities_for(base_caps(), &id("caps"));
        assert!(!c.logprobs && c.images && c.tools);

        // The cache names tools=false, and says nothing of images: images stays.
        std::fs::write(
            cache.join("p.json"),
            r#"{"fetched_at": 1, "models": [{"id": "caps", "context_window": 1, "max_output": 1, "family": "f", "deprecated": false, "capabilities": {"tools": false}}]}"#,
        )
        .unwrap();
        let c = ModelTable::load("p", CAPS, &cfg, &[], false).unwrap().capabilities_for(base_caps(), &id("caps"));
        assert!(!c.tools && c.images && !c.logprobs);

        // The user's file wins last: logprobs back on, tools still off.
        std::fs::write(
            over.join("p/models.json"),
            r#"{"models": [{"id": "caps", "context_window": 1, "max_output": 1, "family": "f", "deprecated": false, "capabilities": {"logprobs": true}}]}"#,
        )
        .unwrap();
        let c = ModelTable::load("p", CAPS, &cfg, &[], false).unwrap().capabilities_for(base_caps(), &id("caps"));
        assert!(c.logprobs && !c.tools && c.images);
    }

    #[test]
    fn the_old_effort_field_still_means_the_effort_capability() {
        let t = ModelTable::load("p", CAPS, &ProviderConfig::default(), &[], false).unwrap();
        assert!(t.capabilities_for(base_caps(), &id("legacy")).effort);
        assert!(!t.capabilities_for(base_caps(), &id("caps")).effort);
    }

    #[test]
    fn a_dated_snapshot_takes_its_models_capabilities() {
        let t = ModelTable::load("p", CAPS, &ProviderConfig::default(), &[], false).unwrap();
        assert!(!t.capabilities_for(base_caps(), &id("caps-20250929")).logprobs);
    }
}
