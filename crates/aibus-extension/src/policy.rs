//! The user's policy file (`<config>/extension-policy.toml`): which publishers and signing keys
//! this machine trusts, whether a signature is required, and versions or ids it revokes itself.
//! Absent file, default policy: no signature required, any publisher, nothing revoked locally.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::registry::Revocation;

/// A policy file larger than this is refused.
pub const MAX_POLICY_BYTES: usize = 64 * 1024;

/// A trusted ed25519 key and what it may sign. The scope lives only here, in the user's own file:
/// a registry entry, a package or an update has no field that could carry one.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKey {
    /// The 32-byte public key as 64 hex digits.
    pub key: String,
    /// Extension ids this key may sign, exactly.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Id prefixes this key may sign: `org.devshell.aibus` covers `org.devshell.aibus.x` and never
    /// `org.devshell.aibusx.x`. Two to seven labels, no wildcard.
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// Set (with a reason) when the key is revoked: it signs nothing, and nothing else is affected.
    #[serde(default)]
    pub revoked: Option<String>,
}

impl TrustedKey {
    /// Whether `id` is one of the exact ids or sits under one of the prefixes at a label boundary.
    pub fn covers(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
            || self.prefixes.iter().any(|p| {
                id.strip_prefix(p.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
            })
    }
}

/// What a trusted key may do, as the policy file spells it before the scope existed.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawKey {
    // Only matched to refuse it with a message, never read.
    #[allow(dead_code)]
    Old(String),
    Scoped(TrustedKey),
}

fn trusted_keys<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, TrustedKey>, D::Error> {
    use serde::de::Error;
    BTreeMap::<String, RawKey>::deserialize(d)?
        .into_iter()
        .map(|(name, raw)| match raw {
            RawKey::Scoped(k) => Ok((name, k)),
            RawKey::Old(_) => Err(D::Error::custom(format!(
                "trusted key {name:?} has no scope. A key now says which extension ids it may sign; \
                 write [trusted_keys.{name}] with key = \"<64 hex digits>\" and ids = [\"org.example.tool\"] \
                 and/or prefixes = [\"org.example\"]. I do not guess a scope for an old file"
            ))),
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub require_signature: bool,
    /// Key id to the trusted key and its scope.
    #[serde(default, deserialize_with = "trusted_keys")]
    pub trusted_keys: BTreeMap<String, TrustedKey>,
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
        let prefix = regex::Regex::new(r"^[a-z][a-z0-9-]{0,30}(\.[a-z][a-z0-9-]{0,30}){1,6}$")
            .expect("valid regex");
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for (id, k) in &p.trusted_keys {
            if !key_id.is_match(id) {
                return Err(format!("trusted key id {id:?} is not a plain name"));
            }
            if k.key.len() != 64 || !k.key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "trusted key {id}: an ed25519 public key is 64 hex digits"
                ));
            }
            // The same key under two names would sign the union of both scopes.
            if let Some(other) = seen.insert(k.key.to_ascii_lowercase(), id) {
                return Err(format!(
                    "trusted keys {other} and {id} are the same key; list it once with one scope"
                ));
            }
            if k.ids.is_empty() && k.prefixes.is_empty() {
                return Err(format!(
                    "trusted key {id} has an empty scope; give it ids and/or prefixes"
                ));
            }
            if k.ids.len() + k.prefixes.len() > 64 {
                return Err(format!("trusted key {id} has more than 64 scope entries"));
            }
            if let Some(bad) = k.ids.iter().find(|i| !crate::manifest::valid_id(i)) {
                return Err(format!(
                    "trusted key {id}: {bad:?} is not an extension id (lower-case ASCII labels, no wildcard)"
                ));
            }
            if let Some(bad) = k.prefixes.iter().find(|x| !prefix.is_match(x)) {
                return Err(format!(
                    "trusted key {id}: {bad:?} is not an id prefix (two to seven lower-case labels, no wildcard, no trailing dot)"
                ));
            }
            if k.revoked
                .as_ref()
                .is_some_and(|r| r.is_empty() || r.len() > 500 || r.chars().any(char::is_control))
            {
                return Err(format!(
                    "trusted key {id}: the revocation needs a plain reason"
                ));
            }
        }
        let program = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").expect("valid regex");
        for name in &p.extra_official_clis {
            if !program.is_match(name) || crate::manifest::forbidden_program(name) {
                return Err(format!("extra_official_clis: {name:?} is not a plain program name that may run as an official CLI"));
            }
        }
        if p.extra_official_clis.len() > 16 {
            return Err("extra_official_clis: more than 16 names".into());
        }
        if p.allow_publishers.as_ref().is_some_and(|a| a.len() > 64) || p.revoke.len() > 1024 {
            return Err("the policy lists too many publishers or revocations".into());
        }
        Ok(p)
    }

    /// The policy at `path`, or the default one when there is no file. A file that is there and
    /// wrong is an error, never a silent fall back to a more permissive policy.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        // A policy another user can write could add a trusted key or drop `require_signature`.
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(path) {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if meta.permissions().mode() & 0o022 != 0 {
                return Err(format!("{} is writable by other users", path.display()));
            }
            if meta.uid() != rustix::process::geteuid().as_raw() && meta.uid() != 0 {
                return Err(format!(
                    "{} is owned by uid {}, not this user or root",
                    path.display(),
                    meta.uid()
                ));
            }
        }
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
