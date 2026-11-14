//! A fixture per compat flag (`fixtures/compat/<flag>.json`): the request body the openai
//! crate sends with the flag, and without it, as JSON pointers. A preset that claims a flag is
//! run against that flag's shape, so a claim nobody honours is a red test, not a quiet lie.
//! Nothing here reaches a real vendor.

use std::path::PathBuf;

use aibus_provider::presets::{preset, presets, CompatFlag};
use aibus_provider::{
    Api, CacheHint, ChatRequest, ContentBlock, Effort, Message, MessageRole, ModelId,
    ProviderConfig, ProviderDef, ProviderFactory, RequestMetadata, ToolChoice, ToolSpec,
};
use aibus_provider_openai::Factory;
use aibus_testkit::conformance::{FakeServer, Fixture};
use aibus_testkit::fake_credential;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// The twelve flags, by the name the fixture file carries.
const FLAGS: [&str; 12] = [
    "no_strict_tools",
    "no_developer_role",
    "max_tokens_field",
    "reasoning_field",
    "reasoning_content_replay",
    "no_usage_in_stream",
    "no_tool_choice",
    "no_named_tool_choice",
    "no_store",
    "no_sampling_params",
    "no_system_role",
    "strip_images",
];

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn shape(name: &str) -> Value {
    let text = std::fs::read_to_string(dir().join("fixtures/compat").join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("flag {name} has no fixture: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixtures/compat/{name}.json: {e}"))
}

fn flag_name(f: &CompatFlag) -> String {
    match serde_json::to_value(f).unwrap() {
        Value::String(s) => s,
        Value::Object(o) => o.keys().next().unwrap().clone(),
        other => panic!("{other}"),
    }
}

/// The one request every fixture is checked with: a system prompt, a user turn with a picture,
/// a system note, and an assistant turn that thought before it answered.
fn request(model: &str) -> ChatRequest {
    let at = std::time::SystemTime::UNIX_EPOCH;
    ChatRequest {
        model: ModelId(model.into()),
        system: "Be terse.".into(),
        messages: vec![
            Message {
                role: MessageRole::User,
                blocks: vec![
                    ContentBlock::Text("hi".into()),
                    ContentBlock::Image {
                        media_type: "image/png".into(),
                        data: bytes::Bytes::from_static(b"\x89PNG"),
                    },
                ],
                at,
            },
            Message::text(MessageRole::System, "note"),
            Message {
                role: MessageRole::Assistant,
                blocks: vec![
                    ContentBlock::Thinking {
                        text: "hmm".into(),
                        redacted: false,
                        signature: None,
                    },
                    ContentBlock::Text("ok".into()),
                ],
                at,
            },
        ],
        tools: vec![ToolSpec {
            name: "read".into(),
            description: "Read".into(),
            schema: json!({"type": "object"}),
        }],
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

/// Sends the standard request through a provider built from `def` and returns the body the
/// server saw.
async fn body_for(mut def: ProviderDef, fx: &Value) -> Value {
    let server = FakeServer::start().await;
    def.base_url = Some(server.uri());
    def.default_capabilities.images = Some(true);
    if let Some(c) = fx.get("capabilities") {
        let over: aibus_provider::CapabilityOverrides = serde_json::from_value(c.clone()).unwrap();
        def.default_capabilities.effort = over.effort.or(def.default_capabilities.effort);
    }
    let (api, cassette) = match fx["api"].as_str().unwrap() {
        "openai-responses" => (Api::OpenaiResponses, "rs_stream_text"),
        _ => (Api::OpenaiChat, "oa_stream_text"),
    };
    def.api = Some(api);
    server.script(Fixture::load(
        &dir().join("cassettes").join(format!("{cassette}.http")),
    ));
    let provider = Factory::openai()
        .build(&ProviderConfig {
            def: Some(def),
            ..ProviderConfig::default()
        })
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let model = fx["model"].as_str().unwrap_or("gpt-4.1");
    // The answer does not matter, only the request.
    let _ = provider
        .stream(
            &fake_credential(),
            request(model),
            tx,
            CancellationToken::new(),
        )
        .await;
    drain.await.unwrap();
    serde_json::from_slice(&server.requests()[0].body).unwrap()
}

fn check(name: &str, body: &Value, want: &Value) {
    for (pointer, expected) in want.as_object().unwrap() {
        match (body.pointer(pointer), expected.as_str()) {
            (None, Some("<absent>")) => {}
            (got, Some("<absent>")) => {
                panic!("{name}: {pointer} should be absent, got {got:?}\n{body}")
            }
            (Some(got), _) => assert_eq!(got, expected, "{name}: {pointer}\n{body}"),
            (None, _) => panic!("{name}: {pointer} is missing\n{body}"),
        }
    }
}

fn def_with(flag: &Value) -> ProviderDef {
    let mut def = preset("openai-compatible").unwrap().clone();
    def.compat = Some(vec![serde_json::from_value(flag.clone()).unwrap()]);
    def
}

#[test]
fn every_flag_has_a_fixture_that_names_it() {
    for name in FLAGS {
        let fx = shape(name);
        let flag: CompatFlag =
            serde_json::from_value(fx["flag"].clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(flag_name(&flag), name);
        assert!(
            fx["with"].is_object() && fx["without"].is_object(),
            "{name} needs both shapes"
        );
    }
}

#[tokio::test]
async fn each_flag_changes_the_request_the_way_its_fixture_records() {
    for name in FLAGS {
        let fx = shape(name);
        check(
            &format!("{name} on"),
            &body_for(def_with(&fx["flag"]), &fx).await,
            &fx["with"],
        );
        let mut off = def_with(&fx["flag"]);
        off.compat = Some(vec![]);
        check(
            &format!("{name} off"),
            &body_for(off, &fx).await,
            &fx["without"],
        );
    }
}

#[tokio::test]
async fn a_preset_that_claims_a_flag_sends_the_recorded_shape() {
    let mut claimed = 0;
    for p in presets()
        .iter()
        .filter(|p| matches!(p.api, Some(Api::OpenaiChat | Api::OpenaiResponses)))
    {
        for flag in p.compat.as_deref().unwrap_or_default() {
            let name = flag_name(flag);
            let fx = shape(&name);
            // The preset's own flags all apply; this flag's shape must still hold.
            let mut def = p.clone();
            def.base_url = Some("http://placeholder.invalid".into());
            let body = body_for(def, &fx).await;
            check(&format!("{} claims {name}", p.id), &body, &fx["with"]);
            claimed += 1;
        }
    }
    assert!(
        claimed >= 2,
        "openai and azure-openai claim max_tokens_field, kimi claims reasoning_content_replay"
    );
}
