//! The installed store: `<root>/state.json` and `<root>/packages/<id>/<version>/`. Install,
//! verify, list, update, pin, disable, enable, remove and rollback, each one atomic at the level of
//! the state file. Nothing is activated that did not pass the registry check, and an installed
//! package is re-hashed every time it is resolved, so a file changed on disk after the install
//! quarantines it instead of running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::package::Package;
use crate::policy::Policy;
use crate::registry::{Refusal, Registry, Revocation};
use crate::version::Version;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantined {
    pub version: String,
    pub hash: String,
    /// `revoked`, `provenance_invalid`, `incompatible`, `malformed` or `tampered`.
    pub class: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The version `resolve` returns.
    pub active: Option<String>,
    /// The one before `active`, for `rollback`.
    pub previous: Option<String>,
    /// A pinned id is never changed by `update`.
    pub pinned: bool,
    pub disabled: bool,
    /// Installed versions and their content hashes.
    pub versions: BTreeMap<String, String>,
    pub quarantined: Vec<Quarantined>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub extensions: BTreeMap<String, Record>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The package was refused and a quarantine record was written; nothing was activated.
    #[error("quarantined ({}): {reason}", .class)]
    Quarantined { class: String, reason: String },
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("{0} is not in the registry")]
    NotInRegistry(String),
    #[error("{0} is disabled")]
    Disabled(String),
    #[error("{0} is pinned; unpin it first")]
    Pinned(String),
    #[error("{0} has a quarantined active version")]
    ActiveQuarantined(String),
    #[error("version {to} is not newer than the installed {from}; rolling back is `rollback`")]
    Downgrade { from: String, to: String },
    #[error("nothing newer than {0} is available")]
    UpToDate(String),
    #[error("no earlier version of {0} is installed to roll back to")]
    NoPrevious(String),
    /// The new version asks for more than the installed one; nothing was changed.
    #[error("the new version asks for more: {0:?}; accept them by name to continue")]
    Escalation(Vec<String>),
    #[error("state: {0}")]
    State(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn open(root: &Path) -> Self {
        todo!("{root:?}")
    }

    pub fn state(&self) -> Result<State, StoreError> {
        todo!()
    }

    /// Installs `id` (at `version`, or the newest) from the registry. `accept` names the
    /// escalations the user agreed to, for an install over an older version.
    pub fn install(
        &self,
        reg: &Registry,
        policy: &Policy,
        id: &str,
        version: Option<&Version>,
        accept: &[String],
    ) -> Result<Record, StoreError> {
        todo!("{reg:?} {policy:?} {id} {version:?} {accept:?}")
    }

    /// The newest version above the active one that passes. A pin refuses; a lower version is never
    /// chosen (that is `rollback`).
    pub fn update(
        &self,
        reg: &Registry,
        policy: &Policy,
        id: &str,
        accept: &[String],
    ) -> Result<Record, StoreError> {
        todo!("{reg:?} {policy:?} {id} {accept:?}")
    }

    /// Re-reads the installed active package, re-hashes it against the recorded hash, checks the
    /// revocations of the registry and the policy and the policy's own rules. A failure
    /// quarantines the active version.
    pub fn verify(
        &self,
        id: &str,
        revoked: &[Revocation],
        policy: &Policy,
    ) -> Result<Package, StoreError> {
        todo!("{id} {revoked:?} {policy:?}")
    }

    /// The active package for use: not disabled, not quarantined, re-hashed on every call.
    pub fn resolve(&self, id: &str) -> Result<Package, StoreError> {
        todo!("{id}")
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<Record, StoreError> {
        todo!("{id} {pinned}")
    }

    pub fn set_disabled(&self, id: &str, disabled: bool) -> Result<Record, StoreError> {
        todo!("{id} {disabled}")
    }

    /// Makes the previous version active again.
    pub fn rollback(&self, id: &str) -> Result<Record, StoreError> {
        todo!("{id}")
    }

    /// Removes the record and every installed version of it.
    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        todo!("{id}")
    }

    pub fn refusal_to_error(r: &Refusal) -> StoreError {
        todo!("{r:?}")
    }
}
