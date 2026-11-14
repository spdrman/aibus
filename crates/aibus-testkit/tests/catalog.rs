//! The model catalog (`catalog/*.json`, #116): the move out of the provider crates changed no
//! row, `catalog-sync` produces the expected files offline, and the files stay consistent with
//! the presets and with the crates that include them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use aibus_provider::models::ModelTable;
use aibus_provider::presets::{presets, ModelTableSpec};
use aibus_provider::{ModelId, ProviderConfig};
use aibus_testkit::catalog_sync::{plan, SOURCES};
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(p: impl AsRef<Path>) -> String {
    let p = p.as_ref();
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn catalog_files() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(root().join("catalog")).expect("catalog/ exists") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            out.insert(
                p.file_stem().unwrap().to_string_lossy().into_owned(),
                read(&p),
            );
        }
    }
    out
}

/// Every row of the three tables as they were in the provider crates before the move:
/// (file, id, context_window, max_output, family, deprecated, effort, price as
/// (input, output, cache_read, cache_write)).
#[allow(clippy::type_complexity)]
const BEFORE: [(
    &str,
    &str,
    u32,
    u32,
    &str,
    bool,
    bool,
    Option<(f64, f64, Option<f64>, Option<f64>)>,
); 26] = [
    (
        "anthropic",
        "claude-opus-4-5",
        200000,
        64000,
        "opus",
        false,
        true,
        Some((5.0, 25.0, Some(0.5), Some(6.25))),
    ),
    (
        "anthropic",
        "claude-opus-4-1",
        200000,
        32000,
        "opus",
        false,
        false,
        Some((15.0, 75.0, Some(1.5), Some(18.75))),
    ),
    (
        "anthropic",
        "claude-opus-4-0",
        200000,
        32000,
        "opus",
        false,
        false,
        Some((15.0, 75.0, Some(1.5), Some(18.75))),
    ),
    (
        "anthropic",
        "claude-sonnet-4-5",
        200000,
        64000,
        "sonnet",
        false,
        false,
        Some((3.0, 15.0, Some(0.3), Some(3.75))),
    ),
    (
        "anthropic",
        "claude-sonnet-4-0",
        200000,
        64000,
        "sonnet",
        false,
        false,
        Some((3.0, 15.0, Some(0.3), Some(3.75))),
    ),
    (
        "anthropic",
        "claude-haiku-4-5",
        200000,
        64000,
        "haiku",
        false,
        false,
        Some((1.0, 5.0, Some(0.1), Some(1.25))),
    ),
    (
        "anthropic",
        "claude-3-7-sonnet-latest",
        200000,
        64000,
        "sonnet",
        true,
        false,
        Some((3.0, 15.0, Some(0.3), Some(3.75))),
    ),
    (
        "anthropic",
        "claude-3-5-haiku-latest",
        200000,
        8192,
        "haiku",
        true,
        false,
        Some((0.8, 4.0, Some(0.08), Some(1.0))),
    ),
    (
        "openai",
        "gpt-5",
        400000,
        128000,
        "gpt-5",
        false,
        true,
        Some((1.25, 10.0, Some(0.125), None)),
    ),
    (
        "openai",
        "gpt-5-mini",
        400000,
        128000,
        "gpt-5",
        false,
        true,
        Some((0.25, 2.0, Some(0.025), None)),
    ),
    (
        "openai",
        "gpt-5-nano",
        400000,
        128000,
        "gpt-5",
        false,
        true,
        Some((0.05, 0.4, Some(0.005), None)),
    ),
    (
        "openai",
        "gpt-4.1",
        1047576,
        32768,
        "gpt-4.1",
        false,
        false,
        Some((2.0, 8.0, Some(0.5), None)),
    ),
    (
        "openai",
        "gpt-4.1-mini",
        1047576,
        32768,
        "gpt-4.1",
        false,
        false,
        Some((0.4, 1.6, Some(0.1), None)),
    ),
    (
        "openai",
        "gpt-4.1-nano",
        1047576,
        32768,
        "gpt-4.1",
        false,
        false,
        Some((0.1, 0.4, Some(0.025), None)),
    ),
    (
        "openai",
        "gpt-4o",
        128000,
        16384,
        "gpt-4o",
        false,
        false,
        Some((2.5, 10.0, Some(1.25), None)),
    ),
    (
        "openai",
        "gpt-4o-mini",
        128000,
        16384,
        "gpt-4o",
        false,
        false,
        Some((0.15, 0.6, Some(0.075), None)),
    ),
    (
        "openai",
        "o3",
        200000,
        100000,
        "o",
        false,
        true,
        Some((2.0, 8.0, Some(0.5), None)),
    ),
    (
        "openai",
        "o4-mini",
        200000,
        100000,
        "o",
        false,
        true,
        Some((1.1, 4.4, Some(0.275), None)),
    ),
    (
        "kimi",
        "kimi-k2-0905-preview",
        262144,
        32768,
        "kimi-k2",
        false,
        false,
        Some((0.6, 2.5, Some(0.15), None)),
    ),
    (
        "kimi",
        "kimi-k2-turbo-preview",
        262144,
        32768,
        "kimi-k2",
        false,
        false,
        Some((1.15, 8.0, Some(0.15), None)),
    ),
    (
        "kimi",
        "kimi-k2-thinking",
        262144,
        32768,
        "kimi-k2",
        false,
        false,
        Some((0.6, 2.5, Some(0.15), None)),
    ),
    (
        "kimi",
        "kimi-k2-thinking-turbo",
        262144,
        32768,
        "kimi-k2",
        false,
        false,
        None,
    ),
    (
        "kimi",
        "kimi-latest",
        131072,
        32768,
        "kimi-latest",
        false,
        false,
        None,
    ),
    (
        "kimi",
        "moonshot-v1-8k",
        8192,
        4096,
        "moonshot-v1",
        false,
        false,
        None,
    ),
    (
        "kimi",
        "moonshot-v1-32k",
        32768,
        8192,
        "moonshot-v1",
        false,
        false,
        None,
    ),
    (
        "kimi",
        "moonshot-v1-128k",
        131072,
        16384,
        "moonshot-v1",
        false,
        false,
        None,
    ),
];

#[test]
fn the_move_to_catalog_changed_no_models_or_price_result() {
    let tables: BTreeMap<&str, ModelTable> = [
        ("anthropic", include_str!("../../../catalog/anthropic.json")),
        ("openai", include_str!("../../../catalog/openai.json")),
        ("kimi", include_str!("../../../catalog/kimi.json")),
    ]
    .into_iter()
    .map(|(n, text)| {
        (
            n,
            ModelTable::load(n, text, &ProviderConfig::default(), &[], false).unwrap(),
        )
    })
    .collect();

    for (file, id, ctx, out, family, deprecated, effort, price) in BEFORE {
        let t = &tables[file];
        let info = t
            .models()
            .into_iter()
            .find(|m| m.id.0 == id)
            .unwrap_or_else(|| panic!("{file}/{id} missing"));
        assert_eq!(
            (
                info.context_window,
                info.max_output,
                info.family.as_str(),
                info.deprecated
            ),
            (ctx, out, family, deprecated),
            "{id}"
        );
        assert_eq!(t.entry(&ModelId(id.into())).unwrap().effort, effort, "{id}");
        let got = t.price(&ModelId(id.into())).map(|p| {
            (
                p.input_per_mtok,
                p.output_per_mtok,
                p.cache_read_per_mtok,
                p.cache_write_per_mtok,
            )
        });
        assert_eq!(got, price, "{id}");
    }
    for (file, t) in &tables {
        let n = BEFORE.iter().filter(|r| r.0 == *file).count();
        assert_eq!(
            t.models().len(),
            n,
            "{file} has rows the golden list does not"
        );
    }
}

#[test]
fn every_preset_has_a_catalog_file_that_parses_and_prices_are_complete() {
    let files = catalog_files();
    let mut named = Vec::new();
    for p in presets() {
        match &p.model_table {
            // A server that lists its own models (a local one, Azure) ships no table.
            Some(ModelTableSpec::Named(n)) if n == "empty" => {}
            Some(ModelTableSpec::Named(n)) => {
                assert!(
                    files.contains_key(n),
                    "preset {} names table {n}, which has no catalog/{n}.json",
                    p.id
                );
                assert!(
                    aibus_provider::catalog::table(n).is_some(),
                    "catalog::table does not know {n}"
                );
                named.push(n.clone());
            }
            other => panic!(
                "preset {} keeps its table inline ({other:?}); it belongs in catalog/",
                p.id
            ),
        }
    }
    for name in files.keys() {
        assert!(
            named.contains(name),
            "catalog/{name}.json is not used by any preset"
        );
        assert!(
            SOURCES.iter().any(|(n, _)| n == name),
            "catalog-sync does not know where {name} comes from"
        );
    }
    for (name, text) in &files {
        let v: Value =
            serde_json::from_str(text).unwrap_or_else(|e| panic!("catalog/{name}.json: {e}"));
        ModelTable::load(name, text, &ProviderConfig::default(), &[], false)
            .unwrap_or_else(|_| panic!("catalog/{name}.json is not a model table"));
        for row in v["models"].as_array().unwrap() {
            let id = &row["id"];
            if let Some(price) = row.get("price") {
                for bucket in [
                    "input_per_mtok",
                    "output_per_mtok",
                    "cache_read_per_mtok",
                    "cache_write_per_mtok",
                ] {
                    assert!(
                        price.get(bucket).is_some(),
                        "catalog/{name}.json: {id} has no {bucket} (write null for none)"
                    );
                }
                assert!(
                    price["input_per_mtok"].is_number() && price["output_per_mtok"].is_number(),
                    "{name}: {id}"
                );
            }
        }
    }
}

#[test]
fn only_aibus_provider_includes_tables_and_only_from_catalog_so_no_crate_keeps_a_copy() {
    for krate in [
        "aibus-provider",
        "aibus-provider-anthropic",
        "aibus-provider-openai",
        "aibus-provider-gemini",
    ] {
        let src = root().join("crates").join(krate).join("src");
        for e in std::fs::read_dir(&src).unwrap() {
            let p = e.unwrap().path();
            let is_preset_file = p.file_name().is_some_and(|n| n == "presets.json");
            assert!(
                p.extension().is_none_or(|x| x != "json") || is_preset_file,
                "{} is a second copy of a table",
                p.display()
            );
            if p.extension().is_some_and(|x| x == "rs") {
                for line in read(&p)
                    .lines()
                    .filter(|l| l.contains("include_str!") && l.contains(".json"))
                {
                    assert!(
                        line.contains("/catalog/") || line.contains("presets.json"),
                        "{}: {line}",
                        p.display()
                    );
                }
            }
        }
    }
}

#[test]
fn catalog_files_are_in_the_layout_the_sync_tool_writes() {
    let fixtures = root().join("fixtures/catalog-sync");
    // Round trip: syncing a catalog against a snapshot that says nothing about it changes nothing.
    let (files, _) = plan(&serde_json::json!({}), &catalog_files(), false, false).unwrap();
    assert!(files.is_empty());
    for (name, text) in catalog_files() {
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            aibus_testkit::catalog_sync::render(&v),
            text,
            "catalog/{name}.json is not in canonical layout"
        );
    }
    assert!(fixtures.join("snapshot.json").exists());
}

fn fixture_snapshot() -> Value {
    serde_json::from_str(&read(root().join("fixtures/catalog-sync/snapshot.json"))).unwrap()
}

fn before(names: &[&str]) -> BTreeMap<String, String> {
    names
        .iter()
        .map(|n| {
            (
                (*n).to_owned(),
                read(root().join(format!("fixtures/catalog-sync/before/{n}.json"))),
            )
        })
        .collect()
}

#[test]
fn sync_on_the_committed_snapshot_gives_the_expected_files_and_leaves_dated_rows_alone() {
    let (files, notes) = plan(&fixture_snapshot(), &before(&["anthropic"]), false, false).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        files["anthropic"],
        read(root().join("fixtures/catalog-sync/expected/anthropic.json"))
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("claude-haiku-4-5") && n.contains("input_per_mtok 1.0 -> 1.1")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("claude-newcomer-1") && n.contains("--add-new")),
        "{notes:?}"
    );
    assert!(!files["anthropic"].contains("claude-newcomer-1"));

    // Running it again on its own output is a no-op.
    let again: BTreeMap<String, String> = files.clone();
    let (files2, _) = plan(&fixture_snapshot(), &again, false, false).unwrap();
    assert!(files2.is_empty());
}

#[test]
fn add_new_adds_the_models_the_catalog_does_not_have() {
    let (files, _) = plan(&fixture_snapshot(), &before(&["anthropic"]), true, false).unwrap();
    assert_eq!(
        files["anthropic"],
        read(root().join("fixtures/catalog-sync/expected/anthropic-add-new.json"))
    );
}

#[test]
fn sync_refuses_to_drop_a_price_bucket_unless_told_to() {
    let err = plan(
        &fixture_snapshot(),
        &before(&["anthropic", "openai"]),
        false,
        false,
    )
    .unwrap_err();
    assert!(
        err.contains("gpt-4.1") && err.contains("cache_read_per_mtok"),
        "{err}"
    );
    assert!(err.contains("--allow-price-removal"), "{err}");

    let (files, _) = plan(&fixture_snapshot(), &before(&["openai"]), false, true).unwrap();
    assert_eq!(
        files["openai"],
        read(root().join("fixtures/catalog-sync/expected/openai-allowed.json"))
    );
}

#[test]
fn sync_reads_a_snapshot_and_catalog_without_touching_them() {
    // `plan` is pure: the inputs are values, the outputs are strings, and a provider the
    // snapshot lacks is reported and left alone.
    let (files, notes) = plan(
        &serde_json::json!({"anthropic": {"models": {}}}),
        &before(&["openai"]),
        false,
        false,
    )
    .unwrap();
    assert!(files.is_empty());
    assert!(
        notes.iter().any(|n| n.contains("no \"openai\" provider")),
        "{notes:?}"
    );
}

#[test]
fn a_dated_price_starts_on_its_second_and_not_before() {
    let from = 1_900_000_000;
    let text = read(root().join("fixtures/catalog-sync/before/anthropic.json"));
    let t = ModelTable::load("anthropic", &text, &ProviderConfig::default(), &[], false).unwrap();
    let id = ModelId("claude-haiku-4-5".into());
    let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
    assert_eq!(t.price_at(&id, at(from - 1)).unwrap().input_per_mtok, 1.0);
    assert_eq!(t.price_at(&id, at(from)).unwrap().input_per_mtok, 0.9);
    assert_eq!(
        t.price_at(&id, at(from + 1)).unwrap().cache_write_per_mtok,
        Some(1.1)
    );
    assert_eq!(t.models().iter().filter(|m| m.id == id).count(), 1);
}

#[test]
fn the_bedrock_and_vertex_tables_price_every_row_through_price_of_to_a_plain_claude_row() {
    let files = catalog_files();
    let anthropic: Value = serde_json::from_str(&files["anthropic"]).unwrap();
    let plain: Vec<&str> = anthropic["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    for name in ["bedrock", "vertex"] {
        let table: Value = serde_json::from_str(&files[name])
            .unwrap_or_else(|e| panic!("catalog/{name}.json: {e}"));
        let rows = table["models"].as_array().unwrap();
        assert!(rows.len() >= 4, "{name}: a few current models");
        for row in rows {
            let id = row["id"].as_str().unwrap();
            let target = row["price_of"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: {id} has no price_of"));
            assert!(
                plain.contains(&target),
                "{name}: {id} is priced like {target}, which the anthropic table does not have"
            );
            let t = ModelTable::load(name, &files[name], &ProviderConfig::default(), &[], false)
                .unwrap();
            let own = t.price(&aibus_provider::ModelId(id.into()));
            let plain_price = ModelTable::load(
                "anthropic",
                &files["anthropic"],
                &ProviderConfig::default(),
                &[],
                false,
            )
            .unwrap()
            .price(&aibus_provider::ModelId(target.into()));
            assert!(own.is_some() && own == plain_price, "{name}: {id}");
        }
    }
}

#[test]
fn the_sync_writes_aibus_capability_names_not_models_dev_names_and_the_table_reads_them() {
    let (files, _) = plan(&fixture_snapshot(), &before(&["anthropic"]), true, false).unwrap();
    let v: Value = serde_json::from_str(&files["anthropic"]).unwrap();
    let rows = v["models"].as_array().unwrap();
    let row = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id && r.get("price_from").is_none())
            .unwrap()
    };

    // tool_call -> capabilities.tools, an image modality -> capabilities.images, reasoning -> the row's.
    let haiku = row("claude-haiku-4-5");
    assert_eq!(
        haiku["capabilities"],
        serde_json::json!({"images": true, "tools": true})
    );
    assert_eq!(haiku["reasoning"], true);
    let newcomer = row("claude-newcomer-1");
    assert_eq!(
        newcomer["capabilities"],
        serde_json::json!({"images": false, "tools": true})
    );
    assert_eq!(newcomer["reasoning"], false);

    // A hand-set capability stays; the keys an older sync wrote under models.dev's names go.
    let sonnet = row("claude-sonnet-4-5");
    assert_eq!(
        sonnet["capabilities"],
        serde_json::json!({"images": true, "max_tools": 8, "tools": true})
    );
    for r in rows {
        for stale in ["tool_call", "reasoning", "input_modalities"] {
            assert!(
                r["capabilities"].get(stale).is_none(),
                "{} still has {stale}",
                r["id"]
            );
        }
    }
    // And the table that loads the file accepts every row as written.
    ModelTable::load(
        "anthropic",
        &files["anthropic"],
        &ProviderConfig::default(),
        &[],
        false,
    )
    .unwrap();
}
