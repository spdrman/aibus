//! The stream-commit gate on the `mini` backend; the measurement is in `shared/stream.rs`.

use aibus_graph::mini::MiniGraph;

#[path = "shared/fixture.rs"]
mod fixture;
#[path = "../../../benches/gate.rs"]
mod gate;
#[path = "shared/stream.rs"]
mod stream;

fn main() {
    stream::run("mini", MiniGraph::new);
}
