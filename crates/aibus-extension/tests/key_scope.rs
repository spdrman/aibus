//! A trusted signing key is bound to the extension ids it may sign (#335): the policy file carries
//! the scope, verify, install, update and rollback refuse a signature outside it, nothing but the
//! user's own policy file can widen it, and a revoked key takes only its own ids with it.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::PathBuf;

use aibus_extension::policy::Policy;
use aibus_extension::registry::{signed_message, Refusal, Registry, INDEX_FILE};
use aibus_extension::store::{Store, StoreError};
use common::{package_with, template_as};
use ring::signature::{Ed25519KeyPair, KeyPair};

const A: &str = "org.devshell.aibus.alpha";
const B: &str = "org.devshell.aibus.beta";
const X: &str = "org.devshell.aibusx.gamma";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn pair(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn public(seed: u8) -> String {
    hex(pair(seed).public_key().as_ref())
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
    fn reg(&self) -> PathBuf {
        self.t.path().join("reg")
    }
    fn store(&self) -> Store {
        Store::open(&self.t.path().join("store"))
    }
    fn open(&self) -> Registry {
        Registry::open(&self.reg()).unwrap()
    }
    /// Publishes `id` at `version` and signs the entry with the key made from `seed` under `key_id`.
    fn signed(&self, id: &str, version: &str, key_id: &str, seed: u8) {
        let name = format!("{id}-{version}");
        let p = package_with(&self.t.path().join("src"), &name, &template_as(id, version));
        let e = Registry::publish(&self.reg(), &p).unwrap();
        let sig = pair(seed).sign(&signed_message(&e.id, &e.version, &e.hash));
        self.edit_entry(id, version, |entry| {
            entry["signature"] =
                serde_json::json!({"key_id": key_id, "signature": hex(sig.as_ref())});
        });
    }
    fn edit_entry(&self, id: &str, version: &str, f: impl FnOnce(&mut serde_json::Value)) {
        let path = self.reg().join(INDEX_FILE);
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let entry = v["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|e| e["id"] == id && e["version"] == version)
            .unwrap();
        f(entry);
        fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }
}

/// One `[trusted_keys.<name>]` table.
fn key(name: &str, seed: u8, scope: &str) -> String {
    format!(
        "[trusted_keys.{name}]\nkey = \"{}\"\n{scope}\n",
        public(seed)
    )
}

fn policy(text: &str) -> Policy {
    Policy::parse(text).unwrap_or_else(|e| panic!("policy refused: {e}\n{text}"))
}

fn fetch(w: &World, id: &str, version: &str, p: &Policy) -> Result<(), Refusal> {
    let r = w.open();
    let e = r
        .find(id, Some(&aibus_extension::version::exact(version).unwrap()))
        .unwrap()
        .clone();
    r.fetch(&e, p).map(|_| ())
}

fn class(e: &StoreError) -> &str {
    match e {
        StoreError::Quarantined { class, .. } => class,
        other => panic!("expected quarantine, got {other:?}"),
    }
}

// ---- the scope itself ----

#[test]
fn a_key_signing_an_id_outside_its_scope_is_refused_and_the_reason_names_both() {
    let w = World::new();
    w.signed(B, "0.1.0", "k1", 1);
    let p = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, &format!("ids = [\"{A}\"]"))
    ));
    let err = fetch(&w, B, "0.1.0", &p).unwrap_err();
    assert_eq!(err.class(), "provenance_invalid", "{err:?}");
    let text = err.to_string();
    assert!(text.contains("k1") && text.contains(B), "{text}");
}

#[test]
fn a_key_signing_an_id_inside_its_scope_passes_by_exact_id_and_by_prefix() {
    let w = World::new();
    w.signed(A, "0.1.0", "k1", 1);
    w.signed(B, "0.1.0", "k2", 2);
    let p = policy(&format!(
        "require_signature = true\n{}{}",
        key("k1", 1, &format!("ids = [\"{A}\"]")),
        key("k2", 2, "prefixes = [\"org.devshell.aibus\"]"),
    ));
    fetch(&w, A, "0.1.0", &p).unwrap();
    fetch(&w, B, "0.1.0", &p).unwrap();
}

#[test]
fn a_prefix_is_an_id_boundary_not_a_string_prefix() {
    let w = World::new();
    w.signed(X, "0.1.0", "k1", 1);
    let p = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, "prefixes = [\"org.devshell.aibus\"]")
    ));
    let err = fetch(&w, X, "0.1.0", &p).unwrap_err();
    assert_eq!(err.class(), "provenance_invalid", "{err:?}");
}

#[test]
fn an_exact_id_is_not_a_prefix_of_a_longer_one() {
    let w = World::new();
    w.signed("org.devshell.aibus.alpha2", "0.1.0", "k1", 1);
    let p = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, &format!("ids = [\"{A}\"]"))
    ));
    assert!(fetch(&w, "org.devshell.aibus.alpha2", "0.1.0", &p).is_err());
}

#[test]
fn a_scope_that_is_empty_a_wildcard_or_one_label_is_refused_with_a_clear_error() {
    for scope in [
        "",
        "ids = []",
        "prefixes = [\"*\"]",
        "prefixes = [\"org.*\"]",
        "prefixes = [\"org\"]",
        "prefixes = [\"\"]",
        "prefixes = [\"org.devshell.aibus.\"]",
        "ids = [\"org.devshell.*\"]",
    ] {
        let err = Policy::parse(&key("k1", 1, scope)).unwrap_err();
        assert!(err.contains("k1"), "{scope:?}: {err}");
    }
}

#[test]
fn case_and_unicode_lookalikes_never_match_and_never_parse() {
    // In the policy: refused as not ids, so a lookalike cannot sit in a scope unnoticed.
    Policy::parse(&key("k1", 1, "ids = [\"org.devshell.aibus.alpha\"]")).unwrap();
    for scope in [
        "ids = [\"Org.Devshell.Aibus.alpha\"]",
        "ids = [\"org.devshell.\u{430}ibus.alpha\"]",
        "prefixes = [\"org.devshell.\u{430}ibus\"]",
        "prefixes = [\"ORG.devshell\"]",
        "ids = [\"org.devshell.aibus.alpha\u{200b}\"]",
    ] {
        assert!(Policy::parse(&key("k1", 1, scope)).is_err(), "{scope:?}");
    }
    // In an index: refused as not extension ids, so a signature cannot be fetched for one.
    let w = World::new();
    w.signed(A, "0.1.0", "k1", 1);
    for lookalike in ["Org.Devshell.Aibus.alpha", "org.devshell.\u{430}ibus.alpha"] {
        w.edit_entry(A, "0.1.0", |e| e["id"] = lookalike.into());
        assert!(Registry::open(&w.reg()).is_err(), "{lookalike:?}");
        w.edit_entry(lookalike, "0.1.0", |e| e["id"] = A.into());
    }
}

#[test]
fn a_key_listed_twice_is_refused_whatever_the_scopes() {
    // The same key under two names with different scopes would be the union of both.
    let text = format!(
        "{}{}",
        key("k1", 1, &format!("ids = [\"{A}\"]")),
        key("k2", 1, &format!("ids = [\"{B}\"]")),
    );
    let err = Policy::parse(&text).unwrap_err();
    assert!(err.contains("k1") && err.contains("k2"), "{err}");
    // The same name twice is a TOML error, and a case change of the hex is the same key.
    let again = format!(
        "{}{}",
        key("k1", 1, &format!("ids = [\"{A}\"]")),
        key("k1", 1, &format!("ids = [\"{B}\"]"))
    );
    assert!(Policy::parse(&again).is_err());
    let upper = format!(
        "{}[trusted_keys.k2]\nkey = \"{}\"\nids = [\"{B}\"]\n",
        key("k1", 1, &format!("ids = [\"{A}\"]")),
        public(1).to_uppercase(),
    );
    assert!(Policy::parse(&upper).is_err());
}

// ---- the old file format ----

#[test]
fn an_old_policy_file_without_scopes_is_refused_not_migrated_and_says_what_to_write() {
    let old = format!(
        "require_signature = true\n[trusted_keys]\nk1 = \"{}\"\n",
        public(1)
    );
    let err = Policy::parse(&old).unwrap_err();
    assert!(err.contains("k1"), "{err}");
    assert!(err.contains("ids") && err.contains("prefixes"), "{err}");
    assert!(err.contains("[trusted_keys.k1]"), "{err}");
}

// ---- nothing but the user's policy widens a scope ----

#[test]
fn a_registry_entry_cannot_carry_a_scope() {
    let w = World::new();
    w.signed(B, "0.1.0", "k1", 1);
    for field in ["ids", "prefixes", "scope"] {
        w.edit_entry(B, "0.1.0", |e| {
            e["signature"][field] = serde_json::json!([B])
        });
        assert!(Registry::open(&w.reg()).is_err(), "signature.{field}");
        w.edit_entry(B, "0.1.0", |e| {
            e["signature"].as_object_mut().unwrap().remove(field);
        });
        w.edit_entry(B, "0.1.0", |e| e[field] = serde_json::json!([B]));
        assert!(Registry::open(&w.reg()).is_err(), "entry.{field}");
        w.edit_entry(B, "0.1.0", |e| {
            e.as_object_mut().unwrap().remove(field);
        });
    }
}

#[test]
fn a_package_cannot_carry_a_scope_for_its_own_key() {
    let t = tempfile::tempdir().unwrap();
    let m = format!(
        "{}\n[signing]\nids = [\"{B}\"]\nprefixes = [\"org\"]\n",
        template_as(B, "0.2.0")
    );
    let p = package_with(t.path(), "wide", &m);
    let ok = package_with(t.path(), "plain", &template_as(B, "0.2.0"));
    aibus_extension::package::read(&ok).unwrap();
    assert!(aibus_extension::package::read(&p).is_err());
}

#[test]
fn an_update_signed_by_a_key_outside_its_scope_is_quarantined_and_the_installed_version_stays() {
    let w = World::new();
    w.signed(A, "0.1.0", "k1", 1);
    let p = policy(&format!(
        "require_signature = true\n{}{}",
        key("k1", 1, &format!("ids = [\"{A}\"]")),
        key("k2", 2, &format!("ids = [\"{B}\"]"))
    ));
    let s = w.store();
    s.install(&w.open(), &p, A, None, &[]).unwrap();
    // The registry now offers 0.2.0 signed by k2, whose scope is B only: the update does not widen k2.
    w.signed(A, "0.2.0", "k2", 2);
    let err = s.update(&w.open(), &p, A, &[]).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
    assert!(err.to_string().contains("k2"), "{err}");
    assert_eq!(s.resolve(A, &p).unwrap().manifest.version, "0.1.0");
}

#[test]
fn an_install_outside_the_scope_is_quarantined_and_activates_nothing() {
    let w = World::new();
    w.signed(B, "0.1.0", "k1", 1);
    let p = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, &format!("ids = [\"{A}\"]"))
    ));
    let s = w.store();
    let err = s.install(&w.open(), &p, B, None, &[]).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
    let rec = &s.state().unwrap().extensions[B];
    assert!(rec.active.is_none() && rec.versions.is_empty());
    assert!(
        rec.quarantined[0].reason.contains("k1"),
        "{:?}",
        rec.quarantined
    );
}

#[test]
fn narrowing_the_scope_in_the_policy_stops_verify_resolve_and_rollback() {
    let w = World::new();
    w.signed(A, "0.1.0", "k1", 1);
    w.signed(A, "0.2.0", "k1", 1);
    let wide = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, "prefixes = [\"org.devshell.aibus\"]")
    ));
    let narrow = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, &format!("ids = [\"{B}\"]"))
    ));
    let s = w.store();
    s.install(
        &w.open(),
        &wide,
        A,
        Some(&aibus_extension::version::exact("0.1.0").unwrap()),
        &[],
    )
    .unwrap();
    s.install(
        &w.open(),
        &wide,
        A,
        Some(&aibus_extension::version::exact("0.2.0").unwrap()),
        &[],
    )
    .unwrap();
    // Rollback to 0.1.0, signed by the same key, is refused under the narrow scope.
    let err = s.rollback(A, &narrow).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
    // The active 0.2.0 is refused at resolve, and verify names the same reason.
    let err = s.resolve(A, &narrow).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
    let s2 = w.store();
    assert!(s2.resolve(A, &wide).is_err(), "the quarantine stays");
    let s3 = Store::open(&w.t.path().join("store2"));
    s3.install(
        &w.open(),
        &wide,
        A,
        Some(&aibus_extension::version::exact("0.1.0").unwrap()),
        &[],
    )
    .unwrap();
    let err = s3.verify(A, &[], &narrow).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
}

// ---- revocation of a key ----

#[test]
fn a_revoked_key_takes_only_its_own_ids_with_it() {
    let w = World::new();
    w.signed(A, "0.1.0", "k1", 1);
    w.signed(B, "0.1.0", "k2", 2);
    let both = format!(
        "{}{}",
        key("k1", 1, &format!("ids = [\"{A}\"]")),
        key("k2", 2, &format!("ids = [\"{B}\"]"))
    );
    let ok = policy(&format!("require_signature = true\n{both}"));
    let s = w.store();
    s.install(&w.open(), &ok, A, None, &[]).unwrap();
    s.install(&w.open(), &ok, B, None, &[]).unwrap();
    let revoked = policy(&format!(
        "require_signature = true\n\
         [trusted_keys.k1]\nkey = \"{}\"\nids = [\"{A}\"]\nrevoked = \"leaked\"\n{}",
        public(1),
        key("k2", 2, &format!("ids = [\"{B}\"]")),
    ));
    // A fresh fetch of A, an installed A and the other key's B.
    let err = fetch(&w, A, "0.1.0", &revoked).unwrap_err();
    assert_eq!(err.class(), "revoked", "{err:?}");
    assert!(err.to_string().contains("leaked"), "{err}");
    let err = s.verify(A, &[], &revoked).unwrap_err();
    assert_eq!(class(&err), "revoked", "{err:?}");
    fetch(&w, B, "0.1.0", &revoked).unwrap();
    s.verify(B, &[], &revoked).unwrap();
    s.resolve(B, &revoked).unwrap();
}

#[test]
fn an_id_a_revoked_key_could_sign_is_still_installable_with_another_trusted_key() {
    let w = World::new();
    w.signed(A, "0.1.0", "k2", 2);
    let p = policy(&format!(
        "require_signature = true\n[trusted_keys.k1]\nkey = \"{}\"\nids = [\"{A}\"]\nrevoked = \"leaked\"\n{}",
        public(1),
        key("k2", 2, &format!("ids = [\"{A}\"]")),
    ));
    fetch(&w, A, "0.1.0", &p).unwrap();
}

#[test]
fn a_revoked_key_cannot_be_given_an_empty_reason() {
    let text = format!(
        "[trusted_keys.k1]\nkey = \"{}\"\nids = [\"{A}\"]\nrevoked = \"\"\n",
        public(1)
    );
    assert!(Policy::parse(&text).is_err());
    Policy::parse(&text.replace("\"\"", "\"leaked\"")).unwrap();
}

// ---- a key taken out of the policy, and installs from before signers were recorded ----

fn installed_signed_by_k1(w: &World) -> (Store, String) {
    w.signed(A, "0.1.0", "k1", 1);
    let trusted = format!(
        "require_signature = true\n{}",
        key("k1", 1, &format!("ids = [\"{A}\"]"))
    );
    let s = w.store();
    s.install(&w.open(), &policy(&trusted), A, None, &[])
        .unwrap();
    (s, trusted)
}

#[test]
fn removing_a_key_from_the_policy_quarantines_what_it_signed() {
    let w = World::new();
    let (s, _) = installed_signed_by_k1(&w);
    // Another key stays, so the policy is not empty and still requires signatures.
    let without = policy(&format!(
        "require_signature = true\n{}",
        key("k2", 2, &format!("ids = [\"{B}\"]"))
    ));
    let err = s.resolve(A, &without).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
    assert!(err.to_string().contains("k1"), "{err}");
    let s2 = Store::open(&w.t.path().join("store2"));
    let trusted = policy(&format!(
        "require_signature = true\n{}",
        key("k1", 1, &format!("ids = [\"{A}\"]"))
    ));
    s2.install(&w.open(), &trusted, A, None, &[]).unwrap();
    let err = s2.verify(A, &[], &without).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
}

#[test]
fn renaming_a_key_keeps_it_trusted_and_its_new_scope_applies() {
    let w = World::new();
    let (s, _) = installed_signed_by_k1(&w);
    let renamed = policy(&format!(
        "require_signature = true\n{}",
        key("k9", 1, &format!("ids = [\"{A}\"]"))
    ));
    s.resolve(A, &renamed).unwrap();
    let renamed_narrow = policy(&format!(
        "require_signature = true\n{}",
        key("k9", 1, &format!("ids = [\"{B}\"]"))
    ));
    let err = s.resolve(A, &renamed_narrow).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
}

#[test]
fn an_install_from_before_signers_were_recorded_is_quarantined_only_when_a_signature_is_required() {
    let w = World::new();
    let (s, trusted) = installed_signed_by_k1(&w);
    // Make the record look like one written before this change.
    let path = w.t.path().join("store/state.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    v["extensions"][A]
        .as_object_mut()
        .unwrap()
        .remove("signers");
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    // Not required: it stays served, like a fresh unsigned install.
    let lax = policy(&trusted.replace("require_signature = true", "require_signature = false"));
    s.resolve(A, &lax).unwrap();
    s.verify(A, &[], &lax).unwrap();
    let err = s.resolve(A, &policy(&trusted)).unwrap_err();
    assert_eq!(class(&err), "provenance_invalid", "{err:?}");
    assert!(err.to_string().contains("install"), "{err}");
}

#[test]
fn an_unsigned_install_stays_served_when_the_policy_trusts_keys_but_does_not_require_one() {
    let w = World::new();
    let p = package_with(&w.t.path().join("src"), "u", &template_as(A, "0.1.0"));
    Registry::publish(&w.reg(), &p).unwrap();
    let pol = policy(&key("k2", 2, &format!("ids = [\"{B}\"]")));
    let s = w.store();
    s.install(&w.open(), &pol, A, None, &[]).unwrap();
    s.resolve(A, &pol).unwrap();
    s.verify(A, &[], &pol).unwrap();
}
