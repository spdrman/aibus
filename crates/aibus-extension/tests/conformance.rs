//! Fixture-based conformance and the authoring round trip (#286): a third party copies the
//! template, packages, tests and installs it with no change to core source.
#![cfg(unix)]

mod common;

use std::fs;

use aibus_extension::conformance;
use aibus_extension::package;
use aibus_extension::policy::Policy;
use aibus_extension::registry::Registry;
use aibus_extension::store::Store;
use common::{package_with, reference_dir, template_as, template_dir};

fn names(r: &conformance::Report) -> Vec<String> {
    r.checks
        .iter()
        .map(|c| format!("{}={}", c.name, c.passed))
        .collect()
}

#[test]
fn the_template_passes_its_own_fixtures() {
    let r = conformance::run(&package::read(&template_dir()).unwrap());
    assert!(r.passed(), "{:#?}", r.checks);
    assert!(r.checks.len() >= 8, "{:#?}", names(&r));
}

#[test]
fn the_reference_extension_passes_its_recorded_streams_offline() {
    let r = conformance::run(&package::read(&reference_dir()).unwrap());
    assert!(r.passed(), "{:#?}", r.checks);
}

/// Spec 6.1's layout names `basic` and `native-subagent`; both are there and both have a verdict.
#[test]
fn the_reference_extension_ships_a_basic_and_a_native_subagent_fixture() {
    let r = conformance::run(&package::read(&reference_dir()).unwrap());
    for f in ["basic", "native-subagent"] {
        assert!(
            r.checks.iter().any(|c| c.name.contains(f) && c.passed),
            "{f}: {:?}",
            names(&r)
        );
    }
}

#[test]
fn a_fixture_whose_expectation_is_wrong_fails_by_name() {
    let t = tempfile::tempdir().unwrap();
    let p = package_with(t.path(), "p", &common::template_manifest());
    fs::write(
        p.join("fixtures/basic.expected.json"),
        r#"{"ending":"exit:0","status":"failed","kinds":["attempt_started"]}"#,
    )
    .unwrap();
    let r = conformance::run(&package::read(&p).unwrap());
    assert!(!r.passed());
    assert!(
        r.checks
            .iter()
            .any(|c| !c.passed && c.name.contains("basic")),
        "{:?}",
        names(&r)
    );
}

#[test]
fn a_fixture_with_no_expected_file_fails() {
    let t = tempfile::tempdir().unwrap();
    let p = package_with(t.path(), "p", &common::template_manifest());
    fs::remove_file(p.join("fixtures/basic.expected.json")).unwrap();
    let r = conformance::run(&package::read(&p).unwrap());
    assert!(!r.passed());
}

/// The obligations every extension owes the trace: unique ascending uuid v7 ids, an evidence level
/// on every event, and a data quality on every metric; missing usage stays unknown.
#[test]
fn the_runner_checks_ordering_evidence_and_metric_quality_and_names_them() {
    let r = conformance::run(&package::read(&template_dir()).unwrap());
    for want in [
        "event ids ascend",
        "every event has an evidence level",
        "every metric has a data quality",
        "usage that is missing stays unknown",
    ] {
        assert!(
            r.checks.iter().any(|c| c.name.contains(want) && c.passed),
            "{want}: {:?}",
            names(&r)
        );
    }
}

/// The acceptance criterion: write, package, test and install an extension without touching core.
#[test]
fn a_third_party_can_write_package_test_and_install_an_extension() {
    let t = tempfile::tempdir().unwrap();
    // "Write": copy the template under a new id.
    let id = "org.thirdparty.aibus.mycli";
    let mine = package_with(t.path(), "mycli", &template_as(id, "1.0.0"));
    // "Test": the fixtures pass offline.
    assert!(conformance::run(&package::read(&mine).unwrap()).passed());
    // "Package": publish into a registry, which records the content hash.
    let reg = t.path().join("registry");
    let entry = Registry::publish(&reg, &mine).unwrap();
    assert!(entry.hash.starts_with("sha256-tree-v1:"));
    // "Install": through the store, then the installed copy passes the same fixtures.
    let store = Store::open(&t.path().join("store"));
    let rec = store
        .install(
            &Registry::open(&reg).unwrap(),
            &Policy::default(),
            id,
            None,
            &[],
        )
        .unwrap();
    assert_eq!(rec.active.as_deref(), Some("1.0.0"));
    assert!(conformance::run(&store.resolve(id).unwrap()).passed());
}

#[test]
fn the_declarative_engine_labels_what_it_cannot_map_and_never_drops_it() {
    use aibus_trace::EventKind;
    let t = tempfile::tempdir().unwrap();
    let p = package_with(t.path(), "p", &common::template_manifest());
    let pkg = package::read(&p).unwrap();
    let out = conformance::observe(&pkg, "fixtures/failed-tool.jsonl", "exit:0").unwrap();
    // The unknown line is a warning, the non-JSON line is a gap, the failed tool is a failure.
    assert!(out.kinds.contains(&EventKind::TraceWarning));
    assert!(out.kinds.contains(&EventKind::GapDetected));
    assert!(out.kinds.contains(&EventKind::ToolFailed));
}

#[test]
fn a_nonzero_exit_or_a_cancel_ends_the_attempt_failed_or_cancelled_never_completed() {
    use aibus_trace::AttemptStatus;
    let t = tempfile::tempdir().unwrap();
    let p = package_with(t.path(), "p", &common::template_manifest());
    let pkg = package::read(&p).unwrap();
    assert_eq!(
        conformance::observe(&pkg, "fixtures/basic.jsonl", "exit:2")
            .unwrap()
            .status,
        AttemptStatus::Failed
    );
    assert_eq!(
        conformance::observe(&pkg, "fixtures/basic.jsonl", "cancelled")
            .unwrap()
            .status,
        AttemptStatus::Cancelled
    );
    assert_eq!(
        conformance::observe(&pkg, "fixtures/basic.jsonl", "wall_time")
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert_eq!(
        conformance::observe(&pkg, "fixtures/basic.jsonl", "exit:0")
            .unwrap()
            .status,
        AttemptStatus::Completed
    );
}
