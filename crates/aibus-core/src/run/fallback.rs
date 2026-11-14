//! Account fallback (F9, 04 section 4.3): where a run, an advisor or a router goes when its
//! account fails before the first token of a turn.
//!
//! The chain is built when the run launches: each account of the fallback list, with the model
//! resolved on it, its provider and its credential. A switch is a decision about one error, so
//! `next` takes the error and answers with the account to try, or `None` to end as before.

use std::sync::Arc;

use aibus_provider::ProviderError;

use super::ModelSwitch;
use crate::accounts::AccountRegistry;
use crate::config::FallbackKind;
use crate::ids::AccountName;

pub struct Fallback {
    /// The accounts to try, in order.
    pub chain: Vec<ModelSwitch>,
    /// `defaults.fallback_on`.
    pub on: Vec<FallbackKind>,
    /// Where an account that triggered a switch is marked as cooling.
    pub accounts: Arc<AccountRegistry>,
}

impl Fallback {
    /// `failed` gave `e` before anything reached the user. The account to try next, when `e` is
    /// a kind that moves, and one in the chain has not been `tried` this turn and is not itself
    /// cooling. `failed` is marked cooling only when there is somewhere to go.
    pub fn next(
        &self,
        failed: &AccountName,
        tried: &[AccountName],
        e: &ProviderError,
    ) -> Option<ModelSwitch> {
        let kind = FallbackKind::of(e.kind).filter(|k| self.on.contains(k))?;
        let to = self.chain.iter().find(|s| {
            s.account != *failed && !tried.contains(&s.account) && !self.accounts.cooled(&s.account)
        })?;
        self.accounts.cool(failed, kind, e);
        tracing::info!(
            "account {failed} failed ({:?}), falling back to {}",
            e.kind,
            to.account
        );
        Some(to.clone())
    }
}
