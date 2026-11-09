//! Stream-commit measurement on the `causl` backend: the same fixture and gate as
//! `aibus-core/benches/commit_stream.rs`, included so the two cannot drift.

#[path = "../../aibus-core/benches/shared/fixture.rs"]
mod fixture;
#[path = "../../../benches/gate.rs"]
mod gate;
#[path = "../../aibus-core/benches/shared/stream.rs"]
mod stream;

fn main() {
    stream::run("causl", aibus_graph_causl::CauslGraph::new);
}
