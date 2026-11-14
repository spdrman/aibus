//! Shared by the integration tests: the binary run in a throwaway `$HOME`.

#![allow(dead_code)]

pub mod mcp_client;
pub mod schema;
pub mod transcript;

use std::path::Path;
use std::process::{Command, Output, Stdio};

pub const XDG: [&str; 5] = [
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "AIBUS_CONFIG",
];

/// `aibus` with `HOME` and the working directory in `home`/`proj`, no `XDG_*`, stdin closed.
pub fn command(home: &Path) -> Command {
    let proj = home.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_aibus"));
    c.env("HOME", home).current_dir(&proj).stdin(Stdio::null());
    for v in XDG {
        c.env_remove(v);
    }
    c
}

pub fn aibus(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().unwrap()
}

pub fn ndjson(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
