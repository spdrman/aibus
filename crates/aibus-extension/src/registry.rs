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
    pub signature: Option<Signature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub id: String,
    /// Absent: every version.
    pub version: Option<String>,
    /// Absent: any content.
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
    #[error("publisher {0} is not allowed by the policy")]
    PublisherNotAllowed(String),
    #[error("incompatible: {0}")]
    Incompatible(#[from] Incompatible),
    #[error("{0}")]
    Package(#[from] PackageError),
}

/// How a refusal is filed in quarantine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Revoked,
    ProvenanceInvalid,
    Incompatible,
    Malformed,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::ProvenanceInvalid => "provenance_invalid",
            Self::Incompatible => "incompatible",
            Self::Malformed => "malformed",
        }
    }
}

impl Refusal {
    pub fn class(&self) -> Class {
        todo!()
    }
}

/// What a signature covers: the identity and the content hash, with a domain tag.
pub fn signed_message(id: &str, version: &str, hash: &str) -> Vec<u8> {
    todo!("{id} {version} {hash}")
}

/// Checks `pkg` against `entry`, the revocations (the registry's and the policy's) and the policy.
pub fn check(
    pkg: &Package,
    entry: &Entry,
    revoked: &[Revocation],
    policy: &Policy,
) -> Result<(), Refusal> {
    todo!("{pkg:?} {entry:?} {revoked:?} {policy:?}")
}

#[derive(Clone, Debug)]
pub struct Registry {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub revoked: Vec<Revocation>,
}

impl Registry {
    /// Reads and validates the index: size, version, every id and version well formed, no
    /// duplicate (id, version), every path inside the registry.
    pub fn open(dir: &Path) -> Result<Self, RegistryError> {
        todo!("{dir:?}")
    }

    /// Entries whose id or package name contains `query`, case-insensitive, newest first.
    pub fn search(&self, query: &str) -> Vec<&Entry> {
        todo!("{query}")
    }

    /// The entry for `id` at `version`, or its newest version.
    pub fn find(&self, id: &str, version: Option<&Version>) -> Option<&Entry> {
        todo!("{id} {version:?}")
    }

    /// Reads the entry's package and checks it.
    pub fn fetch(&self, entry: &Entry, policy: &Policy) -> Result<Package, Refusal> {
        todo!("{entry:?} {policy:?}")
    }

    /// Adds the package at `pkg_dir` to the registry at `dir` (created if missing): copies it to
    /// `<id>/<version>`, records its hash. Refuses a (id, version) already there.
    pub fn publish(dir: &Path, pkg_dir: &Path) -> Result<Entry, RegistryError> {
        todo!("{dir:?} {pkg_dir:?}")
    }
}
