//! OpenRouter reports what a request cost in `usage.cost` (#116). That figure replaces the
//! table's estimate and `cost_source` says so; a response without it falls back to the table.

mod common;

use aibus_provider::presets::preset;
use aibus_provider::{AuthMethodKind, CostSource, ModelId, ProviderConfig, ProviderFactory};
use aibus_provider_openai::Factory;
use common::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

async fn run(cassette_name: &str, preset_id: &str) -> (aibus_provider::Usage, serde_json::Value) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(cassette(cassette_name).template())
        .mount(&server)
        .await;
    let mut def = preset(preset_id).unwrap().clone();
    def.base_url = Some(server.uri());
    let cfg = ProviderConfig {
        def: Some(def),
        ..ProviderConfig::default()
    };
    let p = Factory::compatible().build(&cfg).unwrap();
    let mut req = request("hi");
    req.model = ModelId("openai/gpt-4.1".into());
    let (result, _) = collect(&*p, &cred(AuthMethodKind::ApiKey), req).await;
    let usage = result.unwrap().usage;
    let sent: serde_json::Value =
        serde_json::from_slice(&server.received_requests().await.unwrap().remove(0).body).unwrap();
    (usage, sent)
}

#[tokio::test]
async fn openrouter_reported_cost_replaces_the_estimate_and_is_marked() {
    let (usage, sent) = run("openrouter_with_cost", "openrouter").await;
    assert_eq!(usage.cost_usd, 0.00042);
    assert_eq!(usage.cost_source, CostSource::Provider);
    assert_eq!(
        sent["usage"],
        serde_json::json!({"include": true}),
        "the gateway is asked to report it"
    );
}

#[tokio::test]
async fn openrouter_without_a_reported_cost_falls_back_to_the_table() {
    let (usage, _) = run("openrouter_without_cost", "openrouter").await;
    assert_eq!(usage.cost_source, CostSource::Table);
    // `openai/gpt-4.1` is the one priced-less row of the preset's table: n/a is 0, never a guess.
    assert_eq!(usage.cost_usd, 0.0);
}

#[tokio::test]
async fn another_vendor_is_not_trusted_for_a_cost_and_is_not_asked() {
    let (usage, sent) = run("openrouter_with_cost", "groq").await;
    assert_eq!(usage.cost_source, CostSource::Table);
    assert_eq!(usage.cost_usd, 0.0);
    assert!(sent.get("usage").is_none());
}
