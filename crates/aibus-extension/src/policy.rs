//! The user's policy file (`<config>/extension-policy.toml`): which publishers and signing keys
//! this machine trusts, whether a signature is required, and versions or ids it revokes itself.
//! Absent file, default policy: no signature required, any publisher, nothing revoked locally.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::registry::Revocation;

/// A policy file larger than this is refused.
pub const MAX_POLICY_BYTES: usize = 64 * 1024;

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
    /// Program names the user wants run as official CLIs besides the host's own four.
    #[serde(default)]
    pub extra_official_clis: Vec<String>,
    #[serde(default)]
    pub revoke: Vec<Revocation>,
}

impl Policy {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_POLICY_BYTES {
            return Err(format!("the policy file is over {MAX_POLICY_BYTES} bytes"));
        }
        let p: Self = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        let key_id = regex::Regex::new(r"^[A-Za-z0-9._-]{1,64}$").expect("valid regex");
        for (id, key) in &p.trusted_keys {
            if !key_id.is_match(id) {
                return Err(format!("trusted key id {id:?} is not a plain name"));
            }
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "trusted key {id}: an ed25519 public key is 64 hex digits"
                ));
            }
        }
        if p.allow_publishers.as_ref().is_some_and(|a| a.len() > 64) || p.revoke.len() > 1024 {
            return Err("the policy lists too many publishers or revocations".into());
        }
        Ok(p)
    }

    /// The policy at `path`, or the default one when there is no file. A file that is there and
    /// wrong is an error, never a silent fall back to a more permissive policy.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        match aibus_core::persist::fs::read_regular(path, MAX_POLICY_BYTES as u64) {
            Ok(bytes) => Self::parse(
                &String::from_utf8(bytes).map_err(|_| "the policy file is not UTF-8".to_owned())?,
            ),
            Err(aibus_core::persist::fs::ReadError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(Self::default())
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}
