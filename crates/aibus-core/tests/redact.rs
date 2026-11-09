//! The redactor against a corpus (09 section 4): every pattern in `patterns.toml` has lines it
//! must catch and lines it must leave alone, and the `tracing` layer applies the same rules.

use std::io;
use std::sync::{Arc, Mutex};

use aibus_core::{AccountName, Redactor};
use tracing_subscriber::layer::SubscriberExt;

/// (pattern name, line, the part of the line that must be gone afterwards)
const POSITIVE: &[(&str, &str, &str)] = &[
    (
        "anthropic-key",
        "ANTHROPIC_API_KEY=sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv_wxyz-0123",
        "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv_wxyz-0123",
    ),
    (
        "openai-key",
        "using sk-AbCdEfGhIjKlMnOpQrStUvWxYz0123 for the call",
        "sk-AbCdEfGhIjKlMnOpQrStUvWxYz0123",
    ),
    (
        "github-token",
        "remote: ghp_0123456789abcdefghijklmnopqrstuvwxyz",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
    ),
    (
        "github-token",
        "GH_TOKEN gho_0123456789abcdefghijklmnopqrstuvwxyz set",
        "gho_0123456789abcdefghijklmnopqrstuvwxyz",
    ),
    (
        "github-fine-grained",
        "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789",
        "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789",
    ),
    (
        "slack-token",
        "SLACK=xoxb-1234567890-abcdefghijkl",
        "xoxb-1234567890-abcdefghijkl",
    ),
    (
        "slack-token",
        "xoxp-1234567890-1234567890-abcdef",
        "xoxp-1234567890-1234567890-abcdef",
    ),
    (
        "aws-access-key",
        "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
        "AKIAIOSFODNN7EXAMPLE",
    ),
    (
        "private-key",
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0123\nabcd\n-----END RSA PRIVATE KEY-----\nafter",
        "MIIEowIBAAKCAQEA0123",
    ),
    (
        "private-key",
        "-----BEGIN PRIVATE KEY-----\nMIIBVgIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----",
        "MIIBVgIBADANBgkqhkiG9w0BAQEFAASC",
    ),
    (
        "bearer",
        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz.0123456789",
        "abcdefghijklmnopqrstuvwxyz.0123456789",
    ),
    (
        "jwt",
        "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
    ),
    (
        "assignment",
        "curl 'https://x.test/api?user=1&token=abc123def456&v=2'",
        "abc123def456",
    ),
    ("assignment", "export PASSWORD=correct-horse-battery", "correct-horse-battery"),
    ("assignment", "db passwd=hunter2 host=x", "hunter2"),
    ("assignment", "ACCESS_TOKEN=zzzz9999", "zzzz9999"),
    (
        "openai-project-key",
        "Incorrect API key provided: sk-proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56_Qr78-St90",
        "sk-proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56_Qr78-St90",
    ),
    (
        "openai-project-key",
        "key sk-svcacct-Zz11Yy22Xx33Ww44Vv55Uu66Tt77 rejected",
        "sk-svcacct-Zz11Yy22Xx33Ww44Vv55Uu66Tt77",
    ),
    (
        "google-api-key",
        "GET https://generativelanguage.googleapis.com/v1beta/models?alt=sse&k=AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q",
        "AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q",
    ),
    ("api-key-field", r#"{"headers":{"x-api-key":"gw-9f8e7d6c5b4a3210"}}"#, "gw-9f8e7d6c5b4a3210"),
    ("api-key-field", "retry https://gw.test/v1/chat?api_key=gw-9f8e7d6c5b4a3210&n=1", "gw-9f8e7d6c5b4a3210"),
    ("api-key-field", "X-Goog-Api-Key: gw-9f8e7d6c5b4a3210", "gw-9f8e7d6c5b4a3210"),
];

const NEGATIVE: &[&str] = &[
    "the task-abcdefghijklmnopqrstuvwxyz is done",
    "sk-short",
    "ask-abcdefghijklmnopqrstuvwxyz0123",
    "a ghp_ prefix with nothing after it",
    "ghp_tooshort",
    "github_pat_ alone",
    "xoxz-1234567890-abcdefghijkl",
    "AKIA is a prefix; AKIAshort",
    "AKIAIOSFODNN7EXAMPLEEXTRA is longer than a key id",
    "-----BEGIN CERTIFICATE-----\nMIIC\n-----END CERTIFICATE-----",
    "Bearer short",
    "the bearer of this note",
    "eyJhbGciOiJIUzI1NiJ9 is a header with no payload",
    "max tokens=5 was a typo for tokens = 5",
    "passwords are stored in the keychain",
    "token budget exceeded",
    "sk-proj-****************************abcd is how a provider masks a key",
    "AIzaShort and AIza alone are not keys",
    "api_key is missing from the config",
    "the api key: short",
    "# Heading\nplain prose, paths like crates/aibus/src/main.rs and numbers 12345",
];

#[test]
fn every_pattern_catches_its_positive_lines() {
    let r = Redactor::new();
    for (name, line, secret) in POSITIVE {
        let out = r.redact(line);
        assert!(!out.contains(secret), "{name}: {secret} survived in {out}");
        assert!(
            out.contains(&format!("[redacted:pattern:{name}]")),
            "{name}: no tag in {out}"
        );
    }
}

#[test]
fn every_pattern_is_exercised_by_the_corpus() {
    let names: std::collections::BTreeSet<&str> = POSITIVE.iter().map(|p| p.0).collect();
    let spec = [
        "anthropic-key",
        "openai-key",
        "github-token",
        "github-fine-grained",
        "slack-token",
        "aws-access-key",
        "private-key",
        "bearer",
        "jwt",
        "assignment",
        "openai-project-key",
        "google-api-key",
        "api-key-field",
    ];
    for s in spec {
        assert!(names.contains(s), "no positive line for {s}");
    }
}

#[test]
fn the_negative_corpus_comes_back_unchanged() {
    let r = Redactor::new();
    for line in NEGATIVE {
        assert_eq!(r.redact(line), *line);
    }
}

#[test]
fn text_around_a_match_is_kept() {
    let r = Redactor::new();
    let out = r.redact("before sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv after");
    assert_eq!(out, "before [redacted:pattern:anthropic-key] after");
}

#[test]
fn a_registered_secret_goes_even_when_no_pattern_knows_its_shape() {
    let r = Redactor::new();
    r.register(&AccountName("kimi".into()), "kimi-plain-value-77");
    assert_eq!(
        r.redact("sent kimi-plain-value-77 to the server"),
        "sent [redacted:kimi] to the server"
    );
}

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_tracing_layer_redacts_registered_secrets_and_patterns() {
    let r = Redactor::new();
    r.register(&AccountName("work".into()), "live-credential-123");
    let sink = Sink::default();
    let w = sink.clone();
    let subscriber = tracing_subscriber::registry().with(aibus_core::redact::layer(r, move || w.clone()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(key = "live-credential-123", "calling with live-credential-123");
        tracing::warn!("saw Bearer abcdefghijklmnopqrstuvwxyz0123 in a header");
    });
    let out = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(!out.contains("live-credential-123"), "{out}");
    assert!(!out.contains("abcdefghijklmnopqrstuvwxyz0123"), "{out}");
    assert!(out.contains("[redacted:work]"), "{out}");
    assert!(out.contains("[redacted:pattern:bearer]"), "{out}");
    assert!(out.contains("calling with"), "{out}");
}
