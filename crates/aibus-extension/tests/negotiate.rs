//! Compatibility negotiation and capability probes (#286).

mod common;

use std::collections::BTreeSet;

use aibus_extension::manifest::{Capability, Manifest};
use aibus_extension::negotiate::{check_host, negotiate, Incompatible, Verdict, PROTOCOL};
use aibus_supervisor::cli::Probe;
use aibus_supervisor::cli::Version;
use common::{edit, reference_dir, template_manifest};

fn cli(version: Version, flags: &[&str]) -> Probe {
    Probe {
        version,
        tested: false,
        flags: flags
            .iter()
            .map(|f| (*f).to_owned())
            .collect::<BTreeSet<_>>(),
    }
}

fn template() -> Manifest {
    Manifest::parse(&template_manifest()).unwrap()
}

#[test]
fn the_host_speaks_protocol_1_0_0() {
    assert_eq!(PROTOCOL, Version(1, 0, 0));
}

#[test]
fn a_protocol_range_that_excludes_the_host_is_incompatible() {
    let m = Manifest::parse(&edit(
        &template_manifest(),
        "protocol = \">=1.0.0, <2.0.0\"",
        "protocol = \">=2.0.0, <3.0.0\"",
    ))
    .unwrap();
    assert!(matches!(
        check_host(&m, "linux"),
        Err(Incompatible::Protocol { .. })
    ));
}

#[test]
fn an_os_the_extension_does_not_list_is_incompatible() {
    assert!(check_host(&template(), "linux").is_ok());
    assert!(matches!(
        check_host(&template(), "windows"),
        Err(Incompatible::Os { .. })
    ));
}

#[test]
fn a_cli_outside_the_written_for_range_is_incompatible() {
    let err = negotiate(&template(), "linux", &cli(Version(0, 9, 0), &["--usage"])).unwrap_err();
    assert!(matches!(err, Incompatible::CliVersion { .. }), "{err:?}");
}

#[test]
fn a_probe_required_capability_follows_the_help_text() {
    let with = negotiate(&template(), "linux", &cli(Version(1, 2, 0), &["--usage"])).unwrap();
    assert!(with.supports(Capability::UsageReport));
    let without = negotiate(&template(), "linux", &cli(Version(1, 2, 0), &[])).unwrap();
    match &without.capabilities[&Capability::UsageReport] {
        Verdict::Unsupported(why) => assert!(why.contains("--usage"), "{why}"),
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_probe_with_a_minimum_version_needs_that_version() {
    let m = edit(
        &template_manifest(),
        "flags = [\"--usage\"]",
        "flags = [\"--usage\"]\nmin_version = \"1.5.0\"",
    );
    let m = Manifest::parse(&m).unwrap();
    assert!(
        !negotiate(&m, "linux", &cli(Version(1, 4, 0), &["--usage"]))
            .unwrap()
            .supports(Capability::UsageReport)
    );
    assert!(negotiate(&m, "linux", &cli(Version(1, 5, 0), &["--usage"]))
        .unwrap()
        .supports(Capability::UsageReport));
}

/// The extension cannot gain a capability the CLI happens to have: `unsupported` stays so.
#[test]
fn a_capability_declared_unsupported_stays_unsupported_whatever_the_cli_has() {
    let n = negotiate(
        &template(),
        "linux",
        &cli(Version(1, 2, 0), &["--usage", "--resume", "--workspace"]),
    )
    .unwrap();
    for c in [
        Capability::SessionResume,
        Capability::WorkspaceControl,
        Capability::InteractiveApproval,
    ] {
        assert!(!n.supports(c), "{c:?}");
    }
}

#[test]
fn every_capability_gets_a_verdict() {
    let n = negotiate(&template(), "linux", &cli(Version(1, 2, 0), &[])).unwrap();
    assert_eq!(n.capabilities.len(), 13);
}

/// Amendment E4: the extension wants hook payloads and the host has no live way to take them.
#[test]
fn hook_delivery_the_host_cannot_provide_is_unsupported_with_the_reason() {
    let text = std::fs::read_to_string(reference_dir().join("aibus-extension.toml")).unwrap();
    let m = Manifest::parse(&text).unwrap();
    let n = negotiate(
        &m,
        "macos",
        &cli(Version(2, 1, 311), &["--include-hook-events"]),
    )
    .unwrap();
    match &n.capabilities[&Capability::HookChannel] {
        Verdict::Unsupported(why) => assert!(why.contains("hook"), "{why}"),
        v => panic!("{v:?}"),
    }
}

/// E4: the host cannot deliver hook payloads, so a manifest that declares `hook_channel` as
/// supported, with or without `needs_delivery`, is still told no.
#[test]
fn hook_channel_declared_supported_is_not_offered_while_the_host_cannot_deliver_hooks() {
    let m = Manifest::parse(&edit(
        &template_manifest(),
        "hook_channel = \"unsupported\"",
        "hook_channel = \"supported\"",
    ))
    .unwrap();
    let n = negotiate(&m, "linux", &cli(Version(1, 2, 0), &["--usage"])).unwrap();
    assert!(!n.supports(Capability::HookChannel));
}
