//! Stream-commit measurement on both backends in one run: the same fixture and gate as
//! `aibus-core/benches/commit_stream.rs`, included so the two cannot drift. `mini` goes first,
//! then `causl`, and a `side_by_side` line per case puts them next to each other.

use aibus_graph::mini::MiniGraph;

#[path = "../../aibus-core/benches/shared/fixture.rs"]
mod fixture;
#[path = "../../../benches/gate.rs"]
mod gate;
#[path = "../../aibus-core/benches/shared/stream.rs"]
mod stream;

fn main() {
    let (mini_open, mini_any) = stream::run("mini", MiniGraph::new);
    let (causl_open, causl_any) = stream::run("causl", aibus_graph_causl::CauslGraph::new);
    println!("backend: side by side (us, p50 / p99 / max)");
    for (case, m, c) in [
        ("open run", &mini_open, &causl_open),
        ("round-robin", &mini_any, &causl_any),
    ] {
        println!(
            "side_by_side {case}: mini {:.0} / {:.0} / {:.0}, causl {:.0} / {:.0} / {:.0}, causl is {:.1}x at p50 and {:.1}x at p99",
            m.p50,
            m.p99,
            m.max,
            c.p50,
            c.p99,
            c.max,
            c.p50 / m.p50,
            c.p99 / m.p99,
        );
    }
}
