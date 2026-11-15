//! The shareable registry format: a directory with `aibus-registry.json` (entries with a content
//! hash, an optional signature, and a revocation list) and one package directory per entry. Local
//! first: no network service, nothing fetched. A registry is a list of claims, not a guarantee:
//! every fetch re-reads the package and checks it against the entry, the policy and the
//! revocations before anything is installed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::negotiate::Incompatible;
use crate::package::{Package, PackageError};
use crate::policy::Policy;
use crate::version::Version;

pub const INDEX_FILE: &str = "aibus-registry.json";
pub const MAX_INDEX_BYTES: usize = 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
/// The only index version this host reads.
pub const INDEX_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub key_id: String,
    /// ed25519 over `signed_message`, 128 hex digits.
    pub signature: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub version: String,
    pub hash: String,
    /// The package directory, relative to the registry, one plain name or `a/b`; never `..`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub id: String,
    /// Absent: every version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Absent: any content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("the registry index is {0} bytes, the most read is {MAX_INDEX_BYTES}")]
    TooLarge(usize),
    #[error("the registry index is not valid: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(String),
}

/// Why a package was not accepted, and what the store does about it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("revoked: {0}")]
    Revoked(String),
    #[error("the package hashes to {actual}, the registry says {expected}")]
    HashMismatch { expected: String, actual: String },
    #[error("the package is {found}, the registry entry says {expected}")]
    IdentityMismatch { expected: String, found: String },
    #[error("the signature does not verify: {0}")]
    BadSignature(String),
    #[error("the policy requires a signature from a trusted key and this entry has none")]
    SignatureRequired,
    #[error("key {key_id} may not sign {id}: it is trusted for other ids, and only the policy file widens that")]
    KeyOutOfScope { key_id: String, id: String },
    #[error("key {key_id} is revoked: {reason}")]
    KeyRevoked { key_id: String, reason: String },
    #[error("publisher {0} is not allowed by the policy")]
    PublisherNotAllowed(String),
    #[error("incompatible: {0}")]
    Incompatible(#[from] Incompatible),
    #[error("{0}")]
    Package(#[from] PackageError),
}

impl Refusal {
    /// How the refusal is filed in quarantine: `revoked`, `provenance_invalid`, `incompatible`
    /// or `malformed`.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Revoked(_) | Self::KeyRevoked { .. } => "revoked",
            Self::HashMismatch { .. }
            | Self::IdentityMismatch { .. }
            | Self::BadSignature(_)
            | Self::SignatureRequired
            | Self::KeyOutOfScope { .. }
            | Self::PublisherNotAllowed(_) => "provenance_invalid",
            Self::Incompatible(_) => "incompatible",
            Self::Package(_) => "malformed",
        }
    }
}

/// What a signature covers: the identity and the content hash, with a domain tag.
pub fn signed_message(id: &str, version: &str, hash: &str) -> Vec<u8> {
    format!("aibus-extension-v1\n{id}\n{version}\n{hash}").into_bytes()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Whether the policy still lets `key_id` stand behind `id`: the key is not revoked and `id` is in
/// its scope. A key the policy does not list is not this function's business.
pub fn signer_ok(policy: &Policy, key_id: &str, id: &str) -> Result<(), Refusal> {
    let Some(k) = policy.trusted_keys.get(key_id) else {
        return Ok(());
    };
    if let Some(reason) = &k.revoked {
        return Err(Refusal::KeyRevoked {
            key_id: key_id.to_owned(),
            reason: reason.clone(),
        });
    }
    if !k.covers(id) {
        return Err(Refusal::KeyOutOfScope {
            key_id: key_id.to_owned(),
            id: id.to_owned(),
        });
    }
    Ok(())
}

/// The first revocation that names this id, version and content (a revocation without a version
/// covers every version, one without a hash any content).
pub fn revoked_by<'a>(
    list: impl IntoIterator<Item = &'a Revocation>,
    id: &str,
    version: &str,
    hash: &str,
) -> Option<&'a Revocation> {
    list.into_iter().find(|r| {
        r.id == id
            && r.version.as_ref().is_none_or(|v| v == version)
            && r.hash.as_ref().is_none_or(|h| h == hash)
    })
}

/// Checks `pkg` against `entry`, the revocations (the registry's and the policy's) and the policy.
pub fn check(
    pkg: &Package,
    entry: &Entry,
    revoked: &[Revocation],
    policy: &Policy,
) -> Result<(), Refusal> {
    let m = &pkg.manifest;
    if m.id != entry.id || m.version != entry.version {
        return Err(Refusal::IdentityMismatch {
            expected: format!("{} {}", entry.id, entry.version),
            found: format!("{} {}", m.id, m.version),
        });
    }
    if pkg.hash != entry.hash {
        return Err(Refusal::HashMismatch {
            expected: entry.hash.clone(),
            actual: pkg.hash.clone(),
        });
    }
    if let Some(r) = revoked_by(
        revoked.iter().chain(&policy.revoke),
        &entry.id,
        &entry.version,
        &entry.hash,
    ) {
        return Err(Refusal::Revoked(r.reason.clone()));
    }
    match (
        &entry.signature,
        policy
            .trusted_keys
            .get(entry.signature.as_ref().map_or("", |s| &s.key_id)),
    ) {
        (Some(sig), Some(key)) => {
            let bad = |why: &str| Refusal::BadSignature(why.to_owned());
            signer_ok(policy, &sig.key_id, &entry.id)?;
            let key = unhex(&key.key).ok_or_else(|| bad("the trusted key is not hex"))?;
            let raw = unhex(&sig.signature).ok_or_else(|| bad("the signature is not hex"))?;
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
                .verify(
                    &signed_message(&entry.id, &entry.version, &entry.hash),
                    &raw,
                )
                .map_err(|_| bad("it does not match this id, version and hash"))?;
        }
        (Some(sig), None) if policy.require_signature => {
            return Err(Refusal::BadSignature(format!(
                "{:?} is not a trusted key",
                sig.key_id
            )));
        }
        (None, _) if policy.require_signature => return Err(Refusal::SignatureRequired),
        _ => {}
    }
    if let Some(allowed) = &policy.allow_publishers {
        if !allowed.contains(&m.publisher.name) {
            return Err(Refusal::PublisherNotAllowed(m.publisher.name.clone()));
        }
    }
    crate::negotiate::check_host(m, std::env::consts::OS)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Registry {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub revoked: Vec<Revocation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    index_version: u32,
    entries: Vec<Entry>,
    #[serde(default)]
    revoked: Vec<Revocation>,
}

fn invalid(why: impl Into<String>) -> RegistryError {
    RegistryError::Invalid(why.into())
}

fn validate(index: &Index) -> Result<(), RegistryError> {
    if index.index_version != INDEX_VERSION {
        return Err(invalid(format!(
            "index_version {} is not {INDEX_VERSION}",
            index.index_version
        )));
    }
    if index.entries.len() > MAX_ENTRIES || index.revoked.len() > MAX_ENTRIES {
        return Err(invalid(format!(
            "more than {MAX_ENTRIES} entries or revocations"
        )));
    }
    let hash = regex::Regex::new(r"^sha256-tree-v1:[0-9a-f]{64}$").expect("valid regex");
    let key_id = regex::Regex::new(r"^[A-Za-z0-9._-]{1,64}$").expect("valid regex");
    let part = regex::Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9._-]{0,99}$").expect("valid regex");
    let mut seen = std::collections::BTreeSet::new();
    for e in &index.entries {
        if !crate::manifest::valid_id(&e.id) {
            return Err(invalid(format!(
                "entry id {:?} is not an extension id",
                e.id
            )));
        }
        if crate::version::exact(&e.version).is_none() {
            return Err(invalid(format!(
                "entry version {:?} is not x.y.z",
                e.version
            )));
        }
        if !hash.is_match(&e.hash) {
            return Err(invalid(format!(
                "entry hash {:?} is not sha256-tree-v1",
                e.hash
            )));
        }
        // A relative path of one to three plain names.
        let parts: Vec<&str> = e.path.split('/').collect();
        if !(1..=3).contains(&parts.len()) || !parts.iter().all(|x| part.is_match(x)) {
            return Err(invalid(format!(
                "entry path {:?} is not a plain relative path",
                e.path
            )));
        }
        if let Some(s) = &e.signature {
            if !key_id.is_match(&s.key_id)
                || s.signature.len() != 128
                || unhex(&s.signature).is_none()
            {
                return Err(invalid("a signature is 128 hex digits with a plain key id"));
            }
        }
        if !seen.insert((e.id.clone(), e.version.clone())) {
            return Err(invalid(format!("duplicate entry {} {}", e.id, e.version)));
        }
    }
    for r in &index.revoked {
        if !crate::manifest::valid_id(&r.id)
            || r.version
                .as_deref()
                .is_some_and(|v| crate::version::exact(v).is_none())
            || r.hash.as_deref().is_some_and(|h| !hash.is_match(h))
            || r.reason.is_empty()
            || r.reason.len() > 500
            || r.reason.chars().any(char::is_control)
        {
            return Err(invalid(format!("revocation of {:?} is malformed", r.id)));
        }
    }
    Ok(())
}

fn not_a_link(p: &Path) -> Result<std::fs::Metadata, RegistryError> {
    let m = std::fs::symlink_metadata(p)
        .map_err(|e| RegistryError::Io(format!("{}: {e}", p.display())))?;
    if m.file_type().is_symlink() {
        return Err(invalid(format!("{} is a link", p.display())));
    }
    Ok(m)
}

fn read_index(dir: &Path) -> Result<Index, RegistryError> {
    if !not_a_link(dir)?.is_dir() {
        return Err(invalid(format!("{} is not a directory", dir.display())));
    }
    let file = dir.join(INDEX_FILE);
    let meta = not_a_link(&file)?;
    if meta.len() > MAX_INDEX_BYTES as u64 {
        return Err(RegistryError::TooLarge(meta.len() as usize));
    }
    let bytes = aibus_core::persist::fs::read_regular(&file, MAX_INDEX_BYTES as u64).map_err(
        |e| match e {
            aibus_core::persist::fs::ReadError::TooBig(n, _) => RegistryError::TooLarge(n as usize),
            other => RegistryError::Io(other.to_string()),
        },
    )?;
    let index: Index = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    validate(&index)?;
    Ok(index)
}

impl Registry {
    /// Reads and validates the index: size, version, every id and version well formed, no
    /// duplicate (id, version), every path inside the registry.
    pub fn open(dir: &Path) -> Result<Self, RegistryError> {
        let index = read_index(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            entries: index.entries,
            revoked: index.revoked,
        })
    }

    fn newest_first(&self) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.entries.iter().collect();
        v.sort_by(|a, b| {
            let ver = |e: &Entry| crate::version::exact(&e.version);
            (&a.id, ver(b)).cmp(&(&b.id, ver(a)))
        });
        v
    }

    /// Entries whose id or package name contains `query`, case-insensitive, newest first.
    pub fn search(&self, query: &str) -> Vec<&Entry> {
        let q = query.to_lowercase();
        self.newest_first()
            .into_iter()
            .filter(|e| e.id.to_lowercase().contains(&q) || e.path.to_lowercase().contains(&q))
            .collect()
    }

    /// The entry for `id` at `version`, or its newest version.
    pub fn find(&self, id: &str, version: Option<&Version>) -> Option<&Entry> {
        let mut hits = self.newest_first().into_iter().filter(|e| e.id == id);
        match version {
            Some(v) => hits.find(|e| crate::version::exact(&e.version).as_ref() == Some(v)),
            None => hits.next(),
        }
    }

    /// Reads the entry's package and checks it.
    pub fn fetch(&self, entry: &Entry, policy: &Policy) -> Result<Package, Refusal> {
        // No link anywhere on the way down from the registry directory.
        let mut at = self.dir.clone();
        for part in entry.path.split('/') {
            at.push(part);
            if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(PackageError::Link(entry.path.clone()).into());
            }
        }
        let pkg = crate::package::read(&at)?;
        check(&pkg, entry, &self.revoked, policy)?;
        Ok(pkg)
    }

    /// Adds the package at `pkg_dir` to the registry at `dir` (created if missing): copies it to
    /// `<id>/<version>`, records its hash. Refuses a (id, version) already there.
    pub fn publish(dir: &Path, pkg_dir: &Path) -> Result<Entry, RegistryError> {
        let pkg = crate::package::read(pkg_dir).map_err(|e| invalid(e.to_string()))?;
        let mut index = if dir.join(INDEX_FILE).exists() {
            read_index(dir)?
        } else {
            std::fs::create_dir_all(dir).map_err(|e| RegistryError::Io(e.to_string()))?;
            Index {
                index_version: INDEX_VERSION,
                entries: Vec::new(),
                revoked: Vec::new(),
            }
        };
        let (id, version) = (pkg.manifest.id.clone(), pkg.manifest.version.clone());
        if index
            .entries
            .iter()
            .any(|e| e.id == id && e.version == version)
        {
            return Err(invalid(format!(
                "{id} {version} is already in the registry"
            )));
        }
        let path = format!("{id}/{version}");
        let target = dir.join(&path);
        std::fs::create_dir_all(target.parent().expect("has a parent"))
            .map_err(|e| RegistryError::Io(e.to_string()))?;
        pkg.write(&target)
            .map_err(|e| RegistryError::Io(e.to_string()))?;
        let entry = Entry {
            id,
            version,
            hash: pkg.hash,
            path,
            signature: None,
        };
        index.entries.push(entry.clone());
        validate(&index)?;
        let text = serde_json::to_vec_pretty(&index).map_err(|e| invalid(e.to_string()))?;
        aibus_core::persist::fs::write_atomic(&dir.join(INDEX_FILE), text)
            .map_err(|e| RegistryError::Io(e.to_string()))?;
        Ok(entry)
    }
}
