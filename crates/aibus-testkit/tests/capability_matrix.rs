//! `docs/CAPABILITIES.md` is generated: `capability_matrix::render` is a pure function with a
//! golden test, and the committed page has to be what it returns for the real presets, built
//! through the real adapters. Set `AIBUS_WRITE_CAPABILITIES=1` to rewrite the page instead of
//! failing (run it in a container).

use std::path::PathBuf;
use std::sync::Arc;

use aibus_provider::presets::{presets, ModelTableSpec, ProviderDef};
use aibus_provider::{Api, Capabilities, ProviderConfig, ProviderFactory, SystemRole};
use aibus_testkit::capability_matrix::{render, Row};
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn caps(tools: bool, thinking: bool, images: bool) -> Capabilities {
    Capabilities {
        tools,
        parallel_tool_calls: tools,
        streaming: true,
        thinking,
        prompt_cache: false,
        logprobs: false,
        effort: thinking,
        images,
        system_role: SystemRole::Separate,
        max_tools: None,
    }
}

#[test]
fn render_is_a_pure_function_and_matches_its_golden_page() {
    let rows = [
        Row {
            def: ProviderDef {
                id: "alpha".into(),
                api: Some(Api::OpenaiChat),
                auth_methods: vec!["api_key".into(), "env".into()],
                compat: Some(vec![
                    serde_json::from_str(r#"{"max_tokens_field":"max_completion_tokens"}"#)
                        .unwrap(),
                    serde_json::from_str(r#""no_store""#).unwrap(),
                ]),
                ..ProviderDef::default()
            },
            caps: caps(true, true, true),
            catalog_rows: 3,
        },
        Row {
            def: ProviderDef {
                id: "beta".into(),
                api: Some(Api::AnthropicVertex),
                auth_methods: vec!["none".into()],
                ..ProviderDef::default()
            },
            caps: caps(true, false, false),
            catalog_rows: 0,
        },
    ];
    let page = render(&rows);
    assert_eq!(page, render(&rows), "the same rows give the same page");
    assert_eq!(
        page,
        std::fs::read_to_string(root().join("fixtures/capability-matrix/golden.md")).unwrap()
    );
}

/// The factories the binary registers, with a Vertex token source that is never asked.
fn factories() -> Vec<Box<dyn ProviderFactory>> {
    struct NoTokens;
    #[async_trait::async_trait]
    impl aibus_provider_anthropic::TokenSource for NoTokens {
        async fn token(
            &self,
            _: &aibus_provider::Credential,
        ) -> Result<aibus_provider::Secret<String>, aibus_provider::ProviderError> {
            Err(aibus_provider::ProviderError::new(
                aibus_provider::ProviderErrorKind::Auth,
                "no token in the matrix test",
            ))
        }
    }
    vec![
        Box::new(aibus_provider_anthropic::Factory),
        Box::new(aibus_provider_anthropic::CloudFactory {
            tokens: Arc::new(NoTokens),
        }),
        Box::new(aibus_provider_openai::Factory::openai()),
        Box::new(aibus_provider_gemini::Factory),
    ]
}

/// What the adapter for `def` reports, built the way the binary builds it. The settings a
/// preset leaves to the user (a base URL, a region, a project) get a placeholder: nothing
/// is sent anywhere.
fn adapter_caps(def: &ProviderDef) -> Capabilities {
    let mut def = def.clone();
    def.base_url
        .get_or_insert_with(|| "http://127.0.0.1:1/v1".into());
    def.region.get_or_insert_with(|| "us-east-1".into());
    def.project.get_or_insert_with(|| "matrix".into());
    let api = def.api.expect("every preset has an api").as_str();
    let factories = factories();
    let factory = factories
        .iter()
        .find(|f| f.apis().contains(&api))
        .unwrap_or_else(|| panic!("no adapter serves {api}"));
    let cfg = ProviderConfig {
        def: Some(def.clone()),
        ..ProviderConfig::default()
    };
    factory
        .build(&cfg)
        .unwrap_or_else(|e| panic!("{}: {e}", def.id))
        .capabilities()
}

fn catalog_rows(def: &ProviderDef) -> usize {
    let Some(ModelTableSpec::Named(name)) = &def.model_table else {
        return 0;
    };
    let text =
        aibus_provider::catalog::table(name).unwrap_or_else(|| panic!("no catalog table {name}"));
    let v: Value = serde_json::from_str(text).unwrap();
    v["models"].as_array().map_or(0, Vec::len)
}

#[test]
fn the_committed_matrix_is_what_the_presets_and_the_catalog_say() {
    let rows: Vec<Row> = presets()
        .iter()
        .map(|def| Row {
            caps: adapter_caps(def),
            catalog_rows: catalog_rows(def),
            def: def.clone(),
        })
        .collect();
    let page = render(&rows);
    let path = root().join("docs/CAPABILITIES.md");
    if std::env::var_os("AIBUS_WRITE_CAPABILITIES").is_some() {
        std::fs::write(&path, &page).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(committed, page, "docs/CAPABILITIES.md is out of date: run AIBUS_WRITE_CAPABILITIES=1 cargo test -p aibus-testkit --test capability_matrix");
    // One row per preset, and a preset's default capabilities are what the table says.
    assert_eq!(
        page.lines().filter(|l| l.starts_with("| `")).count(),
        presets().len()
    );
}
