//! The installed store: `<root>/state.json` and `<root>/packages/<id>/<version>/`. Install,
//! verify, list, update, pin, disable, enable, remove and rollback, each one atomic at the level of
//! the state file. Nothing is activated that did not pass the registry check, and an installed
//! package is re-hashed every time it is resolved, so a file changed on disk after the install
//! quarantines it instead of running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::{escalations, valid_id};
use crate::package::{self, Package};
use crate::policy::Policy;
use crate::registry::{self, Entry, Registry, Revocation};
use crate::version::{exact, Version};

/// `state.json` larger than this is not read.
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

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
#[serde(deny_unknown_fields)]
pub struct State {
    pub extensions: BTreeMap<String, Record>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The package was refused and a quarantine record was written; nothing was activated.
    #[error("quarantined ({class}): {reason}")]
    Quarantined { class: String, reason: String },
    #[error("{0:?} is not an extension id")]
    BadId(String),
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

fn bad_id(id: &str) -> Result<(), StoreError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(StoreError::BadId(id.chars().take(80).collect()))
    }
}

fn quarantine(rec: &mut Record, version: &str, hash: &str, class: &str, reason: &str) {
    let q = Quarantined {
        version: version.to_owned(),
        hash: hash.to_owned(),
        class: class.to_owned(),
        reason: reason.chars().take(500).collect(),
    };
    if !rec
        .quarantined
        .iter()
        .any(|x| x.version == q.version && x.hash == q.hash && x.class == q.class)
    {
        rec.quarantined.push(q);
    }
    if rec.active.as_deref() == Some(version) {
        rec.active = None;
    }
}

impl Store {
    pub fn open(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }

    fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// The installed state; an absent file is an empty store, a damaged one is an error.
    pub fn state(&self) -> Result<State, StoreError> {
        let bytes = match aibus_core::persist::fs::read_regular(&self.state_file(), MAX_STATE_BYTES)
        {
            Ok(b) => b,
            Err(aibus_core::persist::fs::ReadError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(State::default());
            }
            Err(e) => return Err(StoreError::State(e.to_string())),
        };
        let st: State =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::State(e.to_string()))?;
        let hash = regex::Regex::new(r"^sha256-tree-v1:[0-9a-f]{64}$").expect("valid regex");
        for (id, rec) in &st.extensions {
            let versions_ok = rec
                .versions
                .iter()
                .all(|(v, h)| exact(v).is_some() && hash.is_match(h))
                && rec
                    .active
                    .iter()
                    .chain(&rec.previous)
                    .all(|v| exact(v).is_some());
            if !valid_id(id) || !versions_ok {
                return Err(StoreError::State(format!(
                    "the record for {id:?} is malformed"
                )));
            }
        }
        Ok(st)
    }

    /// Read, change and write the state under one lock. The state is saved whatever `change`
    /// returns, so a quarantine it recorded before failing is kept.
    fn transact<T>(
        &self,
        change: impl FnOnce(&mut State) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        aibus_core::persist::fs::create_private_dir(&self.root)?;
        let mut out = None;
        aibus_core::persist::fs::update_locked(&self.root.join("state.lock"), |_| {
            out = Some((|| {
                let mut st = self.state()?;
                let r = change(&mut st);
                let text =
                    serde_json::to_vec_pretty(&st).map_err(|e| StoreError::State(e.to_string()))?;
                aibus_core::persist::fs::write_atomic(&self.state_file(), text)?;
                r
            })());
            Ok(String::new())
        })?;
        out.expect("the closure ran")
    }

    fn version_dir(&self, id: &str, version: &str) -> PathBuf {
        self.root.join("packages").join(id).join(version)
    }

    /// The installed copy of `id` at `version`, read without following a link and checked against
    /// the hash recorded at install. `Err(None)` is a link or a hash that moved (tampered).
    fn read_installed(
        &self,
        id: &str,
        version: &str,
        hash: &str,
    ) -> Result<Package, Box<(String, String)>> {
        let tampered = |why: String| Box::new(("tampered".to_owned(), why));
        let parent = self.root.join("packages").join(id);
        if std::fs::symlink_metadata(&parent).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(tampered(format!("{} is a link", parent.display())));
        }
        let pkg =
            package::read(&self.version_dir(id, version)).map_err(|e| tampered(e.to_string()))?;
        if pkg.hash != hash {
            return Err(tampered(format!(
                "it now hashes to {}, not {hash}",
                pkg.hash
            )));
        }
        Ok(pkg)
    }

    /// Installs one registry entry: the checks, then the files, then the record.
    fn install_entry(
        &self,
        st: &mut State,
        reg: &Registry,
        policy: &Policy,
        entry: &Entry,
        accept: &[String],
    ) -> Result<Record, StoreError> {
        let rec = st.extensions.entry(entry.id.clone()).or_default();
        let new_version = exact(&entry.version).expect("the registry validated it");
        if let Some(active) = rec.active.as_deref().and_then(exact) {
            if new_version < active {
                return Err(StoreError::Downgrade {
                    from: active.to_string(),
                    to: entry.version.clone(),
                });
            }
        }
        let pkg = match reg.fetch(entry, policy) {
            Ok(p) => p,
            Err(refusal) => {
                let class = refusal.class();
                quarantine(
                    rec,
                    &entry.version,
                    &entry.hash,
                    class,
                    &refusal.to_string(),
                );
                return Err(StoreError::Quarantined {
                    class: class.to_owned(),
                    reason: refusal.to_string(),
                });
            }
        };
        if let Some(old_version) = rec.active.clone().filter(|v| *v != entry.version) {
            if let Some(old) = rec
                .versions
                .get(&old_version)
                .and_then(|h| self.read_installed(&entry.id, &old_version, h).ok())
            {
                let unaccepted: Vec<String> = escalations(&old.manifest, &pkg.manifest)
                    .into_iter()
                    .filter(|e| !accept.contains(e))
                    .collect();
                if !unaccepted.is_empty() {
                    return Err(StoreError::Escalation(unaccepted));
                }
            }
        }
        let target = self.version_dir(&entry.id, &entry.version);
        if self
            .read_installed(&entry.id, &entry.version, &pkg.hash)
            .is_err()
        {
            let parent = self.root.join("packages").join(&entry.id);
            if std::fs::symlink_metadata(&parent).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(StoreError::State(format!("{} is a link", parent.display())));
            }
            aibus_core::persist::fs::create_private_dir(&parent)?;
            let stage = parent.join(format!(".stage-{}", uuid::Uuid::now_v7()));
            pkg.write(&stage)?;
            if std::fs::symlink_metadata(&target).is_ok() {
                // Something else is there (a leftover, or a copy that no longer matches): set it
                // aside, never write into it.
                let _ = std::fs::rename(
                    &target,
                    parent.join(format!(".old-{}", uuid::Uuid::now_v7())),
                );
            }
            std::fs::rename(&stage, &target)?;
        }
        if rec.active.as_deref() != Some(entry.version.as_str()) {
            rec.previous = rec.active.take().filter(|v| *v != entry.version);
        }
        rec.versions.insert(entry.version.clone(), pkg.hash);
        rec.active = Some(entry.version.clone());
        Ok(rec.clone())
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
        bad_id(id)?;
        let entry = reg
            .find(id, version)
            .ok_or_else(|| StoreError::NotInRegistry(id.to_owned()))?
            .clone();
        self.transact(|st| self.install_entry(st, reg, policy, &entry, accept))
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
        bad_id(id)?;
        self.transact(|st| {
            let rec = st
                .extensions
                .get(id)
                .ok_or_else(|| StoreError::NotInstalled(id.to_owned()))?;
            if rec.pinned {
                return Err(StoreError::Pinned(id.to_owned()));
            }
            let floor = rec.active.as_deref().and_then(exact);
            let mut newer: Vec<Entry> = reg
                .search(id)
                .into_iter()
                .filter(|e| {
                    e.id == id && floor.is_none_or(|f| exact(&e.version).is_some_and(|v| v > f))
                })
                .cloned()
                .collect();
            if newer.is_empty() {
                return Err(StoreError::UpToDate(rec.active.clone().unwrap_or_default()));
            }
            // `search` is newest first. A candidate that is quarantined is passed over for the
            // next newest; an escalation or a store error is the user's to see.
            let mut last = None;
            for entry in newer.drain(..) {
                match self.install_entry(st, reg, policy, &entry, accept) {
                    Err(e @ StoreError::Quarantined { .. }) => last = Some(e),
                    other => return other,
                }
            }
            Err(last.expect("at least one candidate"))
        })
    }

    /// Re-reads the installed active package, re-hashes it against the recorded hash, checks the
    /// revocations of the registry and the policy and the policy's own rules. A failure
    /// quarantines the active version. (A signature is checked at install; the policy's
    /// `require_signature` is not asked of an install that is already there.)
    pub fn verify(
        &self,
        id: &str,
        revoked: &[Revocation],
        policy: &Policy,
    ) -> Result<Package, StoreError> {
        bad_id(id)?;
        self.transact(|st| {
            let rec = st
                .extensions
                .get_mut(id)
                .ok_or_else(|| StoreError::NotInstalled(id.to_owned()))?;
            let version = rec
                .active
                .clone()
                .ok_or_else(|| StoreError::ActiveQuarantined(id.to_owned()))?;
            let hash = rec.versions.get(&version).cloned().unwrap_or_default();
            let fail = |rec: &mut Record, class: &str, why: String| {
                quarantine(rec, &version, &hash, class, &why);
                StoreError::Quarantined {
                    class: class.to_owned(),
                    reason: why,
                }
            };
            let pkg = match self.read_installed(id, &version, &hash) {
                Ok(p) => p,
                Err(b) => return Err(fail(rec, &b.0, b.1)),
            };
            let entry = Entry {
                id: id.to_owned(),
                version: version.clone(),
                hash: hash.clone(),
                path: String::new(),
                signature: None,
            };
            let relaxed = Policy {
                require_signature: false,
                ..policy.clone()
            };
            match registry::check(&pkg, &entry, revoked, &relaxed) {
                Ok(()) => Ok(pkg),
                Err(r) => Err(fail(rec, r.class(), r.to_string())),
            }
        })
    }

    /// The active package for use: not disabled, not quarantined, re-hashed on every call.
    pub fn resolve(&self, id: &str, policy: &Policy) -> Result<Package, StoreError> {
        let _ = policy;
        bad_id(id)?;
        let st = self.state()?;
        let rec = st
            .extensions
            .get(id)
            .ok_or_else(|| StoreError::NotInstalled(id.to_owned()))?;
        if rec.disabled {
            return Err(StoreError::Disabled(id.to_owned()));
        }
        let version = rec
            .active
            .clone()
            .ok_or_else(|| StoreError::ActiveQuarantined(id.to_owned()))?;
        let hash = rec.versions.get(&version).cloned().unwrap_or_default();
        match self.read_installed(id, &version, &hash) {
            Ok(p) => Ok(p),
            Err(b) => self.transact(|st| {
                if let Some(rec) = st.extensions.get_mut(id) {
                    quarantine(rec, &version, &hash, &b.0, &b.1);
                }
                Err(StoreError::Quarantined {
                    class: b.0.clone(),
                    reason: b.1.clone(),
                })
            }),
        }
    }

    fn edit(&self, id: &str, f: impl FnOnce(&mut Record)) -> Result<Record, StoreError> {
        bad_id(id)?;
        self.transact(|st| {
            let rec = st
                .extensions
                .get_mut(id)
                .ok_or_else(|| StoreError::NotInstalled(id.to_owned()))?;
            f(rec);
            Ok(rec.clone())
        })
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<Record, StoreError> {
        self.edit(id, |r| r.pinned = pinned)
    }

    pub fn set_disabled(&self, id: &str, disabled: bool) -> Result<Record, StoreError> {
        self.edit(id, |r| r.disabled = disabled)
    }

    /// Makes the previous version active again, after re-hashing it.
    pub fn rollback(&self, id: &str, policy: &Policy) -> Result<Record, StoreError> {
        let _ = policy;
        bad_id(id)?;
        self.transact(|st| {
            let rec = st
                .extensions
                .get_mut(id)
                .ok_or_else(|| StoreError::NotInstalled(id.to_owned()))?;
            let prev = rec
                .previous
                .clone()
                .ok_or_else(|| StoreError::NoPrevious(id.to_owned()))?;
            let hash = rec.versions.get(&prev).cloned().unwrap_or_default();
            if let Err(b) = self.read_installed(id, &prev, &hash) {
                quarantine(rec, &prev, &hash, &b.0, &b.1);
                return Err(StoreError::Quarantined {
                    class: b.0,
                    reason: b.1,
                });
            }
            rec.active = Some(prev);
            rec.previous = None;
            Ok(rec.clone())
        })
    }

    /// Removes the record and every installed version of it. A link where the files should be is
    /// removed as a link and never followed.
    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        bad_id(id)?;
        self.transact(|st| {
            st.extensions
                .remove(id)
                .ok_or_else(|| StoreError::NotInstalled(id.to_owned()))?;
            let dir = self.root.join("packages").join(id);
            match std::fs::symlink_metadata(&dir) {
                Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&dir)?,
                Ok(_) => std::fs::remove_dir_all(&dir)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            Ok(())
        })
    }
}
