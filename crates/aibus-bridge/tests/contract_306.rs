//! #306: the TypeScript in `bindings/` is generated from the Rust DTOs, and `ts/bridge.ts` (hand
//! written) imports only types that exist there. Run with `AIBUS_BLESS=1` to regenerate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aibus_bridge::{Request, Response};
use ts_rs::TS;

fn read_dir(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".ts") {
            out.insert(name, fs::read_to_string(e.path()).unwrap());
        }
    }
    out
}

fn bindings() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings")
}

#[test]
fn generated_bindings_match_the_checked_in_files_306() {
    let tmp = tempfile::tempdir().unwrap();
    Request::export_all_to(tmp.path()).unwrap();
    Response::export_all_to(tmp.path()).unwrap();
    let fresh = read_dir(tmp.path());
    assert!(fresh.contains_key("Request.ts") && fresh.contains_key("EventBatch.ts"));
    if std::env::var_os("AIBUS_BLESS").is_some() {
        fs::create_dir_all(bindings()).unwrap();
        for (name, text) in &fresh {
            fs::write(bindings().join(name), text).unwrap();
        }
    }
    assert_eq!(
        read_dir(&bindings()),
        fresh,
        "the TS bindings drifted from the Rust DTOs: run `AIBUS_BLESS=1 cargo test -p aibus-bridge --test contract_306` and commit bindings/"
    );
}

#[test]
fn the_hand_written_module_imports_only_generated_types_306() {
    let src =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("ts/bridge.ts")).unwrap();
    let have = read_dir(&bindings());
    let mut n = 0;
    for line in src.lines().filter(|l| l.starts_with("import type")) {
        let from = line.split("from \"").nth(1).unwrap();
        let file = from.trim_end_matches("\";").rsplit('/').next().unwrap();
        assert!(have.contains_key(&format!("{file}.ts")), "{line}");
        n += 1;
    }
    assert!(n >= 5, "bridge.ts should import the DTOs it uses");
}
