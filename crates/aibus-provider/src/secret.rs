//! Secrets that cannot be serialised or printed (04 section 5).

use std::fmt;
use std::time::Instant;

use zeroize::{Zeroize, Zeroizing};

use crate::types::AccountName;

/// No `Serialize`, `Debug` prints `***`, memory is zeroised on drop.
pub struct Secret<T: Zeroize>(Zeroizing<T>);

impl<T: Zeroize> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(Zeroizing::new(value))
    }

    /// The one way to the value. Call it at the point of use and do not log the result.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthMethodKind {
    ApiKey,
    Bearer,
    Env,
    None,
}

/// Handed to a provider as `&Credential` for the duration of one call.
pub struct Credential {
    pub account: AccountName,
    pub secret: Secret<String>,
    pub kind: AuthMethodKind,
    pub resolved_at: Instant,
}

// Written by hand so a derive added later cannot start printing the secret field raw.
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("account", &self.account)
            .field("secret", &self.secret)
            .field("kind", &self.kind)
            .finish()
    }
}
