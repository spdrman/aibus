//! The registry format and the checks every fetch makes (#286): a manifest plus a content hash,
//! duplicates, traversal, hash mismatch, version downgrade, revocation, signatures and policy.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;

use aibus_extension::policy::Policy;
use aibus_extension::registry::{
    self, signed_message, Entry, Refusal, Registry, RegistryError, Revocation, Signature,
    INDEX_FILE,
};
use aibus_extension::version::Version;
use common::{edit, package_with, registry_of, template_as, template_manifest};
use ring::signature::{Ed25519KeyPair, KeyPair};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn index(reg: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(reg.join(INDEX_FILE)).unwrap()).unwrap()
}

fn write_index(reg: &std::path::Path, v: &serde_json::Value) {
    fs::write(reg.join(INDEX_FILE), serde_json::to_vec_pretty(v).unwrap()).unwrap();
}

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
    fn pkg(&self, name: &str, version: &str) -> std::path::PathBuf {
        package_with(
            &self.t.path().join("src"),
            name,
            &template_as("org.example.aibus.examplecli", version),
        )
    }
}

#[test]
fn publish_then_open_lists_the_entry_with_its_content_hash() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    assert_eq!(r.entries.len(), 1);
    let e = &r.entries[0];
    assert_eq!(
        (e.id.as_str(), e.version.as_str()),
        ("org.example.aibus.examplecli", "0.1.0")
    );
    assert!(e.hash.starts_with("sha256-tree-v1:"));
    let pkg = r.fetch(e, &Policy::default()).unwrap();
    assert_eq!(pkg.hash, e.hash);
}

#[test]
fn publishing_the_same_id_and_version_twice_is_refused() {
    let w = World::new();
    let p = w.pkg("a", "0.1.0");
    Registry::publish(&w.reg(), &p).unwrap();
    assert!(matches!(
        Registry::publish(&w.reg(), &p),
        Err(RegistryError::Invalid(_))
    ));
}

#[test]
fn search_matches_id_and_name_case_insensitively_and_lists_the_newest_first() {
    let w = World::new();
    let r = registry_of(
        &w.reg(),
        &[
            w.pkg("a", "0.1.0"),
            w.pkg("b", "0.2.0"),
            w.pkg("c", "0.10.0"),
        ],
    );
    let versions: Vec<&str> = r
        .search("EXAMPLECLI")
        .iter()
        .map(|e| e.version.as_str())
        .collect();
    assert_eq!(versions, ["0.10.0", "0.2.0", "0.1.0"]);
    assert!(r.search("nothing-like-this").is_empty());
}

#[test]
fn find_without_a_version_is_the_newest_by_number_not_by_text() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.9.0"), w.pkg("b", "0.10.0")]);
    assert_eq!(
        r.find("org.example.aibus.examplecli", None)
            .unwrap()
            .version,
        "0.10.0"
    );
    assert_eq!(
        r.find("org.example.aibus.examplecli", Some(&Version(0, 9, 0)))
            .unwrap()
            .version,
        "0.9.0"
    );
    assert!(r
        .find("org.example.aibus.examplecli", Some(&Version(7, 0, 0)))
        .is_none());
}

#[test]
fn a_duplicate_id_and_version_in_the_index_is_refused() {
    let w = World::new();
    registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let mut v = index(&w.reg());
    let e = v["entries"][0].clone();
    v["entries"].as_array_mut().unwrap().push(e);
    write_index(&w.reg(), &v);
    assert!(
        matches!(Registry::open(&w.reg()), Err(RegistryError::Invalid(m)) if m.contains("duplicate"))
    );
}

#[test]
fn an_entry_path_that_climbs_or_is_absolute_is_refused_at_open() {
    for bad in ["../x", "/etc", "a/../../x", "a//b", "", "."] {
        let w = World::new();
        registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
        let mut v = index(&w.reg());
        v["entries"][0]["path"] = bad.into();
        write_index(&w.reg(), &v);
        assert!(
            matches!(Registry::open(&w.reg()), Err(RegistryError::Invalid(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn a_malformed_id_version_or_hash_in_the_index_is_refused() {
    for (field, bad) in [("id", "../x"), ("version", "1.0"), ("hash", "md5:abc")] {
        let w = World::new();
        registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
        let mut v = index(&w.reg());
        v["entries"][0][field] = bad.into();
        write_index(&w.reg(), &v);
        assert!(
            matches!(Registry::open(&w.reg()), Err(RegistryError::Invalid(_))),
            "{field}"
        );
    }
}

#[test]
fn an_oversized_index_too_many_entries_an_unknown_field_or_a_future_version_is_refused() {
    let w = World::new();
    registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let big = w.reg().join(INDEX_FILE);
    let good = fs::read(&big).unwrap();
    fs::write(&big, vec![b' '; registry::MAX_INDEX_BYTES + 1]).unwrap();
    assert!(matches!(
        Registry::open(&w.reg()),
        Err(RegistryError::TooLarge(_))
    ));
    fs::write(&big, &good).unwrap();

    let mut v = index(&w.reg());
    v["surprise"] = true.into();
    write_index(&w.reg(), &v);
    assert!(matches!(
        Registry::open(&w.reg()),
        Err(RegistryError::Invalid(_))
    ));

    let mut v = index(&w.reg());
    v["index_version"] = 2.into();
    write_index(&w.reg(), &v);
    assert!(matches!(
        Registry::open(&w.reg()),
        Err(RegistryError::Invalid(_))
    ));

    let mut v = index(&w.reg());
    let e = v["entries"][0].clone();
    let many: Vec<_> = (0..registry::MAX_ENTRIES + 1)
        .map(|i| {
            let mut e = e.clone();
            e["version"] = format!("1.0.{i}").into();
            e
        })
        .collect();
    v["entries"] = many.into();
    write_index(&w.reg(), &v);
    // Either cap may fire first: 4097 entries are also over the size cap.
    assert!(matches!(
        Registry::open(&w.reg()),
        Err(RegistryError::Invalid(_) | RegistryError::TooLarge(_))
    ));
}

#[test]
fn a_symlinked_index_or_registry_directory_is_refused() {
    let w = World::new();
    registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let alias = w.t.path().join("alias");
    symlink(w.reg(), &alias).unwrap();
    assert!(Registry::open(&alias).is_err());
    let real = w.t.path().join("real.json");
    fs::rename(w.reg().join(INDEX_FILE), &real).unwrap();
    symlink(&real, w.reg().join(INDEX_FILE)).unwrap();
    assert!(Registry::open(&w.reg()).is_err());
}

#[test]
fn an_entry_whose_package_directory_is_a_link_to_elsewhere_is_refused_on_fetch() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let e = r.entries[0].clone();
    let elsewhere = w.t.path().join("elsewhere");
    fs::rename(w.reg().join(&e.path), &elsewhere).unwrap();
    symlink(&elsewhere, w.reg().join(&e.path)).unwrap();
    assert!(matches!(
        r.fetch(&e, &Policy::default()),
        Err(Refusal::Package(_))
    ));
}

/// Provenance: the package on disk is not the one the index hashed.
#[test]
fn a_package_changed_after_it_was_indexed_is_a_hash_mismatch() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let e = &r.entries[0];
    fs::write(w.reg().join(&e.path).join("fixtures/basic.jsonl"), b"{}\n").unwrap();
    let err = r.fetch(e, &Policy::default()).unwrap_err();
    assert!(matches!(err, Refusal::HashMismatch { .. }), "{err:?}");
    assert_eq!(err.class(), "provenance_invalid");
}

#[test]
fn an_entry_whose_manifest_names_another_id_or_version_is_refused() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let mut v = index(&w.reg());
    v["entries"][0]["id"] = "org.example.aibus.impostor".into();
    write_index(&w.reg(), &v);
    let r2 = Registry::open(&w.reg()).unwrap();
    let err = r2.fetch(&r2.entries[0], &Policy::default()).unwrap_err();
    assert!(matches!(err, Refusal::IdentityMismatch { .. }), "{err:?}");
    assert_eq!(err.class(), "provenance_invalid");
    drop(r);
}

#[test]
fn a_revoked_version_hash_or_whole_id_is_refused_and_is_class_revoked() {
    for rev in [
        serde_json::json!({"id":"org.example.aibus.examplecli","version":"0.1.0","reason":"bad release"}),
        serde_json::json!({"id":"org.example.aibus.examplecli","reason":"publisher withdrawn"}),
    ] {
        let w = World::new();
        registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
        let mut v = index(&w.reg());
        v["revoked"] = vec![rev].into();
        write_index(&w.reg(), &v);
        let r = Registry::open(&w.reg()).unwrap();
        let err = r.fetch(&r.entries[0], &Policy::default()).unwrap_err();
        assert_eq!(err.class(), "revoked", "{err:?}");
    }
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let by_hash = [Revocation {
        id: "org.example.aibus.examplecli".into(),
        version: None,
        hash: Some(r.entries[0].hash.clone()),
        reason: "this content".into(),
    }];
    let pkg = r.fetch(&r.entries[0], &Policy::default()).unwrap();
    let err = registry::check(&pkg, &r.entries[0], &by_hash, &Policy::default()).unwrap_err();
    assert_eq!(err.class(), "revoked");
}

#[test]
fn the_users_own_policy_can_revoke_too() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let policy = Policy::parse("[[revoke]]\nid = \"org.example.aibus.examplecli\"\nversion = \"0.1.0\"\nreason = \"I said so\"\n").unwrap();
    let err = r.fetch(&r.entries[0], &policy).unwrap_err();
    assert!(
        matches!(err, Refusal::Revoked(ref m) if m.contains("I said so")),
        "{err:?}"
    );
}

#[test]
fn an_incompatible_package_is_class_incompatible() {
    let w = World::new();
    let m = edit(
        &template_manifest(),
        "protocol = \">=1.0.0, <2.0.0\"",
        "protocol = \">=2.0.0, <3.0.0\"",
    );
    let p = package_with(&w.t.path().join("src"), "a", &m);
    let r = registry_of(&w.reg(), &[p]);
    let err = r.fetch(&r.entries[0], &Policy::default()).unwrap_err();
    assert_eq!(err.class(), "incompatible", "{err:?}");
}

fn signed(w: &World, tamper_key: bool) -> (Registry, Policy, Entry) {
    let r0 = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let seed = [7u8; 32];
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let e = &r0.entries[0];
    let sig = pair.sign(&signed_message(&e.id, &e.version, &e.hash));
    let mut v = index(&w.reg());
    v["entries"][0]["signature"] = serde_json::to_value(Signature {
        key_id: "k1".into(),
        signature: hex(sig.as_ref()),
    })
    .unwrap();
    write_index(&w.reg(), &v);
    let other = Ed25519KeyPair::from_seed_unchecked(&[9u8; 32]).unwrap();
    let public = if tamper_key {
        hex(other.public_key().as_ref())
    } else {
        hex(pair.public_key().as_ref())
    };
    let policy = Policy::parse(&format!(
        "require_signature = true\n[trusted_keys]\nk1 = \"{public}\"\n"
    ))
    .unwrap();
    let r = Registry::open(&w.reg()).unwrap();
    let e = r.entries[0].clone();
    (r, policy, e)
}

#[test]
fn a_good_signature_from_a_trusted_key_passes_when_one_is_required() {
    let w = World::new();
    let (r, policy, e) = signed(&w, false);
    assert!(r.fetch(&e, &policy).is_ok());
}

#[test]
fn a_signature_from_another_key_or_no_signature_fails_when_one_is_required() {
    let w = World::new();
    let (r, policy, e) = signed(&w, true);
    let err = r.fetch(&e, &policy).unwrap_err();
    assert!(matches!(err, Refusal::BadSignature(_)), "{err:?}");
    assert_eq!(err.class(), "provenance_invalid");

    let w2 = World::new();
    let r2 = registry_of(&w2.reg(), &[w2.pkg("a", "0.1.0")]);
    let err = r2
        .fetch(
            &r2.entries[0],
            &Policy::parse("require_signature = true").unwrap(),
        )
        .unwrap_err();
    assert!(matches!(err, Refusal::SignatureRequired), "{err:?}");
}

#[test]
fn a_signature_made_for_one_hash_does_not_cover_another_version() {
    let w = World::new();
    let (_r, policy, _e) = signed(&w, false);
    let mut v = index(&w.reg());
    v["entries"][0]["version"] = "0.1.1".into();
    write_index(&w.reg(), &v);
    let r = Registry::open(&w.reg()).unwrap();
    let err = r.fetch(&r.entries[0], &policy).unwrap_err();
    // The identity check or the signature check fires; either way it is provenance, not a pass.
    assert_eq!(err.class(), "provenance_invalid", "{err:?}");
}

#[test]
fn a_publisher_outside_the_policys_allowlist_is_refused() {
    let w = World::new();
    let r = registry_of(&w.reg(), &[w.pkg("a", "0.1.0")]);
    let err = r
        .fetch(
            &r.entries[0],
            &Policy::parse("allow_publishers = [\"Someone Else\"]").unwrap(),
        )
        .unwrap_err();
    assert!(matches!(err, Refusal::PublisherNotAllowed(_)), "{err:?}");
    assert!(r
        .fetch(
            &r.entries[0],
            &Policy::parse("allow_publishers = [\"Example Corp\"]").unwrap()
        )
        .is_ok());
}

#[test]
fn a_policy_with_an_unknown_key_or_a_bad_public_key_is_refused() {
    assert!(Policy::parse("surprise = 1").is_err());
    assert!(Policy::parse("[trusted_keys]\nk = \"nothex\"").is_err());
    assert!(Policy::parse("[trusted_keys]\nk = \"abcd\"").is_err());
}

// ---- #332 review, item 6: the entry cap on its own ----

/// 4097 compact entries are about 600 KB, under the 1 MiB size cap, so only the entry cap can refuse.
#[test]
fn the_entry_cap_refuses_an_index_that_is_well_under_the_size_cap() {
    let w = World::new();
    fs::create_dir_all(w.reg()).unwrap();
    let entries: Vec<serde_json::Value> = (0..registry::MAX_ENTRIES + 1)
        .map(|i| {
            serde_json::json!({
                "id": "org.a.b", "version": format!("1.0.{i}"),
                "hash": format!("sha256-tree-v1:{}", "0".repeat(64)), "path": "p"
            })
        })
        .collect();
    let text =
        serde_json::to_vec(&serde_json::json!({"index_version": 1, "entries": entries})).unwrap();
    assert!(
        text.len() < registry::MAX_INDEX_BYTES,
        "{} bytes",
        text.len()
    );
    fs::write(w.reg().join(INDEX_FILE), text).unwrap();
    assert!(
        matches!(Registry::open(&w.reg()), Err(RegistryError::Invalid(m)) if m.contains("4096"))
    );
    // One fewer is fine.
    let mut v = index(&w.reg());
    v["entries"].as_array_mut().unwrap().pop();
    write_index(&w.reg(), &v);
    assert!(Registry::open(&w.reg()).is_ok());
}
