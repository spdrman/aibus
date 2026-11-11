//! 09 section 4: the redactor against provider error bodies. The recorded cassettes of both
//! provider crates carry no secret, so the redactor has to leave every one of them as it is
//! (a pattern that eats a request id would be a bug too). `fixtures/redact/provider-errors.toml`
//! holds the bodies that do carry one, in the shapes providers and gateways echo them.

use std::path::{Path, PathBuf};

use aibus_core::{AccountName, Redactor};
use aibus_provider::fake::FAKE_KEY;
use serde::Deserialize;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[derive(Deserialize)]
struct Cases {
    case: Vec<Case>,
    #[serde(default)]
    intact: Vec<Intact>,
}

/// A body with no secret in it: the redactor must not touch a word of it.
#[derive(Deserialize)]
struct Intact {
    name: String,
    body: String,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    body: String,
    secrets: Vec<String>,
}

fn cassettes() -> Vec<(PathBuf, String)> {
    let mut all = Vec::new();
    for krate in ["aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-gemini"] {
        let dir = root().join("crates").join(krate).join("tests/cassettes");
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "http") {
                let text = std::fs::read_to_string(&path).unwrap();
                all.push((path, text));
            }
        }
    }
    all
}

#[test]
fn recorded_provider_responses_come_back_unchanged() {
    let r = Redactor::new();
    let all = cassettes();
    assert!(all.len() > 40, "found {} cassettes, the corpus moved?", all.len());
    for (path, text) in all {
        assert_eq!(r.redact(&text), text, "{} changed", path.display());
    }
}

#[test]
fn echoed_credentials_are_gone_from_every_provider_error_shape() {
    let file = root().join("fixtures/redact/provider-errors.toml");
    let cases: Cases = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(cases.case.len() >= 8);
    let r = Redactor::new();
    for c in cases.case {
        let out = r.redact(&c.body);
        for secret in &c.secrets {
            assert!(!out.contains(secret.as_str()), "{}: {secret} survived in {out}", c.name);
        }
        assert!(out.contains("[redacted:"), "{}: nothing was marked in {out}", c.name);
    }
}

#[test]
fn quota_and_content_filter_bodies_come_through_untouched() {
    let file = root().join("fixtures/redact/provider-errors.toml");
    let cases: Cases = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(cases.intact.len() >= 4);
    let r = Redactor::new();
    for c in cases.intact {
        assert_eq!(r.redact(&c.body), c.body, "{} changed", c.name);
    }
}

#[test]
fn a_registered_key_is_replaced_in_a_body_even_when_no_pattern_knows_its_shape() {
    let r = Redactor::new();
    r.register(&AccountName("work".into()), FAKE_KEY);
    let body = format!(r#"{{"error":{{"message":"bad key {FAKE_KEY}"}}}}"#);
    let out = r.redact(&body);
    assert!(!out.contains(FAKE_KEY) && out.contains("[redacted:work]"), "{out}");
}
