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
    assert!(conformance::run(&store.resolve(id, &Policy::default()).unwrap()).passed());
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

// ---- #332 review, item 6: each rule check is shown to fail on a bad stream ----

mod bad_streams {
    use aibus_extension::conformance::invariants;
    use aibus_extension::package;
    use aibus_trace::{EventKind, Origin, TraceEvent, Visibility};

    fn good() -> Vec<TraceEvent> {
        let pkg = package::read(&super::template_dir()).unwrap();
        aibus_extension::conformance::observe(&pkg, "fixtures/basic.jsonl", "exit:0")
            .unwrap()
            .events
    }

    fn failing(events: &[TraceEvent]) -> Vec<String> {
        invariants("x", events)
            .into_iter()
            .filter(|c| !c.passed)
            .map(|c| c.name)
            .collect()
    }

    #[test]
    fn a_good_stream_fails_nothing() {
        assert!(failing(&good()).is_empty());
    }

    #[test]
    fn ids_that_go_backwards_or_are_not_v7_fail_the_ascending_check() {
        let mut e = good();
        e.swap(1, 2);
        assert!(failing(&e).iter().any(|n| n.contains("event ids ascend")));
        let mut e = good();
        e[1].event_id = uuid::Uuid::from_u128(5);
        assert!(failing(&e).iter().any(|n| n.contains("event ids ascend")));
    }

    #[test]
    fn a_derived_event_passed_off_as_observed_or_a_cli_event_with_no_raw_line_fails_the_evidence_check(
    ) {
        let mut e = good();
        e[1].origin = Origin::Derived;
        e[1].visibility = Visibility::Observed;
        assert!(failing(&e).iter().any(|n| n.contains("evidence level")));
        let mut e = good();
        let i = e
            .iter()
            .position(|x| x.kind == EventKind::MessageChunk)
            .unwrap();
        e[i].evidence_refs.clear();
        assert!(failing(&e).iter().any(|n| n.contains("evidence level")));
    }

    #[test]
    fn a_metric_with_no_quality_fails_the_quality_check() {
        let mut e = good();
        let i = e
            .iter()
            .position(|x| x.kind == EventKind::UsageReported)
            .unwrap();
        e[i].data["metrics"][0]
            .as_object_mut()
            .unwrap()
            .remove("quality");
        assert!(failing(&e)
            .iter()
            .any(|n| n.contains("every metric has a data quality")));
    }

    #[test]
    fn a_missing_number_called_reported_or_a_number_called_unknown_fails_the_unknown_check() {
        let mut e = good();
        let i = e
            .iter()
            .position(|x| x.kind == EventKind::UsageReported)
            .unwrap();
        e[i].data["metrics"][0]["value"] = serde_json::Value::Null;
        e[i].data["metrics"][0]["quality"] = "reported".into();
        assert!(failing(&e)
            .iter()
            .any(|n| n.contains("usage that is missing stays unknown")));
        let mut e = good();
        e[i].data["metrics"][0]["quality"] = "unknown".into();
        assert!(failing(&e)
            .iter()
            .any(|n| n.contains("usage that is missing stays unknown")));
    }

    #[test]
    fn two_endings_none_or_one_that_is_not_last_fail_the_ending_check() {
        let mut e = good();
        let last = e.last().unwrap().clone();
        e.push(last);
        assert!(failing(&e).iter().any(|n| n.contains("ends once, last")));
        let mut e = good();
        e.pop();
        assert!(failing(&e).iter().any(|n| n.contains("ends once, last")));
        let mut e = good();
        let n = e.len();
        e.swap(n - 1, n - 2);
        assert!(failing(&e).iter().any(|n| n.contains("ends once, last")));
    }
}

/// A tool that starts and never reports back ends the attempt with a gap that asks for reconciliation.
#[test]
fn a_tool_that_never_reports_back_is_a_tool_outcome_unknown_gap() {
    use aibus_trace::EventKind;
    let pkg = package::read(&template_dir()).unwrap();
    let o = conformance::observe(&pkg, "fixtures/unfinished-tool.jsonl", "exit:0").unwrap();
    let gap = o
        .events
        .iter()
        .find(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "tool_outcome_unknown")
        .expect("the gap");
    assert_eq!(gap.native.tool_use_id.as_deref(), Some("t5"));
    assert_eq!(gap.data["requires_reconciliation"], true);
    let end = o.events.last().unwrap();
    assert_eq!(end.data["requires_reconciliation"], true);
}

// ---- item 7: what the declarative engine labels ----

mod labels {
    use std::time::SystemTime;

    use aibus_extension::declarative::Declarative;
    use aibus_extension::manifest::{Manifest, UsageQuality};
    use aibus_supervisor::Engine;
    use aibus_trace::{AttemptRef, DataQuality, EventKind};
    use uuid::Uuid;

    fn engine(q: UsageQuality) -> Declarative {
        let m = Manifest::parse(&super::common::template_manifest()).unwrap();
        let a = AttemptRef {
            workflow_run_id: Uuid::from_u128(1),
            attempt_id: Uuid::from_u128(2),
            parent_attempt_id: None,
            provider_extension: m.id.clone(),
        };
        Declarative::new(a, m.normalizer.rule, q)
    }

    #[test]
    fn evidence_refs_count_stdout_and_stderr_lines_separately() {
        let mut e = engine(UsageQuality::Reported);
        let at = SystemTime::UNIX_EPOCH;
        e.stream_line(br#"{"type":"text"}"#, at);
        e.stderr_line(b"a warning", at);
        let b = e.stream_line(br#"{"type":"text"}"#, at);
        assert_eq!(b.events[0].evidence_refs, ["raw:stdout:L2"]);
    }

    #[test]
    fn the_manifests_usage_quality_labels_the_metrics() {
        for (q, want) in [
            (UsageQuality::Reported, DataQuality::Reported),
            (UsageQuality::Estimated, DataQuality::Estimated),
        ] {
            let mut e = engine(q);
            let b = e.stream_line(
                br#"{"type":"usage","usage":{"in":5,"out":2}}"#,
                SystemTime::UNIX_EPOCH,
            );
            assert_eq!(b.events[0].kind, EventKind::UsageReported);
            let m: Vec<aibus_trace::Metric> =
                serde_json::from_value(b.events[0].data["metrics"].clone()).unwrap();
            assert!(m.iter().all(|m| m.quality == want), "{q:?} {m:?}");
        }
        // A manifest that says unknown never claims a number.
        let mut e = engine(UsageQuality::Unknown);
        let b = e.stream_line(
            br#"{"type":"usage","usage":{"in":5,"out":2}}"#,
            SystemTime::UNIX_EPOCH,
        );
        let m: Vec<aibus_trace::Metric> =
            serde_json::from_value(b.events[0].data["metrics"].clone()).unwrap();
        assert!(
            m.iter()
                .all(|m| m.value.is_none() && m.quality == DataQuality::Unknown),
            "{m:?}"
        );
    }

    #[test]
    fn a_negative_fractional_or_huge_token_count_is_unknown_not_reported() {
        for bad in ["-3", "1.5", "1e30", "\"7\"", "null"] {
            let mut e = engine(UsageQuality::Reported);
            let line = format!(r#"{{"type":"usage","usage":{{"in":{bad},"out":2}}}}"#);
            let b = e.stream_line(line.as_bytes(), SystemTime::UNIX_EPOCH);
            let m: Vec<aibus_trace::Metric> =
                serde_json::from_value(b.events[0].data["metrics"].clone()).unwrap();
            assert_eq!(
                (m[0].value, m[0].quality),
                (None, DataQuality::Unknown),
                "{bad}"
            );
            assert_eq!(m[1].quality, DataQuality::Reported, "{bad}");
        }
    }
}
