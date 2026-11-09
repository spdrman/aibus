//! The engine integration suite of 10 section 4: one named test per bullet, each booting a real
//! run (or a real lead with its orchestrator) over temp XDG directories and the fake provider,
//! then asserting on the messages, the graph and the files.
//!
//! Every test holds a `Rig` or a `Solo`, and dropping one is the "credential never appears"
//! check: the captured log and every file under the temp tree are searched for the fake key.
//! The suite runs on the mini backend; the replay and conformance suites are what run on both.

#[path = "../common/mod.rs"]
mod common;

mod accounts;
mod assist;
mod continuity;
mod delegation;
mod lifecycle;
mod rig;
mod single_run;
mod solo;
