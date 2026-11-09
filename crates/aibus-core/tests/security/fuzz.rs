//! 10 section 1 (Fuzz): the four targets' checks run against every committed corpus file and
//! every crash fixture, on every push. A crash a nightly run finds is saved under
//! `fixtures/fuzz/<target>/` and the next run of this test reproduces it.

use std::path::{Path, PathBuf};

#[path = "../../../../fuzz/checks.rs"]
mod checks;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under `dir`, sorted, or none when the directory is not there.
fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut all: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    all.sort();
    all
}

/// Runs `check` on every input of `target` from `fuzz/corpus` and `fixtures/fuzz`, and says which
/// file broke it.
fn replay(target: &str, check: fn(&[u8])) -> usize {
    let mut n = 0;
    for dir in [root().join("fuzz/corpus").join(target), root().join("fixtures/fuzz").join(target)] {
        for file in files(&dir) {
            let data = std::fs::read(&file).unwrap();
            let r = std::panic::catch_unwind(|| check(&data));
            assert!(r.is_ok(), "{target}: {} breaks an invariant", file.display());
            n += 1;
        }
    }
    n
}

#[test]
fn agent_def_corpus_and_crashes_hold() {
    assert!(replay("agent_def", checks::agent_def) > 0, "no corpus for agent_def");
}

#[test]
fn handoff_corpus_and_crashes_hold() {
    assert!(replay("handoff", checks::handoff) > 0, "no corpus for handoff");
}

#[test]
fn redactor_corpus_and_crashes_hold() {
    assert!(replay("redactor", checks::redactor) > 0, "no corpus for redactor");
}

#[test]
fn tool_pattern_corpus_and_crashes_hold() {
    assert!(replay("tool_pattern", checks::tool_pattern) > 0, "no corpus for tool_pattern");
}

/// The repository's own fixtures are seeds too, so a change to a parser that breaks one of them
/// shows up here even before a fuzz run.
#[test]
fn the_repository_fixtures_pass_the_checks() {
    for f in files(&root().join("fixtures/agents/valid")).into_iter().chain(files(&root().join("fixtures/agents/invalid"))) {
        checks::agent_def(&std::fs::read(f).unwrap());
    }
    for f in files(&root().join("fixtures/handoffs")) {
        checks::handoff(&std::fs::read(f).unwrap());
    }
}
