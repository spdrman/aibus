//! The accounts a session can use and the chain that picks one for a model (04 sections 4.1
//! and 4.2, 01 section 3).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aibus_provider::{
    AliasTable, ConfigError, Gated, Limiter, ModelId, ModelRef, Provider, ProviderRegistry,
};
use smol_str::SmolStr;
use tokio::time::Instant;

use crate::config::{self, Config, FallbackKind};
use crate::error::{AibusError, CredentialError};
use crate::ids::AccountName;
use crate::msg::ModelView;

/// What a caller knows that narrows the choice. Steps 1 to 3 of the chain read the first three.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SelectHints {
    /// `--account`, or the `account` argument of a `delegate` call.
    pub explicit: Option<AccountName>,
    /// The agent definition's `account`.
    pub agent: Option<AccountName>,
    /// The parent run's account and model.
    pub inherit: Option<(AccountName, ModelId)>,
    /// Tags that break a tie between several accounts that can serve the model.
    pub prefer_tags: Vec<SmolStr>,
    /// Among several accounts that can serve the model, take one whose provider reports
    /// logprobs (the router's `p` is then a probability and not a self-report, Q4).
    pub logprobs: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    /// Exit code 4; the message is also the TUI banner.
    #[error(
        "no account serves model {model}; add one with: aibus account add <name> --provider <id>"
    )]
    NoAccount { model: String },
    #[error("no account named {0}")]
    Unknown(AccountName),
    #[error("account {0} is disabled")]
    Disabled(AccountName),
    #[error("account {account} cannot serve model {model}")]
    CannotServe { account: AccountName, model: String },
}

impl From<SelectError> for CredentialError {
    fn from(e: SelectError) -> Self {
        match e {
            SelectError::Unknown(a) => CredentialError::UnknownAccount(a),
            SelectError::Disabled(a) => CredentialError::UnknownAccount(a),
            SelectError::NoAccount { model } | SelectError::CannotServe { model, .. } => {
                CredentialError::NoAccountForModel(model)
            }
        }
    }
}

impl From<SelectError> for AibusError {
    fn from(e: SelectError) -> Self {
        AibusError::Auth(e.into())
    }
}

pub struct AccountRegistry {
    accounts: Vec<config::Account>,
    providers: Arc<ProviderRegistry>,
    /// Each account's provider behind its own rate limit.
    gated: BTreeMap<AccountName, Arc<dyn Provider>>,
    models: AliasTable,
    default_account: Option<String>,
    default_model: Option<String>,
    fallback_on: Vec<FallbackKind>,
    /// Accounts a fallback moved away from, until when (`None`: until `aibus account test`
    /// passes). In memory only: a restart, or the reload a login or logout causes, forgets it.
    cooldowns: Mutex<BTreeMap<AccountName, Option<Instant>>>,
}

/// How long an account that ran out of quota sits out when the response did not say when its
/// window resets, and how long one that was overloaded does (04 section 4.3).
const QUOTA_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const OVERLOADED_COOLDOWN: Duration = Duration::from_secs(60);

fn show(m: &ModelRef) -> String {
    match m {
        ModelRef::Alias(a) => a.to_string(),
        ModelRef::Full(id) => id.to_string(),
        ModelRef::Inherit => "inherit".into(),
    }
}

impl AccountRegistry {
    pub fn new(cfg: &Config, providers: Arc<ProviderRegistry>) -> Self {
        let gated = cfg
            .accounts
            .iter()
            .filter_map(|a| {
                let inner = providers.get(&a.provider).ok()?;
                let limiter = Limiter::new(a.account_name(), a.rate);
                let gated: Arc<dyn Provider> = Arc::new(Gated::new(inner, limiter));
                Some((a.account_name(), gated))
            })
            .collect();
        Self {
            accounts: cfg.accounts.clone(),
            providers,
            gated,
            models: cfg.models.clone(),
            default_account: cfg.defaults.account.clone(),
            default_model: cfg.defaults.model.clone(),
            fallback_on: cfg.defaults.fallback_on.clone(),
            cooldowns: Mutex::default(),
        }
    }

    /// The failures that move a run to the next account (`defaults.fallback_on`).
    pub fn fallback_on(&self) -> &[FallbackKind] {
        &self.fallback_on
    }

    /// The accounts to try after `name`, in order: `pinned` (an agent definition's `fallback`,
    /// which may be empty) over the account's own list. Names that are unknown or disabled are
    /// left out; whether each serves the model is for `select` with `explicit` to say.
    pub fn fallback_names(
        &self,
        name: &AccountName,
        pinned: Option<&[AccountName]>,
    ) -> Vec<AccountName> {
        let own: Vec<AccountName>;
        let names = match (pinned, self.account(name)) {
            (Some(p), _) => p,
            (None, Some(a)) => {
                own = a
                    .fallback
                    .iter()
                    .map(|n| AccountName(n.as_str().into()))
                    .collect();
                &own
            }
            (None, None) => return Vec::new(),
        };
        names
            .iter()
            .filter(|n| *n != name && self.enabled(n).is_ok())
            .cloned()
            .collect()
    }

    /// `name` just gave `e` before its first token and a fallback is moving the run on: the
    /// selection chain skips it for a while. A quota waits for the window the headers named, or
    /// 30 minutes; an overload 60 seconds; a rate limit as long as the provider asked; an auth
    /// failure until `tested`.
    pub fn cool(&self, name: &AccountName, kind: FallbackKind, e: &aibus_provider::ProviderError) {
        let reset = e.rate_limit.as_ref().and_then(|r| r.resets_in);
        let span = match kind {
            FallbackKind::Quota => Some(reset.unwrap_or(QUOTA_COOLDOWN)),
            FallbackKind::Overloaded => Some(OVERLOADED_COOLDOWN),
            FallbackKind::RateLimit => Some(e.retry_after.or(reset).unwrap_or(OVERLOADED_COOLDOWN)),
            FallbackKind::Auth => None,
        };
        let until = span.map(|d| Instant::now() + d);
        self.cooldowns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.clone(), until);
    }

    /// Whether `name` is still sitting out. An expired entry is dropped on the way.
    pub fn cooled(&self, name: &AccountName) -> bool {
        let mut g = self
            .cooldowns
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match g.get(name) {
            None => false,
            Some(None) => true,
            Some(Some(t)) if *t > Instant::now() => true,
            Some(Some(_)) => {
                g.remove(name);
                false
            }
        }
    }

    /// `aibus account test` passed for `name`: it is back in the chain.
    pub fn tested(&self, name: &AccountName) {
        self.cooldowns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
    }

    /// The account's provider, behind that account's rate limit.
    pub fn provider(&self, name: &AccountName) -> Result<Arc<dyn Provider>, ConfigError> {
        self.gated.get(name).cloned().ok_or_else(|| {
            let provider = self
                .account(name)
                .map_or_else(|| name.to_string(), |a| a.provider.clone());
            ConfigError::UnknownProvider(provider)
        })
    }

    pub fn account(&self, name: &AccountName) -> Option<&config::Account> {
        self.accounts.iter().find(|a| a.name == name.0.as_str())
    }

    /// A model named on the command line or in config: an alias when `[models]` or an account
    /// defines it, `inherit`, or else a full id.
    pub fn model_ref(&self, s: &str) -> ModelRef {
        if s == "inherit" {
            ModelRef::Inherit
        } else if self.models.contains_key(s)
            || self.accounts.iter().any(|a| a.aliases.contains_key(s))
        {
            ModelRef::Alias(s.into())
        } else {
            ModelRef::Full(ModelId(s.into()))
        }
    }

    /// `account/model` in `[models]` pins the alias to that account.
    fn pin<'a>(&'a self, value: &'a str) -> Option<(&'a config::Account, &'a str)> {
        let (acct, model) = value.split_once('/')?;
        Some((self.accounts.iter().find(|a| a.name == acct)?, model))
    }

    /// The aliases `a` answers to: the global `[models]` that are not pinned to another account,
    /// over the account's own.
    fn alias_table(&self, a: &config::Account) -> AliasTable {
        let mut table = a.aliases.clone();
        for (alias, value) in &self.models {
            match self.pin(value) {
                Some((owner, m)) if owner.name == a.name => {
                    table.insert(alias.clone(), m.into());
                }
                // Pinned to another account: that alias is not this account's to serve.
                Some(_) => {
                    table.remove(alias);
                }
                None => {
                    table.insert(alias.clone(), value.clone());
                }
            }
        }
        table
    }

    /// `model` on `account`: the global `[models]` first, then the account's own aliases.
    fn resolve_on(&self, a: &config::Account, model: &ModelRef) -> Option<ModelId> {
        let provider = self.providers.get(&a.provider).ok()?;
        provider.resolve_model(model, &self.alias_table(a))
    }

    /// What `name` serves according to its provider's tables (and the cache of the last refresh,
    /// when the provider keeps one), without a request: the `/model` picker's rows. Deprecated
    /// models are left out; a price the table lacks stays `None`.
    pub fn model_views(&self, name: &AccountName) -> Vec<ModelView> {
        let Some(a) = self.account(name) else {
            return Vec::new();
        };
        let Ok(provider) = self.providers.get(&a.provider) else {
            return Vec::new();
        };
        let aliases = self.alias_table(a);
        // Capabilities are the provider's today; `capabilities_for(&id)` replaces this one line.
        let caps = provider.capabilities();
        provider
            .models()
            .into_iter()
            .filter(|m| !m.deprecated)
            .map(|m| {
                let price = provider.price(&m.id);
                ModelView {
                    alias: aliases
                        .iter()
                        .find(|(_, v)| v.as_str() == m.id.0.as_str())
                        .map(|(k, _)| k.to_string()),
                    context_window: m.context_window,
                    input_per_mtok: price.map(|p| p.input_per_mtok),
                    output_per_mtok: price.map(|p| p.output_per_mtok),
                    tools: caps.tools,
                    thinking: caps.thinking,
                    images: caps.images,
                    cache: caps.prompt_cache,
                    logprobs: caps.logprobs,
                    id: m.id,
                }
            })
            .collect()
    }

    fn enabled(&self, name: &AccountName) -> Result<&config::Account, SelectError> {
        match self.account(name) {
            None => Err(SelectError::Unknown(name.clone())),
            Some(a) if !a.enabled => Err(SelectError::Disabled(name.clone())),
            Some(a) => Ok(a),
        }
    }

    /// The first step of the chain that yields exactly one account (04 section 4.1).
    ///
    /// Steps 1 and 2 are demands: an account that is unknown, disabled or cannot serve the model
    /// is an error rather than a reason to try the next step. Steps 3 and 4 give way when the
    /// model does not resolve there. The choice is logged when several accounts could serve.
    pub fn select(
        &self,
        model: &ModelRef,
        hints: &SelectHints,
    ) -> Result<(AccountName, ModelId), SelectError> {
        // `inherit` is the parent's model, or for the lead `defaults.model`.
        let inherited = matches!(model, ModelRef::Inherit);
        let model = match (model, &hints.inherit, &self.default_model) {
            (ModelRef::Inherit, Some((_, id)), _) => ModelRef::Full(id.clone()),
            (ModelRef::Inherit, None, Some(d)) => self.model_ref(d),
            (m, _, _) => m.clone(),
        };

        if let Some(name) = hints.explicit.as_ref().or(hints.agent.as_ref()) {
            let a = self.enabled(name)?;
            return match self.resolve_on(a, &model) {
                Some(id) => Ok((name.clone(), id)),
                None => Err(SelectError::CannotServe {
                    account: name.clone(),
                    model: show(&model),
                }),
            };
        }

        if let Some((name, id)) = &hints.inherit {
            if let Some(a) = self.enabled(name).ok().filter(|_| !self.cooled(name)) {
                if inherited {
                    return Ok((name.clone(), id.clone()));
                }
                if let Some(id) = self.resolve_on(a, &model) {
                    return Ok((name.clone(), id));
                }
            }
        }

        if let Some(a) = self
            .default_account
            .as_deref()
            .and_then(|d| self.enabled(&AccountName(d.into())).ok())
            .filter(|a| !self.cooled(&a.account_name()))
        {
            if let Some(id) = self.resolve_on(a, &model) {
                return Ok((a.account_name(), id));
            }
        }

        if let ModelRef::Alias(alias) = &model {
            let pinned = self.models.get(alias).and_then(|v| self.pin(v));
            if let Some((a, m)) =
                pinned.filter(|(a, _)| a.enabled && !self.cooled(&a.account_name()))
            {
                if let Some(id) = self.resolve_on(a, &ModelRef::Full(ModelId(m.into()))) {
                    return Ok((a.account_name(), id));
                }
            }
        }

        let mut found: Vec<(&config::Account, ModelId)> = self
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .filter_map(|a| Some((a, self.resolve_on(a, &model)?)))
            .collect();
        // Accounts that are sitting out are passed over; with nothing else they serve anyway.
        if found.iter().any(|(a, _)| !self.cooled(&a.account_name())) {
            found.retain(|(a, _)| !self.cooled(&a.account_name()));
        }
        let first = found
            .iter()
            .position(|(a, _)| a.tags.iter().any(|t| hints.prefer_tags.contains(t)))
            .or_else(|| {
                let has = |(a, id): &(&config::Account, ModelId)| {
                    hints.logprobs
                        && self
                            .providers
                            .get(&a.provider)
                            .is_ok_and(|p| p.capabilities_for(id).logprobs)
                };
                found.iter().position(has)
            })
            .unwrap_or(0);
        if found.is_empty() {
            return Err(SelectError::NoAccount {
                model: show(&model),
            });
        }
        let (chosen, id) = found.remove(first);
        if !found.is_empty() {
            let also: Vec<&str> = found.iter().map(|(a, _)| a.name.as_str()).collect();
            tracing::info!(
                "model {} served by {} (also: {})",
                show(&model),
                chosen.name,
                also.join(", ")
            );
        }
        Ok((chosen.account_name(), id))
    }
}

#[cfg(test)]
mod tests;
