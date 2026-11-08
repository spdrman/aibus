//! A1.3 stream-commit measurement on the `causl` backend: the same fixture as
//! `aibus-core/benches/commit_stream.rs`, included so the two cannot drift.

#[path = "../../aibus-core/benches/commit_stream.rs"]
#[allow(dead_code)]
mod stream;

fn main() {
    stream::run("causl", aibus_graph_causl::CauslGraph::new);
}
