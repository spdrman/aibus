//! #404: the causl-engine-core pin in `crates/aibus-graph-causl` is the commit behind causl-core-rs v0.9.0.
//! That crate is outside the workspace and needs a token to build, so this reads its files as text.

use std::fs;
use std::path::Path;

const V0_9_0: &str = "afd39707c938772ee3a45dd1714d0d7a8be23747";

fn read(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::read_to_string(root.join(rel)).unwrap()
}

#[test]
fn manifest_pins_the_v0_9_0_commit() {
    let manifest = read("crates/aibus-graph-causl/Cargo.toml");
    let line = manifest
        .lines()
        .find(|l| l.starts_with("causl-engine-core"))
        .unwrap();
    assert!(line.contains(&format!("rev = \"{V0_9_0}\"")), "{line}");
}

#[test]
fn lock_resolves_the_same_commit() {
    let lock = read("crates/aibus-graph-causl/Cargo.lock");
    let at = lock.find("name = \"causl-engine-core\"").unwrap();
    let source = lock[at..]
        .lines()
        .find(|l| l.starts_with("source = "))
        .unwrap();
    assert!(
        source.contains(&format!("?rev={V0_9_0}#{V0_9_0}")),
        "{source}"
    );
}

#[test]
fn build_doc_names_the_same_commit_and_tag() {
    let doc = read("docs/BUILD.md");
    assert!(doc.contains(&format!("rev = \"{V0_9_0}\"")) && doc.contains("(tag v0.9.0)"));
}
