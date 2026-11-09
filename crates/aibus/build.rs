//! Stamps the binary with the tag and commit it was built from, for `aibus version` and
//! `aibus doctor`. The release workflow sets `AIBUS_TAG` and `AIBUS_COMMIT`; a build from a git
//! checkout asks git; a build from a crates.io tarball has neither and says `unknown`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    println!("cargo:rerun-if-env-changed=AIBUS_TAG");
    println!("cargo:rerun-if-env-changed=AIBUS_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let tag = std::env::var("AIBUS_TAG")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| git(&["describe", "--tags", "--exact-match"]))
        .unwrap_or_default();
    let commit = std::env::var("AIBUS_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AIBUS_BUILD_TAG={tag}");
    println!("cargo:rustc-env=AIBUS_BUILD_COMMIT={commit}");
}
