//! #185: the three model tables 0.1.0 shipped (`fixtures/models/0.1.0-*.json`, copied byte for
//! byte from the `v0.1.0` tag) are exactly the shape of a 0.1.0 user's
//! `~/.config/aibus/providers/<provider>/models.json`. Each one must still load as an override
//! over today's built-in table, win on what it says, and leave alone what it never said.

use std::path::{Path, PathBuf};

use aibus_provider::catalog::table;
use aibus_provider::models::ModelTable;
use aibus_provider::{ModelId, ProviderConfig};
use serde_json::Value;

fn fixture(provider: &str) -> (PathBuf, Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/models/0.1.0-{provider}.json"));
    let json = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap();
    (path, json)
}

#[test]
fn every_0_1_0_table_loads_as_an_override_and_keeps_what_it_never_said() {
    for provider in ["anthropic", "openai", "kimi"] {
        let (path, json) = fixture(provider);
        let rows = json["models"].as_array().unwrap();
        assert!(rows.len() >= 4, "{provider}: the corpus lost rows");
        assert!(rows.iter().all(|r| r.get("capabilities").is_none()), "{provider}: a 0.1.0 file has no capabilities");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(provider)).unwrap();
        std::fs::copy(&path, dir.path().join(provider).join("models.json")).unwrap();

        let builtin = table(provider).unwrap();
        let user = ProviderConfig { overrides_dir: Some(dir.path().to_owned()), ..ProviderConfig::default() };
        let before = ModelTable::load(provider, builtin, &ProviderConfig::default(), &[], false).unwrap();
        let after = ModelTable::load(provider, builtin, &user, &[], false).unwrap_or_else(|e| panic!("{provider}: {e}"));

        for row in rows {
            let id = ModelId(row["id"].as_str().unwrap().into());
            let at = format!("{provider} {}", id.0);
            let e = after.entry(&id).unwrap_or_else(|| panic!("{at} is gone"));
            // What the file says wins.
            assert_eq!(u64::from(e.info.context_window), row["context_window"].as_u64().unwrap(), "{at}");
            assert_eq!(u64::from(e.info.max_output), row["max_output"].as_u64().unwrap(), "{at}");
            assert_eq!(e.info.family.as_str(), row["family"].as_str().unwrap(), "{at}");
            assert_eq!(e.info.deprecated, row["deprecated"].as_bool().unwrap_or(false), "{at}");
            match row.get("price") {
                Some(p) => {
                    let got = e.price.unwrap_or_else(|| panic!("{at}: the price is gone"));
                    assert_eq!(got.input_per_mtok, p["input_per_mtok"].as_f64().unwrap(), "{at}");
                    assert_eq!(got.output_per_mtok, p["output_per_mtok"].as_f64().unwrap(), "{at}");
                }
                None => assert!(e.price.is_none(), "{at}: a price appeared from nowhere"),
            }
            // What it never said stays as the built-in table has it.
            if let Some(b) = before.entry(&id) {
                assert_eq!((&e.capabilities, e.reasoning), (&b.capabilities, b.reasoning), "{at}");
                assert!(e.effort || !b.effort, "{at}: the effort flag was lost");
            }
        }
    }
}
