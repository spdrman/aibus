//! Turning an agent definition's `model` into a provider-native id.
//!
//! The built-in agents name their model with a reserved alias (`default_model`,
//! `advisor_model`, `router_model`, `swarm_model`) that stands for the `defaults.*` key of the
//! same name, or with `inherit`. Neither is a real alias, so the provider never sees them.

use aibus_provider::{AliasTable, ModelId, ModelRef, Provider};

/// The `defaults.*` model keys from config. `None` means the key is unset.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ModelDefaults {
    pub default_model: Option<ModelRef>,
    pub advisor_model: Option<ModelRef>,
    pub router_model: Option<ModelRef>,
    pub swarm_model: Option<ModelRef>,
}

/// Resolves `model` for `provider`. A reserved alias becomes its `defaults.*` value, and an unset
/// key falls back to `inherit`. `inherit` takes the parent's model, or the session default for a
/// run without a parent. `None` when nothing names a model the provider knows.
pub fn resolve_model(
    model: &ModelRef,
    defaults: &ModelDefaults,
    parent: Option<&ModelId>,
    provider: &dyn Provider,
    aliases: &AliasTable,
) -> Option<ModelId> {
    let named = match model {
        ModelRef::Alias(a) => match a.as_str() {
            "default_model" => defaults.default_model.as_ref(),
            "advisor_model" => defaults.advisor_model.as_ref(),
            "router_model" => defaults.router_model.as_ref(),
            "swarm_model" => defaults.swarm_model.as_ref(),
            _ => Some(model),
        },
        other => Some(other),
    };
    match named.unwrap_or(&ModelRef::Inherit) {
        ModelRef::Inherit => match parent {
            Some(p) => Some(p.clone()),
            None => match defaults.default_model.as_ref()? {
                ModelRef::Inherit => None,
                r => provider.resolve_model(r, aliases),
            },
        },
        r => provider.resolve_model(r, aliases),
    }
}
