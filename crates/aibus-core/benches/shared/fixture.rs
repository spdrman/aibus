//! The reference session of 03 section 11 and 10 section 9: eight runs, 100,000 transcript
//! lines (about 7 MB) on the first, 500 lines on each of the others, and 2,000 log rows. It is
//! deterministic, and generic over the backend so `aibus-graph-causl` can include this file.
#![allow(dead_code)]

use aibus_core::*;
use aibus_graph::Graph;
use aibus_provider::StreamChunk;

pub fn line(i: usize) -> String {
    format!("line {i:06} the quick brown fox jumps over the lazy dog and keeps going a while\n")
}

pub fn stream(run: RunId, text: String) -> Msg {
    Msg::Stream {
        run,
        chunks: vec![
            StreamChunk::TextDelta(text),
            StreamChunk::Usage(Usage {
                output_tokens: 3,
                ..Usage::default()
            }),
        ],
    }
}

pub fn big_session<G: Graph>(mk: fn() -> G) -> (App<G>, Vec<RunId>) {
    let mut app = App::new(mk(), || 1_700_000_000_000);
    let runs: Vec<RunId> = (0..8).map(|_| RunId::new()).collect();
    for r in &runs {
        app.step(Msg::RunStarted {
            run: *r,
            agent: "worker".parse().unwrap(),
            role: Role::Worker,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("a".into()),
        })
        .unwrap();
    }
    // 100k lines into run 0, in 4 KiB frames.
    let mut chunk = String::new();
    for i in 0..100_000 {
        chunk.push_str(&line(i));
        if chunk.len() >= 4096 {
            app.step(stream(runs[0], std::mem::take(&mut chunk)))
                .unwrap();
        }
    }
    app.step(stream(runs[0], chunk)).unwrap();
    for r in &runs[1..] {
        app.step(stream(*r, line(0).repeat(500))).unwrap();
    }
    for i in 0..2000 {
        app.step(Msg::IoError {
            context: format!("ctx {i}"),
            error: "e".into(),
        })
        .unwrap();
    }
    (app, runs)
}
