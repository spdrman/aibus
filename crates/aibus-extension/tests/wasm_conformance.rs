//! A parser written as a WASM guest produces the same normalised events as the declarative engine
//! for the reference stream, and the conformance runner runs it (#333).
#![cfg(all(unix, feature = "wasm"))]

mod common;

use aibus_extension::conformance;
use aibus_extension::package::{self, Package};
use common::{guest_wasm, package_with, template_manifest, wasm_manifest, wasm_package};

fn comparable(mut v: serde_json::Value) -> serde_json::Value {
    // The only field that names the engine: the parser that produced the event.
    for e in v.as_array_mut().unwrap() {
        e["source"]["parser"] = "x".into();
    }
    v
}

fn events(pkg: &Package, fixture: &str, ending: &str) -> serde_json::Value {
    let o = conformance::observe(pkg, fixture, ending).unwrap();
    comparable(serde_json::to_value(o.events).unwrap())
}

#[test]
fn the_guest_and_the_declarative_engine_agree_on_every_event_of_the_reference_streams() {
    let t = tempfile::tempdir().unwrap();
    let d = package::read(&package_with(t.path(), "decl", &template_manifest())).unwrap();
    let w = package::read(&wasm_package(
        t.path(),
        "wasm",
        &wasm_manifest(),
        &guest_wasm(),
    ))
    .unwrap();
    for (f, ending) in [
        ("fixtures/basic.jsonl", "exit:0"),
        ("fixtures/failed-tool.jsonl", "exit:1"),
        ("fixtures/unfinished-tool.jsonl", "exit:0"),
        ("fixtures/unfinished-tool.jsonl", "wall_time"),
        ("fixtures/basic.jsonl", "cancelled"),
    ] {
        assert_eq!(events(&d, f, ending), events(&w, f, ending), "{f} {ending}");
    }
}

#[test]
fn the_conformance_runner_runs_a_wasm_package_and_it_passes() {
    let t = tempfile::tempdir().unwrap();
    let w = package::read(&wasm_package(
        t.path(),
        "wasm",
        &wasm_manifest(),
        &guest_wasm(),
    ))
    .unwrap();
    let r = conformance::run(&w);
    assert!(r.passed(), "{:#?}", r.checks);
    assert!(r.checks.len() >= 8);
}

#[test]
fn a_guest_that_traps_fails_conformance_by_name_and_does_not_hang_it() {
    let trap = wat::parse_str(
        r#"(module
             (import "aibus" "read_line" (func (param i32 i32) (result i32)))
             (memory (export "memory") 1)
             (func (export "on_line") (result i32) unreachable)
             (func (export "on_finish") (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    let t = tempfile::tempdir().unwrap();
    let w = package::read(&wasm_package(t.path(), "trap", &wasm_manifest(), &trap)).unwrap();
    let r = conformance::run(&w);
    assert!(!r.passed());
    assert!(r
        .checks
        .iter()
        .any(|c| !c.passed && c.name.contains("matches its expected events")));
}

#[test]
fn the_checked_in_guest_is_the_one_its_recorded_hash_names() {
    let sum = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/wasm-guest/parser.wasm.sha256"),
    )
    .expect("tests/wasm-guest/parser.wasm.sha256 is written by build.sh");
    assert_eq!(sum.trim(), aibus_trace::sha256_hex(&guest_wasm()));
}
