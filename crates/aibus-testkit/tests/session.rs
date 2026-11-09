//! The testkit's own fixtures: the reference session and the temp XDG tree.

use aibus_core::replay::snapshot_hash;
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_testkit::{big_session, xdg_temp};

#[test]
fn big_session_is_deterministic_and_the_size_the_spec_names() {
    let (a, runs) = big_session(MiniGraph::new);
    let (b, _) = big_session(MiniGraph::new);
    assert_eq!(runs.len(), 8);
    assert_eq!(snapshot_hash(&a.graph), snapshot_hash(&b.graph));
    assert_eq!(a.graph.now(), b.graph.now());
    assert!(a.graph.now() > 2_000, "{}", a.graph.now());
}

#[test]
fn xdg_temp_resolves_all_four_directories_under_one_root() {
    let x = xdg_temp();
    for p in [&x.paths.config, &x.paths.data, &x.paths.state, &x.paths.cache] {
        assert!(p.is_dir() && p.starts_with(x.root()), "{}", p.display());
    }
}

#[test]
#[should_panic(expected = "a secret reached")]
fn a_planted_key_is_found_in_plain_and_compressed_files() {
    let mut x = xdg_temp();
    x.forbid("sk-planted-1234");
    std::fs::write(x.paths.state.join("clean.txt"), "nothing here").unwrap();
    x.assert_no_secret_on_disk();
    let packed = zstd::encode_all("a line with sk-planted-1234 in it".as_bytes(), 1).unwrap();
    std::fs::write(x.paths.state.join("msgs.jsonl.zst"), packed).unwrap();
    x.assert_no_secret_on_disk();
}
