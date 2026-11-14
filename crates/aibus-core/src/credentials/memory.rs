//! An in-memory keychain for tests and for the CLI tests: nothing touches the OS.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aibus_provider::Secret;

use super::{Keychain, KeychainError, StoreHealth};

#[derive(Default)]
pub struct MemoryKeychain {
    entries: Mutex<BTreeMap<(String, String), String>>,
    /// Why every call fails, for a machine with no keychain.
    down: Option<String>,
}

impl MemoryKeychain {
    pub fn unavailable(reason: &str) -> Self {
        Self {
            down: Some(reason.to_owned()),
            ..Self::default()
        }
    }

    fn check(&self) -> Result<(), KeychainError> {
        match &self.down {
            Some(r) => Err(KeychainError::Unavailable(r.clone())),
            None => Ok(()),
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), String>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Keychain for MemoryKeychain {
    fn get(&self, service: &str, user: &str) -> Result<Option<Secret<String>>, KeychainError> {
        self.check()?;
        Ok(self
            .entries()
            .get(&(service.to_owned(), user.to_owned()))
            .map(|v| Secret::new(v.clone())))
    }

    fn set(&self, service: &str, user: &str, secret: &Secret<String>) -> Result<(), KeychainError> {
        self.check()?;
        self.entries()
            .insert((service.to_owned(), user.to_owned()), secret.expose().clone());
        Ok(())
    }

    fn delete(&self, service: &str, user: &str) -> Result<(), KeychainError> {
        self.check()?;
        self.entries().remove(&(service.to_owned(), user.to_owned()));
        Ok(())
    }

    fn probe(&self) -> StoreHealth {
        match &self.down {
            Some(r) => StoreHealth::Unavailable(r.clone()),
            None => StoreHealth::Available("memory".into()),
        }
    }
}
