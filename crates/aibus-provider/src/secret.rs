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
    /// A short-lived access token the credential store refreshed; sent like a bearer token.
    OAuth,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Says when it was zeroised, which a `String` cannot (its buffer is gone after the drop).
    struct Flag(Arc<AtomicBool>);

    impl Zeroize for Flag {
        fn zeroize(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn dropping_a_secret_zeroises_it() {
        let wiped = Arc::new(AtomicBool::new(false));
        let s = Secret::new(Flag(wiped.clone()));
        assert!(!wiped.load(Ordering::SeqCst));
        drop(s);
        assert!(wiped.load(Ordering::SeqCst));
    }

    #[test]
    fn debug_prints_three_stars_for_a_secret_and_hides_it_in_a_credential() {
        let s = Secret::new("sk-very-secret".to_owned());
        assert_eq!(format!("{s:?}"), "***");
        let c = Credential {
            account: AccountName("work".into()),
            secret: s,
            kind: AuthMethodKind::ApiKey,
            resolved_at: Instant::now(),
        };
        let text = format!("{c:?}");
        assert!(
            text.contains("***") && !text.contains("sk-very-secret"),
            "{text}"
        );
        assert_eq!(format!("{:#?}", c.secret), "***");
    }

    /// `Secret` must not be `Serialize`: if it ever is, the two impls below both apply, the call
    /// is ambiguous and this module stops compiling.
    #[test]
    fn a_secret_cannot_be_serialised() {
        trait AmbiguousIfSerialize<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
        struct Serializable;
        impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<Serializable> for T {}
        <Secret<String> as AmbiguousIfSerialize<_>>::check();
        <Credential as AmbiguousIfSerialize<_>>::check();
    }
}
