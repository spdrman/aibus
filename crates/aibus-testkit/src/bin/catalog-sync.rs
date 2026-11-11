//! `catalog-sync --snapshot FILE [--catalog DIR] [--add-new] [--allow-price-removal] [--write]`
//!
//! Reads a saved copy of the models.dev `api.json` (plain, or `.zst`) and the files in `catalog/`,
//! and says what would change. It opens no socket and writes nothing unless `--write` is given;
//! the maintainer then reads `git diff` and commits. Prices move only through a reviewed PR.

use std::collections::BTreeMap;
use std::path::PathBuf;

use aibus_testkit::catalog_sync::plan;

fn main() {
    if let Err(e) = run() {
        eprintln!("catalog-sync: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut snapshot, mut dir) = (None, PathBuf::from("catalog"));
    let (mut add_new, mut allow, mut write) = (false, false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--snapshot" => snapshot = args.next().map(PathBuf::from),
            "--catalog" => dir = args.next().map(PathBuf::from).ok_or("--catalog needs a directory")?,
            "--add-new" => add_new = true,
            "--allow-price-removal" => allow = true,
            "--write" => write = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let snapshot = snapshot.ok_or("--snapshot FILE is required (save models.dev/api.json yourself; this tool never fetches)")?;
    let bytes = std::fs::read(&snapshot).map_err(|e| format!("{}: {e}", snapshot.display()))?;
    let bytes = if snapshot.extension().is_some_and(|e| e == "zst") {
        zstd::decode_all(&bytes[..]).map_err(|e| format!("{}: {e}", snapshot.display()))?
    } else {
        bytes
    };
    let snapshot = serde_json::from_slice(&bytes).map_err(|e| format!("snapshot: {e}"))?;

    let mut catalog = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|e| e == "json") {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            catalog.insert(stem, std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?);
        }
    }
    let (files, notes) = plan(&snapshot, &catalog, add_new, allow)?;
    notes.iter().for_each(|n| println!("{n}"));
    for (name, text) in &files {
        if write {
            std::fs::write(dir.join(format!("{name}.json")), text).map_err(|e| e.to_string())?;
        }
        println!("{}{}", if write { "wrote " } else { "would write " }, dir.join(format!("{name}.json")).display());
    }
    if !write {
        println!("dry run: nothing written (--write to apply, then read the diff)");
    }
    Ok(())
}
