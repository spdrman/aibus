//! A fixture per compat flag the Messages wire honours (`fixtures/compat/<flag>.json`): the
//! request body with the flag and without it, as JSON pointers. The flags that only concern an
//! OpenAI-shaped server (`max_tokens_field`, `no_store`, ...) have their fixtures in
//! `aibus-provider-openai`; here a definition that claims one of them is refused by the
//! absence of a fixture, so a preset cannot claim a flag no crate honours.

use std::path::PathBuf;

use aibus_provider::presets::{preset, presets, CompatFlag};
use aibus_provider::{
    Api, CacheHint, ChatRequest, ContentBlock, Effort, Message, MessageRole, ModelId, ProviderConfig,
    ProviderDef, ProviderFactory, RequestMetadata, ToolChoice, ToolSpec,
};
use aibus_provider_anthropic::Factory;
use aibus_testkit::conformance::{FakeServer, Fixture};
use aibus_testkit::fake_credential;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const HONOURED: [&str; 5] = ["no_tool_choice", "no_named_tool_choice", "no_sampling_params", "no_system_role", "strip_images"];

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn shape(name: &str) -> Value {
    let text = std::fs::read_to_string(dir().join("fixtures/compat").join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("flag {name} has no fixture here: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixtures/compat/{name}.json: {e}"))
}

fn request() -> ChatRequest {
    ChatRequest {
        model: ModelId("claude-sonnet-4-5".into()),
        system: "Be terse.".into(),
        messages: vec![Message {
            role: MessageRole::User,
            blocks: vec![
                ContentBlock::Text("hi".into()),
                ContentBlock::Image { media_type: "image/png".into(), data: bytes::Bytes::from_static(b"\x89PNG") },
            ],
            at: std::time::SystemTime::UNIX_EPOCH,
        }],
        tools: vec![ToolSpec { name: "read".into(), description: "Read".into(), schema: json!({"type": "object"}) }],
        tool_choice: ToolChoice::Named("read".into()),
        max_output_tokens: 1024,
        temperature: Some(0.5),
        effort: Some(Effort::Low),
        thinking: None,
        cache_hint: CacheHint::None,
        logprobs: false,
        metadata: RequestMetadata::default(),
    }
}

async fn body_for(mut def: ProviderDef, flags: Vec<CompatFlag>) -> Value {
    let server = FakeServer::start().await;
    def.base_url = Some(server.uri());
    def.compat = Some(flags);
    server.script(Fixture::load(&dir().join("cassettes/stream_text.http")));
    let provider = Factory.build(&ProviderConfig { def: Some(def), ..ProviderConfig::default() }).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let _ = provider.stream(&fake_credential(), request(), tx, CancellationToken::new()).await;
    drain.await.unwrap();
    serde_json::from_slice(&server.requests()[0].body).unwrap()
}

fn check(name: &str, body: &Value, want: &Value) {
    for (pointer, expected) in want.as_object().unwrap() {
        match (body.pointer(pointer), expected.as_str()) {
            (None, Some("<absent>")) => {}
            (got, Some("<absent>")) => panic!("{name}: {pointer} should be absent, got {got:?}\n{body}"),
            (Some(got), _) => assert_eq!(got, expected, "{name}: {pointer}\n{body}"),
            (None, _) => panic!("{name}: {pointer} is missing\n{body}"),
        }
    }
}

#[tokio::test]
async fn each_flag_changes_the_request_the_way_its_fixture_records() {
    for name in HONOURED {
        let fx = shape(name);
        let flag: CompatFlag = serde_json::from_value(fx["flag"].clone()).unwrap();
        let def = preset("anthropic").unwrap().clone();
        check(&format!("{name} on"), &body_for(def.clone(), vec![flag]).await, &fx["with"]);
        check(&format!("{name} off"), &body_for(def, vec![]).await, &fx["without"]);
    }
}

#[tokio::test]
async fn a_preset_that_claims_a_flag_sends_the_recorded_shape() {
    for p in presets().iter().filter(|p| matches!(p.api, Some(Api::AnthropicMessages | Api::AnthropicBedrock | Api::AnthropicVertex))) {
        for flag in p.compat.as_deref().unwrap_or_default() {
            let Value::String(name) = serde_json::to_value(flag).unwrap() else {
                panic!("{} claims {flag:?}, which the Messages wire does not honour", p.id);
            };
            let fx = shape(&name);
            check(&format!("{} claims {name}", p.id), &body_for(p.clone(), p.compat.clone().unwrap()).await, &fx["with"]);
        }
    }
}
