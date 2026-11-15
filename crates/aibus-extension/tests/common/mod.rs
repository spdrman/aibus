#![allow(dead_code)]
//! Shared helpers: the template and reference packages, throw-away packages built from edited
//! manifests, and registries around them.

use std::fs;
use std::path::{Path, PathBuf};

use aibus_extension::registry::Registry;

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn template_dir() -> PathBuf {
    repo().join("examples/extensions/template")
}

pub fn reference_dir() -> PathBuf {
    repo().join("examples/extensions/reference-cli")
}

pub fn template_manifest() -> String {
    fs::read_to_string(template_dir().join("aibus-extension.toml")).unwrap()
}

/// `text` with the first `from` replaced by `to`; panics when `from` is not there, so a test never
/// passes because its edit silently did nothing.
pub fn edit(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "the template has no {from:?}");
    text.replacen(from, to, 1)
}

/// A package directory under `root/name` made from the template with `manifest` in place of its
/// manifest (fixtures and licence copied as they are).
pub fn package_with(root: &Path, name: &str, manifest: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("fixtures")).unwrap();
    for f in [
        "LICENSE",
        "README.md",
        "fixtures/basic.jsonl",
        "fixtures/failed-tool.jsonl",
        "fixtures/unfinished-tool.jsonl",
    ] {
        fs::copy(template_dir().join(f), dir.join(f)).unwrap();
    }
    for f in [
        "fixtures/basic.expected.json",
        "fixtures/failed-tool.expected.json",
        "fixtures/unfinished-tool.expected.json",
    ] {
        fs::copy(template_dir().join(f), dir.join(f)).unwrap();
    }
    fs::write(dir.join("aibus-extension.toml"), manifest).unwrap();
    dir
}

/// The template at another id and version.
pub fn template_as(id: &str, version: &str) -> String {
    let m = edit(&template_manifest(), "org.example.aibus.examplecli", id);
    edit(
        &m,
        "version = \"0.1.0\"",
        &format!("version = \"{version}\""),
    )
}

/// Publishes each package directory into a registry at `reg` and opens it.
pub fn registry_of(reg: &Path, pkgs: &[PathBuf]) -> Registry {
    for p in pkgs {
        Registry::publish(reg, p).unwrap();
    }
    Registry::open(reg).unwrap()
}

/// The checked-in guest the conformance tests run (source and build script beside it).
pub fn guest_wasm() -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wasm-guest/parser.wasm"))
        .expect("tests/wasm-guest/parser.wasm is the built fixture: run tests/wasm-guest/build.sh")
}

/// The template's manifest with its declarative rules replaced by a WASM module at `wasm/parser.wasm`.
pub fn wasm_manifest() -> String {
    let m = template_manifest();
    let from = m
        .find("[normalizer]")
        .expect("the template has a [normalizer]");
    let to = m
        .find("# Every capability")
        .expect("the template lists capabilities");
    format!(
        "{}[normalizer]\nengine = \"wasm-module\"\nmodule = \"wasm/parser.wasm\"\n\n{}",
        &m[..from],
        &m[to..]
    )
}

/// A package directory under `root/name`: the template's fixtures and licence, `manifest`, and
/// `module` at `wasm/parser.wasm`.
pub fn wasm_package(root: &Path, name: &str, manifest: &str, module: &[u8]) -> PathBuf {
    let dir = package_with(root, name, manifest);
    fs::create_dir_all(dir.join("wasm")).unwrap();
    fs::write(dir.join("wasm/parser.wasm"), module).unwrap();
    dir
}
