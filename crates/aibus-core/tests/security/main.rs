//! C2.1: the security section (09) closed against its threat table. One test per boundary, and
//! `docs/SECURITY.md` names each of them. Filesystem and process tests, so they run in the
//! container only.
//!
//! Both real provider crates are linked into this binary on purpose: the `xdg_temp` from the
//! testkit installs a log capture for every test that makes a directory, so whatever they log
//! while the engine runs is grepped for the fake key too.

use aibus_provider_anthropic as _;
use aibus_provider_openai as _;

mod boundaries;
mod bundles;
mod commands;
mod environment;
mod fuzz;
mod redaction;
mod sandbox;
