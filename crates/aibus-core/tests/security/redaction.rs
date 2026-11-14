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
    /// Secrets the credential store would have registered by the time this body came back.
    #[serde(default)]
    registered: Vec<String>,
}

fn corpus() -> Cases {
    let file = root().join("fixtures/redact/provider-errors.toml");
    toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap()
}

/// The redactor a row runs against: the patterns, plus what the store would have registered.
fn redactor_for(c: &Case) -> Redactor {
    let r = Redactor::new();
    for (i, secret) in c.registered.iter().enumerate() {
        r.register(&AccountName(format!("corpus-{i}").into()), secret);
    }
    r
}

/// The secrets of a row that `redact` left in its body. Empty is a pass.
fn survivors(c: &Case, redact: impl Fn(&str) -> String) -> Vec<&str> {
    let out = redact(&c.body);
    c.secrets
        .iter()
        .map(String::as_str)
        .filter(|s| out.contains(s))
        .collect()
}

fn cassettes() -> Vec<(PathBuf, String)> {
    let mut all = Vec::new();
    for krate in [
        "aibus-provider-anthropic",
        "aibus-provider-openai",
        "aibus-provider-gemini",
    ] {
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
    assert!(
        all.len() > 40,
        "found {} cassettes, the corpus moved?",
        all.len()
    );
    for (path, text) in all {
        assert_eq!(r.redact(&text), text, "{} changed", path.display());
    }
}

#[test]
fn echoed_credentials_are_gone_from_every_provider_error_shape() {
    let cases = corpus();
    assert!(cases.case.len() >= 20);
    for c in &cases.case {
        let r = redactor_for(c);
        let left = survivors(c, |t| r.redact(t).into_owned());
        assert!(
            left.is_empty(),
            "{}: {left:?} survived in {}",
            c.name,
            r.redact(&c.body)
        );
        assert!(
            r.redact(&c.body).contains("[redacted:"),
            "{}: nothing was marked",
            c.name
        );
    }
}

#[test]
fn every_row_turns_red_when_the_redactor_misses_its_secret() {
    for c in &corpus().case {
        // A redactor that does nothing misses every row.
        assert_eq!(
            survivors(c, str::to_owned).len(),
            c.secrets.len(),
            "{}: the row cannot fail",
            c.name
        );
        // A secret listed as registered is only caught by registration: without it, it stays.
        let bare = Redactor::new();
        for s in &c.registered {
            let left = survivors(c, |t| bare.redact(t).into_owned());
            assert!(
                left.contains(&s.as_str()),
                "{}: {s} is caught by a pattern, so it is not a `registered` row",
                c.name
            );
        }
    }
}

#[test]
fn every_credential_kind_the_code_can_hold_has_a_row_and_the_right_layer_catches_it() {
    let cases = corpus();
    for (kind, value, pattern) in aibus_testkit::CREDENTIAL_KINDS {
        let rows: Vec<&Case> = cases
            .case
            .iter()
            .filter(|c| c.secrets.iter().any(|s| s == value))
            .collect();
        assert!(
            !rows.is_empty(),
            "{kind}: no row in provider-errors.toml plants it"
        );
        match pattern {
            // Known by shape: a bare redactor has to catch it, and the row must not lean on registration.
            Some(name) => {
                let out = Redactor::new().redact(value).into_owned();
                assert!(
                    out.contains(&format!("[redacted:pattern:{name}]")),
                    "{kind}: pattern {name} misses it: {out}"
                );
                assert!(
                    rows.iter().all(|c| c.registered.is_empty()),
                    "{kind}: a pattern kind with a registered row"
                );
            }
            None => assert!(
                rows.iter().any(|c| c.registered.iter().any(|s| s == value)),
                "{kind}: no pattern knows it, so its row must register it"
            ),
        }
    }
}

#[test]
fn quota_and_content_filter_bodies_come_through_untouched() {
    let cases = corpus();
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
    assert!(
        !out.contains(FAKE_KEY) && out.contains("[redacted:work]"),
        "{out}"
    );
}
