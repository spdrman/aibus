//! Identifiers (02 section 5). Ulid-backed ids sort by creation time and need no coordination.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use ulid::Ulid;

pub use aibus_provider::{AccountName, ModelId, ProviderId, ToolCallId};

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self)
            }
        }
    };
}

ulid_id!(
    /// One run of an agent.
    RunId
);
ulid_id!(SessionId);
ulid_id!(PermissionId);

/// Monotonic per session, matches the "forks" counter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ForkId(pub u64);

/// `[a-z0-9][a-z0-9_-]{0,63}` (01 section 1.1). The `:` ban follows from the character set and
/// matters because names end up inside graph node keys.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize)]
pub struct AgentName(SmolStr);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("invalid agent name {0:?}: want [a-z0-9][a-z0-9_-]{{0,63}}")]
pub struct InvalidAgentName(pub String);

impl AgentName {
    pub fn new(name: &str) -> Result<Self, InvalidAgentName> {
        let mut chars = name.chars();
        let first_ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest_ok =
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if first_ok && rest_ok && name.len() <= 64 {
            Ok(Self(name.into()))
        } else {
            Err(InvalidAgentName(name.chars().take(80).collect()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentName {
    type Err = InvalidAgentName;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for AgentName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(&s).map_err(serde::de::Error::custom)
    }
}
