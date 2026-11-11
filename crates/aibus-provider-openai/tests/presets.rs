//! Every OpenAI-shaped preset (issue #110): the data in `presets.json` says the right base URL
//! and key variable, and an instance built from it sends the key in the right header to the
//! right path, checked through the conformance kit's fake server. Nothing here reaches a real
//! vendor.

use std::path::PathBuf;

use aibus_provider::presets::{preset, presets};
use aibus_provider::{Api, AuthMethodKind, ProviderConfig, ProviderFactory, StopReason};
use aibus_provider_openai::Factory;
use aibus_testkit::capture_logs;
use aibus_testkit::conformance::{check_case, Case, Expect, FakeServer, Fixture, Fixtures};

/// (id, base URL, the variable the key usually lives in). Written out here on purpose: the preset
/// file is reviewed like code, and this is the second pair of eyes on each URL.
const EXPECTED: [(&str, &str, &str); 19] = [
    ("openai", "https://api.openai.com/v1", "OPENAI_API_KEY"),
    ("kimi", "https://api.moonshot.ai/v1", "MOONSHOT_API_KEY"),
    ("groq", "https://api.groq.com/openai/v1", "GROQ_API_KEY"),
    ("mistral", "https://api.mistral.ai/v1", "MISTRAL_API_KEY"),
    ("deepseek", "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY"),
    ("xai", "https://api.x.ai/v1", "XAI_API_KEY"),
    ("together", "https://api.together.xyz/v1", "TOGETHER_API_KEY"),
    ("fireworks", "https://api.fireworks.ai/inference/v1", "FIREWORKS_API_KEY"),
    ("cerebras", "https://api.cerebras.ai/v1", "CEREBRAS_API_KEY"),
    ("deepinfra", "https://api.deepinfra.com/v1/openai", "DEEPINFRA_API_KEY"),
    ("perplexity", "https://api.perplexity.ai", "PERPLEXITY_API_KEY"),
    ("openrouter", "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
    ("gemini-openai", "https://generativelanguage.googleapis.com/v1beta/openai", "GEMINI_API_KEY"),
    ("ollama", "http://127.0.0.1:11434/v1", ""),
    ("lm-studio", "http://127.0.0.1:1234/v1", ""),
    ("vllm", "http://127.0.0.1:8000/v1", "VLLM_API_KEY"),
    ("llama-cpp", "http://127.0.0.1:8080/v1", ""),
    ("litellm", "http://127.0.0.1:4000/v1", "LITELLM_API_KEY"),
    ("azure-openai", "", "AZURE_OPENAI_API_KEY"),
];

#[test]
fn every_preset_has_its_url_its_key_variable_and_a_model_table() {
    for (id, url, var) in EXPECTED {
        let p = preset(id).unwrap_or_else(|| panic!("{id} is not a preset"));
        assert_eq!(p.base_url.as_deref().unwrap_or(""), url, "{id}");
        assert_eq!(p.env_vars.first().map_or("", String::as_str), var, "{id}");
        assert!(p.model_table.is_some(), "{id} has a model table (empty is spelled out)");
    }
    // The three that existed before, and the generic server kept for migration, are presets too.
    for id in ["anthropic", "openai-compatible"] {
        assert!(preset(id).is_some(), "{id}");
    }
    let openai_shaped = presets().iter().filter(|p| !matches!(p.api, Some(Api::AnthropicMessages | Api::GeminiGenerateContent))).count();
    assert_eq!(openai_shaped, EXPECTED.len() + 1, "a preset the table above does not know");
}

#[tokio::test]
async fn every_preset_sends_the_key_in_its_header_to_its_path() {
    let text = Fixture::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/oa_stream_text.http"));
    for p in presets().iter().filter(|p| !matches!(p.api, Some(Api::AnthropicMessages | Api::GeminiGenerateContent))) {
        let server = FakeServer::start().await;
        // The vendor's own path prefix (`/openai/v1`, `/inference/v1`) is kept; only the host
        // changes. Azure has no default host: its deployment URL is the user's to give.
        let own = p.base_url.clone().unwrap_or_else(|| "https://res.openai.azure.com/openai/deployments/d".into());
        let path = own.split_once("://").and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned())).unwrap_or_default();
        let mut def = p.clone();
        def.base_url = Some(format!("{}{path}", server.uri()));

        let provider = Factory::openai().build(&ProviderConfig { def: Some(def), ..ProviderConfig::default() }).unwrap();
        let (header, prefix) = match p.compat().auth_header {
            Some(h) => (Box::leak(h.into_boxed_str()) as &str, ""),
            None => ("authorization", "Bearer "),
        };
        let fixtures = Fixtures { model: "m", kind: AuthMethodKind::ApiKey, auth: Some((header, prefix)), cases: vec![], skipped: vec![] };
        let case = Case {
            name: "text",
            fixture: text.clone(),
            expect: Expect::Stream { text: Some("Hello, world"), stop: StopReason::EndTurn },
        };
        let _logs = capture_logs();
        check_case(&provider, &server, &fixtures, &case).await;

        let query = if p.query.is_empty() { String::new() } else { format!("?{}", p.query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")) };
        let target = &server.requests()[0].target;
        assert_eq!(target, &format!("{path}/chat/completions{query}"), "{}", p.id);
    }
}

#[tokio::test]
async fn extra_headers_from_a_provider_table_ride_along_and_never_carry_the_key() {
    let server = FakeServer::start().await;
    let mut def = aibus_provider::presets::resolve(
        "ollama-home",
        Some(&aibus_provider::ProviderDef {
            id: "ollama-home".into(),
            api: Some(Api::OpenaiChat),
            base_url: Some(server.uri()),
            headers: [("x-team".to_owned(), "home".to_owned())].into(),
            ..aibus_provider::ProviderDef::default()
        }),
    )
    .unwrap()
    .unwrap();
    def.auth_methods = vec!["none".into()];
    let provider = Factory::openai().build(&ProviderConfig { def: Some(def), ..ProviderConfig::default() }).unwrap();
    assert_eq!(provider.id().0, "ollama-home");

    let text = Fixture::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/local_stream_text.http"));
    let fixtures = Fixtures { model: "llama3.2", kind: AuthMethodKind::None, auth: None, cases: vec![], skipped: vec![] };
    let case = Case { name: "text", fixture: text, expect: Expect::Stream { text: Some("Hi there"), stop: StopReason::EndTurn } };
    let _logs = capture_logs();
    check_case(&provider, &server, &fixtures, &case).await;
    assert_eq!(server.requests()[0].header("x-team"), Some("home"));
}
