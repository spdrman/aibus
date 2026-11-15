//! The installed store and its lifecycle (#286): install, verify, list, update, pin, disable,
//! remove and rollback; quarantine of incompatible, revoked and provenance-invalid packages;
//! version downgrade and capability escalation on update.
#![cfg(unix)]

mod common;

use std::fs;

use aibus_extension::policy::Policy;
use aibus_extension::registry::{Registry, INDEX_FILE};
use aibus_extension::store::{Store, StoreError};
use aibus_extension::version::Version;
use common::{edit, package_with, template_as, template_manifest};

const ID: &str = "org.example.aibus.examplecli";

struct World {
    t: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self {
            t: tempfile::tempdir().unwrap(),
        }
    }
    fn reg(&self) -> std::path::PathBuf {
        self.t.path().join("reg")
    }
    fn store(&self) -> Store {
        Store::open(&self.t.path().join("store"))
    }
    fn publish(&self, name: &str, manifest: &str) {
        let p = package_with(&self.t.path().join("src"), name, manifest);
        Registry::publish(&self.reg(), &p).unwrap();
    }
    fn at(&self, version: &str) {
        self.publish(&format!("v{version}"), &template_as(ID, version));
    }
    fn open(&self) -> Registry {
        Registry::open(&self.reg()).unwrap()
    }
}

fn quarantined(e: &StoreError) -> &str {
    match e {
        StoreError::Quarantined { class, .. } => class,
        other => panic!("expected quarantine, got {other:?}"),
    }
}

#[test]
fn install_activates_the_newest_and_resolve_returns_it() {
    let w = World::new();
    w.at("0.1.0");
    w.at("0.2.0");
    let s = w.store();
    let rec = s
        .install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    assert_eq!(rec.active.as_deref(), Some("0.2.0"));
    assert_eq!(
        s.resolve(ID, &Policy::default()).unwrap().manifest.version,
        "0.2.0"
    );
    assert!(s
        .root
        .join("packages")
        .join(ID)
        .join("0.2.0/aibus-extension.toml")
        .is_file());
}

#[test]
fn install_of_a_chosen_version_installs_that_one() {
    let w = World::new();
    w.at("0.1.0");
    w.at("0.2.0");
    let rec = w
        .store()
        .install(
            &w.open(),
            &Policy::default(),
            ID,
            Some(&Version(0, 1, 0)),
            &[],
        )
        .unwrap();
    assert_eq!(rec.active.as_deref(), Some("0.1.0"));
}

#[test]
fn an_id_the_registry_does_not_have_is_an_error_and_a_path_shaped_id_is_refused() {
    let w = World::new();
    w.at("0.1.0");
    assert!(matches!(
        w.store().install(
            &w.open(),
            &Policy::default(),
            "org.example.nothing.here",
            None,
            &[]
        ),
        Err(StoreError::NotInRegistry(_))
    ));
    for bad in ["../../etc", "a/b", "", "/abs"] {
        assert!(
            w.store()
                .install(&w.open(), &Policy::default(), bad, None, &[])
                .is_err(),
            "{bad:?}"
        );
        assert!(
            w.store().resolve(bad, &Policy::default()).is_err(),
            "{bad:?}"
        );
        assert!(w.store().remove(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_store_directory_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    assert_eq!(
        fs::metadata(&s.root).unwrap().permissions().mode() & 0o077,
        0
    );
    assert_eq!(
        fs::metadata(s.root.join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
}

#[test]
fn a_revoked_package_is_quarantined_and_never_installed() {
    let w = World::new();
    w.at("0.1.0");
    let mut v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(w.reg().join(INDEX_FILE)).unwrap()).unwrap();
    v["revoked"] = serde_json::json!([{"id": ID, "reason": "withdrawn"}]);
    fs::write(w.reg().join(INDEX_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
    let s = w.store();
    let err = s
        .install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap_err();
    assert_eq!(quarantined(&err), "revoked");
    let rec = &s.state().unwrap().extensions[ID];
    assert!(rec.active.is_none() && rec.versions.is_empty());
    assert_eq!(rec.quarantined.len(), 1);
    assert!(!s.root.join("packages").join(ID).exists());
    assert!(s.resolve(ID, &Policy::default()).is_err());
}

#[test]
fn a_hash_mismatch_is_quarantined_as_provenance_invalid() {
    let w = World::new();
    w.at("0.1.0");
    let r = w.open();
    fs::write(
        w.reg().join(&r.entries[0].path).join("README.md"),
        b"tampered",
    )
    .unwrap();
    let err = w
        .store()
        .install(&r, &Policy::default(), ID, None, &[])
        .unwrap_err();
    assert_eq!(quarantined(&err), "provenance_invalid");
}

#[test]
fn an_incompatible_package_is_quarantined_as_incompatible() {
    let w = World::new();
    w.publish(
        "x",
        &edit(
            &template_as(ID, "0.1.0"),
            "protocol = \">=1.0.0, <2.0.0\"",
            "protocol = \">=9.0.0\"",
        ),
    );
    let err = w
        .store()
        .install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap_err();
    assert_eq!(quarantined(&err), "incompatible");
}

#[test]
fn a_quarantined_version_is_not_retried_by_an_install_that_would_pick_it_again() {
    let w = World::new();
    w.at("0.1.0");
    let r = w.open();
    fs::write(
        w.reg().join(&r.entries[0].path).join("README.md"),
        b"tampered",
    )
    .unwrap();
    let s = w.store();
    s.install(&r, &Policy::default(), ID, None, &[])
        .unwrap_err();
    let again = s
        .install(&r, &Policy::default(), ID, None, &[])
        .unwrap_err();
    assert_eq!(quarantined(&again), "provenance_invalid");
    assert_eq!(
        s.state().unwrap().extensions[ID].quarantined.len(),
        1,
        "the same finding is recorded once"
    );
}

#[test]
fn verify_passes_for_an_untouched_install_and_quarantines_a_tampered_one() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.verify(ID, &[], &Policy::default()).unwrap();
    fs::write(
        s.root
            .join("packages")
            .join(ID)
            .join("0.1.0/fixtures/basic.jsonl"),
        b"{}\n",
    )
    .unwrap();
    let err = s.verify(ID, &[], &Policy::default()).unwrap_err();
    assert_eq!(quarantined(&err), "tampered");
    assert!(
        s.resolve(ID, &Policy::default()).is_err(),
        "a quarantined active version does not resolve"
    );
}

#[test]
fn resolve_rehashes_on_every_call_so_a_swap_after_install_never_runs() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.resolve(ID, &Policy::default()).unwrap();
    fs::write(
        s.root.join("packages").join(ID).join("0.1.0/LICENSE"),
        b"changed",
    )
    .unwrap();
    assert!(s.resolve(ID, &Policy::default()).is_err());
}

#[test]
fn verify_catches_a_revocation_published_after_the_install() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let revoked = [aibus_extension::registry::Revocation {
        id: ID.into(),
        version: Some("0.1.0".into()),
        hash: None,
        reason: "cve".into(),
    }];
    let err = s.verify(ID, &revoked, &Policy::default()).unwrap_err();
    assert_eq!(quarantined(&err), "revoked");
}

#[test]
fn update_moves_to_the_newest_and_keeps_the_old_for_rollback() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    w.at("0.3.0");
    let rec = s.update(&w.open(), &Policy::default(), ID, &[]).unwrap();
    assert_eq!(
        (rec.active.as_deref(), rec.previous.as_deref()),
        (Some("0.3.0"), Some("0.1.0"))
    );
    let back = s.rollback(ID, &Policy::default()).unwrap();
    assert_eq!(back.active.as_deref(), Some("0.1.0"));
    assert_eq!(
        s.resolve(ID, &Policy::default()).unwrap().manifest.version,
        "0.1.0"
    );
}

#[test]
fn update_with_nothing_newer_says_so_and_never_downgrades() {
    let w = World::new();
    w.at("0.1.0");
    w.at("0.2.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    assert!(matches!(
        s.update(&w.open(), &Policy::default(), ID, &[]),
        Err(StoreError::UpToDate(_))
    ));
    // An explicit install of an older version over the active one is a downgrade, and refused.
    let err = s
        .install(
            &w.open(),
            &Policy::default(),
            ID,
            Some(&Version(0, 1, 0)),
            &[],
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Downgrade { .. }), "{err:?}");
    assert_eq!(
        s.state().unwrap().extensions[ID].active.as_deref(),
        Some("0.2.0")
    );
}

/// A registry that is made to offer an older version as the newest (a rolled-back index) does not
/// move an installed extension backwards.
#[test]
fn a_registry_replaying_an_older_index_does_not_downgrade_an_update() {
    let w = World::new();
    w.at("0.2.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let old = World::new();
    old.at("0.1.0");
    assert!(matches!(
        s.update(&old.open(), &Policy::default(), ID, &[]),
        Err(StoreError::UpToDate(_))
    ));
    assert_eq!(
        s.resolve(ID, &Policy::default()).unwrap().manifest.version,
        "0.2.0"
    );
}

#[test]
fn a_pinned_extension_is_not_updated() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.set_pinned(ID, true).unwrap();
    w.at("0.2.0");
    assert!(matches!(
        s.update(&w.open(), &Policy::default(), ID, &[]),
        Err(StoreError::Pinned(_))
    ));
    s.set_pinned(ID, false).unwrap();
    assert!(s.update(&w.open(), &Policy::default(), ID, &[]).is_ok());
}

/// Capability escalation: the new version asks for another executable, so nothing changes until
/// the user accepts that item by name.
#[test]
fn an_update_that_asks_for_more_is_refused_until_the_items_are_accepted() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let more = edit(
        &template_as(ID, "0.2.0"),
        "executable_candidates = [\"examplecli\"]",
        "executable_candidates = [\"examplecli\", \"othercli\"]",
    );
    let more = edit(&more, "env_allow = []", "env_allow = [\"EXAMPLE_HOME\"]");
    w.publish("more", &more);
    let err = s
        .update(&w.open(), &Policy::default(), ID, &[])
        .unwrap_err();
    match &err {
        StoreError::Escalation(items) => {
            assert!(
                items.contains(&"executable:othercli".to_owned())
                    && items.contains(&"env:EXAMPLE_HOME".to_owned()),
                "{items:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        s.state().unwrap().extensions[ID].active.as_deref(),
        Some("0.1.0"),
        "nothing changed"
    );
    // Accepting one of two is not enough.
    assert!(matches!(
        s.update(
            &w.open(),
            &Policy::default(),
            ID,
            &["executable:othercli".to_owned()]
        ),
        Err(StoreError::Escalation(_))
    ));
    let rec = s
        .update(
            &w.open(),
            &Policy::default(),
            ID,
            &[
                "executable:othercli".to_owned(),
                "env:EXAMPLE_HOME".to_owned(),
            ],
        )
        .unwrap();
    assert_eq!(rec.active.as_deref(), Some("0.2.0"));
}

#[test]
fn disable_stops_resolve_and_enable_restores_it() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.set_disabled(ID, true).unwrap();
    assert!(matches!(
        s.resolve(ID, &Policy::default()),
        Err(StoreError::Disabled(_))
    ));
    s.set_disabled(ID, false).unwrap();
    assert!(s.resolve(ID, &Policy::default()).is_ok());
}

#[test]
fn rollback_without_an_earlier_version_is_an_error() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    assert!(matches!(
        s.rollback(ID, &Policy::default()),
        Err(StoreError::NoPrevious(_))
    ));
}

#[test]
fn remove_deletes_the_record_and_every_installed_version_and_nothing_else() {
    let w = World::new();
    w.at("0.1.0");
    let other =
        template_manifest().replace("org.example.aibus.examplecli", "org.example.aibus.other");
    w.publish("other", &other);
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.install(
        &w.open(),
        &Policy::default(),
        "org.example.aibus.other",
        None,
        &[],
    )
    .unwrap();
    s.remove(ID).unwrap();
    assert!(!s.root.join("packages").join(ID).exists());
    assert!(s
        .root
        .join("packages/org.example.aibus.other/0.1.0")
        .is_dir());
    assert!(!s.state().unwrap().extensions.contains_key(ID));
    assert!(matches!(s.remove(ID), Err(StoreError::NotInstalled(_))));
}

#[test]
fn remove_never_follows_a_link_planted_in_the_packages_directory() {
    use std::os::unix::fs::symlink;
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let victim = w.t.path().join("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("precious"), b"keep").unwrap();
    let pkgdir = s.root.join("packages").join(ID);
    fs::remove_dir_all(&pkgdir).unwrap();
    symlink(&victim, &pkgdir).unwrap();
    let _ = s.remove(ID);
    assert!(
        victim.join("precious").is_file(),
        "a link in the store must never be followed"
    );
}

#[test]
fn a_corrupt_or_hostile_state_file_is_an_error_not_a_panic() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    for bad in [
        "not json",
        "{\"extensions\":{\"../x\":{}}}",
        "{\"extensions\":{},\"surprise\":1}",
        "",
    ] {
        fs::write(s.root.join("state.json"), bad).unwrap();
        assert!(s.state().is_err(), "{bad:?}");
        assert!(s.resolve(ID, &Policy::default()).is_err());
    }
}

#[test]
fn list_reports_every_installed_extension_with_its_flags() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.set_pinned(ID, true).unwrap();
    let st = s.state().unwrap();
    let rec = &st.extensions[ID];
    assert!(rec.pinned && !rec.disabled);
    assert_eq!(rec.versions.len(), 1);
}

// ---- #332 review, items 2 to 4 ----

fn with_env(version: &str, env: &str) -> String {
    edit(
        &template_as(ID, version),
        "env_allow = []",
        &format!("env_allow = [\"{env}\"]"),
    )
}

fn revoke(version: &str) -> Policy {
    Policy::parse(&format!(
        "[[revoke]]\nid = \"{ID}\"\nversion = \"{version}\"\nreason = \"withdrawn\"\n"
    ))
    .unwrap()
}

/// A registry that re-points an installed version at different content is not an update.
#[test]
fn the_same_version_with_new_content_is_refused_not_installed_over_the_active_one() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let swapped = World::new();
    swapped.publish(
        "evil",
        &edit(
            &with_env("0.1.0", "EXAMPLE_HOME"),
            "workspace_control = \"unsupported\"",
            "workspace_control = \"supported\"",
        ),
    );
    let err = s
        .install(&swapped.open(), &Policy::default(), ID, None, &[])
        .unwrap_err();
    assert_eq!(quarantined(&err), "provenance_invalid", "{err:?}");
    let rec = &s.state().unwrap().extensions[ID];
    assert_eq!(rec.active.as_deref(), Some("0.1.0"));
    assert!(!s
        .resolve(ID, &Policy::default())
        .unwrap()
        .manifest
        .execution
        .env_allow
        .contains(&"EXAMPLE_HOME".to_owned()));
}

#[test]
fn an_unreadable_installed_copy_fails_the_install_it_does_not_skip_the_check() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    fs::remove_file(
        s.root
            .join("packages")
            .join(ID)
            .join("0.1.0/aibus-extension.toml"),
    )
    .unwrap();
    w.publish("more", &with_env("0.2.0", "EXAMPLE_HOME"));
    let err = s
        .update(&w.open(), &Policy::default(), ID, &[])
        .unwrap_err();
    assert_eq!(quarantined(&err), "tampered", "{err:?}");
    assert!(s.state().unwrap().extensions[ID].active.is_none());
}

/// After the active version is quarantined there is still a last accepted grant to compare with.
#[test]
fn a_quarantined_active_version_leaves_the_last_accepted_grant_as_the_baseline() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let revoked = [aibus_extension::registry::Revocation {
        id: ID.into(),
        version: Some("0.1.0".into()),
        hash: None,
        reason: "cve".into(),
    }];
    s.verify(ID, &revoked, &Policy::default()).unwrap_err();
    assert!(s.state().unwrap().extensions[ID].active.is_none());
    w.publish("more", &with_env("0.2.0", "EXAMPLE_HOME"));
    let err = s
        .install(
            &w.open(),
            &Policy::default(),
            ID,
            Some(&Version(0, 2, 0)),
            &[],
        )
        .unwrap_err();
    match err {
        StoreError::Escalation(items) => {
            assert!(items.contains(&"env:EXAMPLE_HOME".to_owned()), "{items:?}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn install_honours_a_pin_by_id_and_by_version() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    s.set_pinned(ID, true).unwrap();
    w.at("0.2.0");
    assert!(matches!(
        s.install(&w.open(), &Policy::default(), ID, None, &[]),
        Err(StoreError::Pinned(_))
    ));
    assert!(matches!(
        s.install(
            &w.open(),
            &Policy::default(),
            ID,
            Some(&Version(0, 2, 0)),
            &[]
        ),
        Err(StoreError::Pinned(_))
    ));
    assert_eq!(
        s.state().unwrap().extensions[ID].active.as_deref(),
        Some("0.1.0")
    );
    // The version already active is not a move.
    assert!(s
        .install(
            &w.open(),
            &Policy::default(),
            ID,
            Some(&Version(0, 1, 0)),
            &[]
        )
        .is_ok());
}

#[test]
fn rollback_refuses_a_version_the_policy_revokes() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    w.at("0.2.0");
    s.update(&w.open(), &Policy::default(), ID, &[]).unwrap();
    let err = s.rollback(ID, &revoke("0.1.0")).unwrap_err();
    assert_eq!(quarantined(&err), "revoked", "{err:?}");
    assert_eq!(
        s.state().unwrap().extensions[ID].active.as_deref(),
        Some("0.2.0")
    );
}

#[test]
fn resolve_does_not_serve_a_revoked_version_and_remembers_what_the_registry_revoked() {
    let w = World::new();
    w.at("0.1.0");
    let s = w.store();
    s.install(&w.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    // The user's own policy.
    let err = s.resolve(ID, &revoke("0.1.0")).unwrap_err();
    assert_eq!(quarantined(&err), "revoked", "{err:?}");

    // The registry's list is kept from the last time the store saw it, so `resolve` needs no registry.
    let w2 = World::new();
    w2.at("0.1.0");
    let s2 = w2.store();
    s2.install(&w2.open(), &Policy::default(), ID, None, &[])
        .unwrap();
    let mut v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(w2.reg().join(INDEX_FILE)).unwrap()).unwrap();
    v["revoked"] = serde_json::json!([{"id": ID, "version": "0.1.0", "reason": "late news"}]);
    fs::write(w2.reg().join(INDEX_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
    // Any command that reads the registry notes it, even one that goes on to say "up to date".
    assert!(matches!(
        s2.update(&w2.open(), &Policy::default(), ID, &[]),
        Err(StoreError::UpToDate(_))
    ));
    let err = s2.resolve(ID, &Policy::default()).unwrap_err();
    assert_eq!(quarantined(&err), "revoked", "{err:?}");
}
