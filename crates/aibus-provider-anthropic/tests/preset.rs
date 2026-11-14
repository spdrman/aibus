//! The `anthropic` preset (issue #110): built from its definition, with the host swapped for
//! the fake server, it sends the key in `x-api-key` to `/v1/messages`, and a header from a
//! `[[provider]]` table rides along.

use std::path::PathBuf;

use aibus_provider::presets::preset;
use aibus_provider::{AuthMethodKind, ProviderConfig, ProviderFactory, StopReason};
use aibus_provider_anthropic::Factory;
use aibus_testkit::capture_logs;
use aibus_testkit::conformance::{check_case, Case, Expect, FakeServer, Fixture, Fixtures};

#[tokio::test]
async fn the_anthropic_preset_has_its_url_and_sends_the_key_in_x_api_key() {
    let p = preset("anthropic").unwrap();
    assert_eq!(p.base_url.as_deref(), Some("https://api.anthropic.com"));
    assert_eq!(p.env_vars, ["ANTHROPIC_API_KEY"]);

    let server = FakeServer::start().await;
    let mut def = p.clone();
    def.base_url = Some(server.uri());
    def.headers.insert("x-team".into(), "home".into());
    let provider = Factory
        .build(&ProviderConfig {
            def: Some(def),
            ..ProviderConfig::default()
        })
        .unwrap();

    let text = Fixture::load(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/stream_text.http"),
    );
    let fixtures = Fixtures {
        model: "claude-sonnet-4-5",
        kind: AuthMethodKind::ApiKey,
        auth: Some(("x-api-key", "")),
        cases: vec![],
        skipped: vec![],
    };
    let case = Case {
        name: "text",
        fixture: text,
        expect: Expect::Stream {
            text: Some("Hello, world"),
            stop: StopReason::EndTurn,
        },
    };
    let _logs = capture_logs();
    check_case(&provider, &server, &fixtures, &case).await;
    let seen = &server.requests()[0];
    assert_eq!(seen.target, "/v1/messages");
    assert_eq!(seen.header("x-team"), Some("home"));
}
