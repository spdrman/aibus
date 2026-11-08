//! `MiniGraph` against the conformance suite, plus the checks that are about mini itself.

mod conformance;

use aibus_graph::mini::MiniGraph;

#[test]
fn mini_passes_the_conformance_suite() {
    conformance::suite(MiniGraph::new);
}

#[test]
fn mini_stays_under_1500_lines() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mini");
    let lines: usize = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            std::fs::read_to_string(e.unwrap().path())
                .unwrap()
                .lines()
                .count()
        })
        .sum();
    println!("src/mini: {lines} lines");
    assert!(lines < 1500, "src/mini is {lines} lines");
}
