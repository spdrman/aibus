//! The schema files under `docs/schemas/extension/` (#286) against what the code accepts and
//! produces: the template and reference manifests, a published registry index, the events the
//! engines make for the recorded streams, and the fixture expectations. The checker is
//! `common/schema.rs`, which fails on a keyword it does not enforce.

mod common;

use std::path::{Path, PathBuf};

use aibus_extension::manifest::Capability;
use aibus_extension::package;
use aibus_extension::registry::Registry;
use aibus_trace::EventKind;
use common::schema::{check_against, lint_2020_12, load_from};
use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema(name: &str) -> Value {
    load_from("extension", name)
}

fn manifest_json(dir: &str) -> Value {
    let text = std::fs::read_to_string(repo().join(dir).join("aibus-extension.toml")).unwrap();
    toml::from_str::<Value>(&text).unwrap()
}

const EXAMPLES: [&str; 2] = [
    "examples/extensions/template",
    "examples/extensions/reference-cli",
];

#[test]
fn every_schema_is_2020_12_and_lints_clean() {
    for n in [
        "manifest-v1",
        "registry-v1",
        "event-v1",
        "fixture-expected-v1",
    ] {
        let s = schema(n);
        assert_eq!(
            s["$schema"], "https://json-schema.org/draft/2020-12/schema",
            "{n}"
        );
        assert_eq!(lint_2020_12(&s), Vec::<String>::new(), "{n}");
    }
}

#[test]
fn the_template_and_the_reference_manifest_validate() {
    let s = schema("manifest-v1");
    for dir in EXAMPLES {
        assert_eq!(
            check_against(&s, &manifest_json(dir)),
            Vec::<String>::new(),
            "{dir}"
        );
    }
}

#[test]
fn the_schema_refuses_what_the_host_refuses() {
    let s = schema("manifest-v1");
    let base = manifest_json(EXAMPLES[0]);
    type Edit = fn(&mut Value);
    let cases: [(&str, Edit); 7] = [
        ("an extra key", |v| v["surprise"] = json!(1)),
        ("credential access", |v| {
            v["permissions"]["read_provider_credentials"] = json!(true)
        }),
        ("network from the parser", |v| {
            v["permissions"]["external_network_from_parser"] = json!(true)
        }),
        ("a prompt in argv", |v| {
            v["execution"]["prompt"] = json!("argv")
        }),
        ("a path as an executable", |v| {
            v["cli"]["executable_candidates"] = json!(["/bin/sh"])
        }),
        ("an http source", |v| {
            v["source_url"] = json!("http://example.org")
        }),
        ("a missing capability", |v| {
            v["capabilities"]
                .as_object_mut()
                .unwrap()
                .remove("hook_channel");
        }),
    ];
    for (what, edit) in cases {
        let mut v = base.clone();
        edit(&mut v);
        assert!(
            !check_against(&s, &v).is_empty(),
            "the schema accepted {what}"
        );
    }
}

#[test]
fn the_schema_names_the_same_capabilities_and_event_kinds_as_the_code() {
    let template = manifest_json(EXAMPLES[0]);
    let caps: Vec<String> = template["capabilities"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let m = schema("manifest-v1");
    let listed: Vec<String> = m["properties"]["capabilities"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let mut listed = listed;
    listed.sort();
    assert_eq!(listed, caps, "the schema's list is the template's list");
    // And the code reads the same names: each one is a variant.
    for c in &caps {
        serde_json::from_value::<Capability>(json!(c)).unwrap();
    }
    let kinds = [
        EventKind::WorkflowStarted,
        EventKind::StepScheduled,
        EventKind::RouteDecided,
        EventKind::ApprovalRequested,
        EventKind::ApprovalResolved,
        EventKind::AttemptStarted,
        EventKind::AttemptCompleted,
        EventKind::AttemptFailed,
        EventKind::AgentStarted,
        EventKind::AgentStopped,
        EventKind::TaskDelegated,
        EventKind::MessageChunk,
        EventKind::ToolStarted,
        EventKind::ToolCompleted,
        EventKind::ToolFailed,
        EventKind::CheckpointWritten,
        EventKind::ArtifactCreated,
        EventKind::UsageReported,
        EventKind::AuthRequired,
        EventKind::GapDetected,
        EventKind::TraceWarning,
    ];
    let wire: Vec<Value> = kinds
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
    let e = schema("event-v1");
    assert_eq!(e["properties"]["kind"]["enum"].as_array().unwrap(), &wire);
}

#[test]
fn a_published_registry_index_validates_and_a_bad_hash_does_not() {
    let t = tempfile::tempdir().unwrap();
    for dir in EXAMPLES {
        Registry::publish(t.path(), &repo().join(dir)).unwrap();
    }
    let text = std::fs::read_to_string(t.path().join("aibus-registry.json")).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    let s = schema("registry-v1");
    assert_eq!(check_against(&s, &v), Vec::<String>::new());
    v["entries"][0]["hash"] = json!("md5:abc");
    assert!(!check_against(&s, &v).is_empty());
}

#[test]
fn the_events_both_engines_make_for_the_recorded_streams_match_the_event_contract() {
    let s = schema("event-v1");
    for dir in EXAMPLES {
        let pkg = package::read(&repo().join(dir)).unwrap();
        for f in &pkg.manifest.fixtures.files {
            let o = aibus_extension::conformance::observe(&pkg, f, "exit:0").unwrap();
            for e in &o.events {
                let v = serde_json::to_value(e).unwrap();
                assert_eq!(
                    check_against(&s, &v),
                    Vec::<String>::new(),
                    "{dir} {f}: {v}"
                );
            }
        }
    }
}

#[test]
fn every_expected_file_validates() {
    let s = schema("fixture-expected-v1");
    for dir in EXAMPLES {
        let pkg = package::read(&repo().join(dir)).unwrap();
        for f in &pkg.manifest.fixtures.files {
            let p = repo().join(dir).join(f.replace(".jsonl", ".expected.json"));
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            assert_eq!(
                check_against(&s, &v),
                Vec::<String>::new(),
                "{}",
                p.display()
            );
        }
    }
}

/// The template with its rules swapped for a WASM module (#333), as TOML text.
fn wasm_manifest_text() -> String {
    let m = std::fs::read_to_string(repo().join(EXAMPLES[0]).join("aibus-extension.toml")).unwrap();
    let (from, to) = (
        m.find("[normalizer]").unwrap(),
        m.find("# Every capability").unwrap(),
    );
    format!(
        "{}[normalizer]\nengine = \"wasm-module\"\nmodule = \"wasm/parser.wasm\"\n\n{}",
        &m[..from],
        &m[to..]
    )
}

#[test]
fn a_manifest_naming_a_wasm_module_validates_and_a_module_that_climbs_does_not() {
    let s = schema("manifest-v1");
    let v: Value = toml::from_str(&wasm_manifest_text()).unwrap();
    assert_eq!(check_against(&s, &v), Vec::<String>::new());
    for bad in [
        "wasm/../x.wasm",
        "/etc/x.wasm",
        "other/x.wasm",
        "wasm/x.bin",
    ] {
        let mut v = v.clone();
        v["normalizer"]["module"] = json!(bad);
        assert!(
            !check_against(&s, &v).is_empty(),
            "the schema accepted {bad}"
        );
    }
}

#[cfg(feature = "wasm")]
#[test]
fn the_events_a_wasm_guest_makes_match_the_event_contract() {
    use std::collections::BTreeMap;
    let template = repo().join(EXAMPLES[0]);
    let mut files = BTreeMap::new();
    for f in ["LICENSE", "README.md"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            std::fs::read_dir(template.join("fixtures"))
                .unwrap()
                .map(|e| format!("fixtures/{}", e.unwrap().file_name().to_string_lossy())),
        )
    {
        files.insert(f.clone(), std::fs::read(template.join(&f)).unwrap());
    }
    files.insert(
        "aibus-extension.toml".into(),
        wasm_manifest_text().into_bytes(),
    );
    files.insert(
        "wasm/parser.wasm".into(),
        std::fs::read(repo().join("crates/aibus-extension/tests/wasm-guest/parser.wasm")).unwrap(),
    );
    let pkg = package::Package::from_files(files).unwrap();
    let s = schema("event-v1");
    for f in &pkg.manifest.fixtures.files {
        let o = aibus_extension::conformance::observe(&pkg, f, "exit:0").unwrap();
        for e in &o.events {
            let v = serde_json::to_value(e).unwrap();
            assert_eq!(check_against(&s, &v), Vec::<String>::new(), "{f}: {v}");
        }
    }
}
