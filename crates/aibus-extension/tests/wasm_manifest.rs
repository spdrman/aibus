//! A manifest that names a WASM module (#333): the new fields, the package checks, the hash and
//! signature that cover the module like every other file, and the build without the feature.
#![cfg(unix)]

mod common;

use std::fs;

use aibus_extension::manifest::{escalations, Manifest};
use aibus_extension::package;
use common::{edit, template_manifest, wasm_manifest, wasm_package};

#[cfg(not(feature = "wasm"))]
#[test]
fn a_build_without_the_feature_refuses_a_manifest_that_names_a_module_and_says_why() {
    let err = Manifest::parse(&wasm_manifest()).unwrap_err().to_string();
    assert!(
        err.contains("normalizer.engine") && err.contains("without WASM support"),
        "{err}"
    );
}

#[cfg(feature = "wasm")]
mod with_the_host {
    use super::*;
    use aibus_extension::policy::Policy;
    use aibus_extension::registry::{signed_message, Refusal, Registry, Signature};
    use common::{guest_wasm, registry_of, template_as};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn a_manifest_may_name_a_wasm_module_and_nothing_else_about_it() {
        let m = Manifest::parse(&wasm_manifest()).unwrap();
        assert_eq!(m.normalizer.module.as_deref(), Some("wasm/parser.wasm"));
        assert!(m.normalizer.rule.is_empty());
    }

    #[test]
    fn the_module_field_is_checked() {
        let base = wasm_manifest();
        for (what, bad) in [
            ("a path that climbs", "wasm/../x.wasm"),
            ("an absolute path", "/etc/passwd"),
            ("a different directory", "other/parser.wasm"),
            ("not a .wasm file", "wasm/parser.bin"),
            ("a space", "wasm/my parser.wasm"),
        ] {
            let m = edit(&base, "wasm/parser.wasm", bad);
            assert!(Manifest::parse(&m).is_err(), "{what}");
        }
        // No module, or a module next to rules or the declarative engine, is a contradiction.
        assert!(Manifest::parse(&edit(&base, "module = \"wasm/parser.wasm\"\n", "")).is_err());
        let declarative = template_manifest();
        let with_module = edit(
            &declarative,
            "engine = \"declarative-jsonl\"",
            "engine = \"declarative-jsonl\"\nmodule = \"wasm/parser.wasm\"",
        );
        assert!(Manifest::parse(&with_module).is_err());
        let with_rule = format!(
            "{base}\n[[normalizer.rule]]\npointer = \"/type\"\nequals = \"x\"\nkind = \"message_chunk\"\n"
        );
        assert!(Manifest::parse(&with_rule.replace("# Every capability", "# c")).is_err());
    }

    #[test]
    fn a_package_whose_module_is_missing_or_not_a_module_is_refused_when_it_is_read() {
        let t = tempfile::tempdir().unwrap();
        let dir = wasm_package(t.path(), "ok", &wasm_manifest(), &guest_wasm());
        assert!(package::read(&dir).is_ok());

        fs::remove_file(dir.join("wasm/parser.wasm")).unwrap();
        assert!(package::read(&dir).is_err());

        let junk = wasm_package(t.path(), "junk", &wasm_manifest(), b"MZ this is not wasm");
        assert!(package::read(&junk).is_err());

        let wasi = wat::parse_str(
            r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
                 (memory (export "memory") 1)
                 (func (export "on_line") (result i32) (i32.const 0))
                 (func (export "on_finish") (result i32) (i32.const 0)))"#,
        )
        .unwrap();
        let w = wasm_package(t.path(), "wasi", &wasm_manifest(), &wasi);
        let err = package::read(&w).unwrap_err().to_string();
        assert!(err.contains("import"), "{err}");
    }

    #[test]
    fn the_module_is_part_of_the_content_hash() {
        let t = tempfile::tempdir().unwrap();
        let a = package::read(&wasm_package(
            t.path(),
            "a",
            &wasm_manifest(),
            &guest_wasm(),
        ))
        .unwrap();
        let mut other = guest_wasm();
        // A custom section at the end: still a valid module, different bytes.
        other.extend_from_slice(&[0, 5, 4, b'n', b'o', b't', b'e']);
        let b = package::read(&wasm_package(t.path(), "b", &wasm_manifest(), &other)).unwrap();
        assert_ne!(a.hash, b.hash);
        assert!(a.files.contains_key("wasm/parser.wasm"));
    }

    #[test]
    fn a_module_swapped_after_it_was_indexed_or_signed_is_a_provenance_failure() {
        let t = tempfile::tempdir().unwrap();
        let manifest = edit(
            &wasm_manifest(),
            "org.example.aibus.examplecli",
            "org.example.aibus.wasmcli",
        );
        let pkg = wasm_package(&t.path().join("src"), "a", &manifest, &guest_wasm());
        let reg = t.path().join("reg");
        let r0 = registry_of(&reg, &[pkg]);
        let e = r0.entries[0].clone();
        let pair = Ed25519KeyPair::from_seed_unchecked(&[7u8; 32]).unwrap();
        let sig = pair.sign(&signed_message(&e.id, &e.version, &e.hash));
        let index_path = reg.join(aibus_extension::registry::INDEX_FILE);
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&index_path).unwrap()).unwrap();
        v["entries"][0]["signature"] = serde_json::to_value(Signature {
            key_id: "k1".into(),
            signature: hex(sig.as_ref()),
        })
        .unwrap();
        fs::write(&index_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        let policy = Policy::parse(&format!(
            "require_signature = true\n[trusted_keys.k1]\nkey = \"{}\"\nids = [\"org.example.aibus.wasmcli\"]\n",
            hex(pair.public_key().as_ref())
        ))
        .unwrap();
        let r = Registry::open(&reg).unwrap();
        assert!(r.fetch(&r.entries[0], &policy).is_ok());

        // Same size, one byte flipped in the module: the index hash no longer matches.
        let module_path = reg.join(&e.path).join("wasm/parser.wasm");
        let mut bytes = fs::read(&module_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&module_path, bytes).unwrap();
        let err = r.fetch(&r.entries[0], &policy).unwrap_err();
        assert!(matches!(err, Refusal::HashMismatch { .. }), "{err:?}");
        assert_eq!(err.class(), "provenance_invalid");
        let _ = template_as; // the helper's id is the template's; this test uses its own
    }

    #[test]
    fn moving_from_rules_to_a_module_is_an_escalation_the_user_accepts_by_name() {
        let old = Manifest::parse(&template_manifest()).unwrap();
        let new = Manifest::parse(&wasm_manifest()).unwrap();
        assert!(escalations(&old, &new).contains(&"engine:wasm-module".to_owned()));
        // A new module under the same engine name is new code, so it is an escalation too.
        let other = Manifest::parse(&edit(
            &wasm_manifest(),
            "wasm/parser.wasm",
            "wasm/other.wasm",
        ))
        .unwrap();
        assert!(escalations(&new, &other).contains(&"module:wasm/other.wasm".to_owned()));
    }
}
