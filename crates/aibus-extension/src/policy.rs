//! The user's policy file (`<config>/extension-policy.toml`): which publishers and signing keys
//! this machine trusts, whether a signature is required, and versions or ids it revokes itself.
//! Absent file, default policy: no signature required, any publisher, nothing revoked locally.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::registry::Revocation;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub require_signature: bool,
    /// Key id to the 32-byte ed25519 public key as 64 hex digits.
    #[serde(default)]
    pub trusted_keys: BTreeMap<String, String>,
    /// When set, only these publisher names install.
    pub allow_publishers: Option<Vec<String>>,
    #[serde(default)]
    pub revoke: Vec<Revocation>,
}

impl Policy {
    pub fn parse(text: &str) -> Result<Self, String> {
        todo!("{text}")
    }
}
