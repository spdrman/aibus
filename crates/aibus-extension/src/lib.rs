//! The extension SDK (#286, spec 6.1 to 6.3), amended by the Phase 1 gate (A1.5, ratified
//! 2026-11-15).
//!
//! An extension is a directory: `aibus-extension.toml`, a licence, a README, fixtures and, for the
//! declarative engine, nothing else. It never runs inside aibus. The host reads it with
//! [`package::read`] (hostile input: names, links, sizes), pins it by content hash, checks it
//! against the registry, policy and revocation list ([`registry`], [`policy`]), keeps it in the
//! [`store`], negotiates what it can do against the real CLI ([`negotiate`]) and turns it into one
//! launch ([`sandbox`]) of a program on the host's allowlist of official CLIs. The normaliser that reads the CLI's events is
//! an [`aibus_supervisor::Engine`]: the host's own for the built-in Claude parser, or the
//! declarative one in [`declarative`] for CLIs whose events are one line, one event. A stateful
//! parser for any other CLI needs a WASI component, which this release does not host (see
//! `docs/spec/16-extensions.md`).
#![forbid(unsafe_code)]

pub mod conformance;
pub mod declarative;
pub mod manifest;
pub mod negotiate;
pub mod package;
pub mod policy;
pub mod registry;
pub mod sandbox;
pub mod store;
pub mod version;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use manifest::{Capability, Manifest, ManifestError, Support};
pub use package::{Package, PackageError};
