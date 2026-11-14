//! The OS keychain through the `keyring` crate (04 section 5): macOS Keychain, Windows
//! Credential Manager, Secret Service on Linux. Nothing else in aibus links `keyring`.

use aibus_core::{Keychain, KeychainError, StoreHealth};
use aibus_provider::Secret;

pub struct OsKeychain;

/// A machine with no keychain, or one that cannot be reached, is `Unavailable`; the rest is a
/// failure of that one call. The messages come from the backend and never hold a secret.
fn map(e: keyring::Error) -> KeychainError {
    match e {
        keyring::Error::NoStorageAccess(e) | keyring::Error::PlatformFailure(e) => {
            KeychainError::Unavailable(e.to_string())
        }
        keyring::Error::BadEncoding(_) => {
            KeychainError::Failed("the stored value is not valid text".into())
        }
        other => KeychainError::Failed(other.to_string()),
    }
}

fn entry(service: &str, user: &str) -> Result<keyring::Entry, KeychainError> {
    keyring::Entry::new(service, user).map_err(map)
}

impl Keychain for OsKeychain {
    fn get(&self, service: &str, user: &str) -> Result<Option<Secret<String>>, KeychainError> {
        match entry(service, user)?.get_password() {
            Ok(p) => Ok(Some(Secret::new(p))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map(e)),
        }
    }

    fn set(&self, service: &str, user: &str, secret: &Secret<String>) -> Result<(), KeychainError> {
        entry(service, user)?
            .set_password(secret.expose())
            .map_err(map)
    }

    fn delete(&self, service: &str, user: &str) -> Result<(), KeychainError> {
        match entry(service, user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map(e)),
        }
    }

    fn probe(&self) -> StoreHealth {
        let name = if cfg!(target_os = "macos") {
            "keyring (macOS Keychain)"
        } else if cfg!(target_os = "windows") {
            "keyring (Credential Manager)"
        } else {
            "keyring (Secret Service)"
        };
        match entry("aibus", "probe").and_then(|e| match e.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map(e)),
        }) {
            Ok(()) => StoreHealth::Available(name.into()),
            Err(e) => StoreHealth::Unavailable(e.to_string()),
        }
    }
}

// The real backend is not exercised here: a container has no keychain and a developer's must not
// be touched. These cover the error mapping, and that the adapter compiles against `keyring`.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn boxed(msg: &str) -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(io::Error::other(msg.to_owned()))
    }

    #[test]
    fn a_missing_or_unreachable_keychain_is_unavailable() {
        assert_eq!(
            map(keyring::Error::NoStorageAccess(boxed("locked"))),
            KeychainError::Unavailable("locked".into())
        );
        assert_eq!(
            map(keyring::Error::PlatformFailure(boxed("no dbus"))),
            KeychainError::Unavailable("no dbus".into())
        );
    }

    #[test]
    fn other_errors_fail_the_call_and_never_echo_stored_bytes() {
        let KeychainError::Failed(m) =
            map(keyring::Error::BadEncoding(b"sk-secret-bytes".to_vec()))
        else {
            panic!("not Failed")
        };
        assert!(!m.contains("sk-secret"));
        assert!(matches!(
            map(keyring::Error::Invalid("service".into(), "empty".into())),
            KeychainError::Failed(_)
        ));
        assert!(matches!(
            map(keyring::Error::TooLong("user".into(), 255)),
            KeychainError::Failed(_)
        ));
    }

    #[test]
    fn no_entry_is_none_on_get_and_fine_on_delete() {
        // keyring's own mock store: every `Entry` starts empty.
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        assert!(OsKeychain.get("aibus", "account:none").unwrap().is_none());
        OsKeychain.delete("aibus", "account:none").unwrap();
        OsKeychain
            .set("aibus", "account:none", &Secret::new("k".into()))
            .unwrap();
        assert!(matches!(OsKeychain.probe(), StoreHealth::Available(_)));
    }
}
