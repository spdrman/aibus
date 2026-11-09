//! Sessions on disk (07 sections 2.1 and 2.2): the last snapshot plus the logged commits
//! rebuild the session, transcripts are redacted and capped, and `msgs.jsonl.zst` replays to the
//! recorded hash.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::config::Persist;
use aibus_core::persist::commits;
use aibus_core::persist::frames::read_lines;
use aibus_core::persist::session::{load, write_snapshot, SessionWriter, MSGS};
use aibus_core::replay::{msg_from_json, replay_hash};
use aibus_core::{AccountName, App, Msg, Redactor, RunId};
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_provider::StreamChunk;

fn fixture(file: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "../../tests/replay/explore-fake", file]
        .iter()
        .collect();
    std::fs::read_to_string(path).unwrap()
}

fn recorded() -> Vec<Msg> {
    fixture("msgs.jsonl")
        .lines()
        .map(|l| msg_from_json(&serde_json::from_str(l).unwrap()).unwrap())
        .collect()
}

fn app() -> App<MiniGraph> {
    let mut app = App::new(MiniGraph::new(), || 5_000);
    app.record_commits();
    app
}

fn writer(dir: &Path, redactor: Redactor, persist: &Persist) -> SessionWriter {
    SessionWriter::open(dir, redactor, persist, 64 * 1024).unwrap()
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn run_dir(dir: &Path) -> PathBuf {
    let runs: Vec<_> = std::fs::read_dir(dir.join("runs")).unwrap().flatten().collect();
    assert_eq!(runs.len(), 1);
    runs[0].path()
}

fn drive(w: &mut SessionWriter, app: &mut App<MiniGraph>, msgs: impl IntoIterator<Item = Msg>) {
    for msg in msgs {
        w.msg(&msg, SystemTime::now()).unwrap();
        app.step(msg).unwrap();
        w.after_step(app).unwrap();
    }
}

#[test]
fn the_last_snapshot_plus_the_logged_commits_rebuild_the_session() {
    let dir = xdg_temp();
    let mut a = app();
    let mut w = writer(dir.path(), Redactor::new(), &Persist::default());
    let resizes = (0..70).map(|i| Msg::Resize { cols: 80 + i, rows: 24 });
    drive(&mut w, &mut a, recorded().into_iter().chain(resizes));
    // A snapshot was taken when 64 messages had gone by; the rest lives only in the log.
    assert!(dir.path().join("graph.snapshot.json.zst").is_file());
    // The process "crashes" here: no shutdown snapshot, only what was flushed.
    w.flush().unwrap();

    let (snap, tail) = load(dir.path()).unwrap();
    assert!(!tail.is_empty(), "commits after the snapshot exist");
    assert!(tail.iter().all(|r| r.time > snap.now));
    let mut b = App::new(MiniGraph::new(), || 5_000);
    b.hydrate(snap, &tail).unwrap();
    assert_eq!(b.graph.snapshot().inputs, a.graph.snapshot().inputs);
}

#[test]
fn the_shutdown_snapshot_alone_is_the_whole_session() {
    let dir = xdg_temp();
    let mut a = app();
    let mut w = writer(dir.path(), Redactor::new(), &Persist::default());
    drive(&mut w, &mut a, recorded());
    w.finish(&mut a).unwrap();

    let (snap, tail) = load(dir.path()).unwrap();
    // `Persisted` itself is logged after the snapshot was taken.
    assert_eq!(tail.iter().map(|r| r.intent.as_str()).collect::<Vec<_>>(), ["Persisted"]);
    let mut b = App::new(MiniGraph::new(), || 5_000);
    b.hydrate(snap, &tail).unwrap();
    assert_eq!(b.graph.snapshot().inputs, a.graph.snapshot().inputs);
    assert_eq!(commits::read_all(dir.path()).unwrap().len(), a.graph.commit_log().len());
}

#[test]
fn every_recorded_msg_replays_from_msgs_jsonl_zst_to_the_recorded_hash() {
    let dir = xdg_temp();
    let mut a = app();
    let mut w = writer(dir.path(), Redactor::new(), &Persist::default());
    drive(&mut w, &mut a, recorded());
    w.finish(&mut a).unwrap();
    let text: String = read_lines(&dir.path().join(MSGS))
        .unwrap()
        .iter()
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(replay_hash(&text).unwrap(), fixture("snapshot.hash").trim());
}

#[test]
fn a_transcript_has_the_07_line_types_and_meta_json() {
    let dir = xdg_temp();
    let mut a = app();
    let mut w = writer(dir.path(), Redactor::new(), &Persist::default());
    let msgs = recorded();
    let Msg::RunStarted { run, .. } = &msgs[0] else { panic!() };
    let run = *run;
    drive(&mut w, &mut a, msgs[..1].to_vec());
    w.prompt(run, SystemTime::now(), "map this repo").unwrap();
    drive(&mut w, &mut a, msgs[1..].to_vec());
    w.finish(&mut a).unwrap();

    let rd = run_dir(dir.path());
    let got = lines(&rd.join("transcript.jsonl"));
    let kinds: Vec<_> = got
        .iter()
        .map(|l| match l["type"].as_str().unwrap() {
            "message" => format!("message:{}", l["role"].as_str().unwrap()),
            other => other.to_owned(),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "message:user",
            "usage",
            "message:assistant",
            "tool_result",
            "usage",
            "message:assistant",
            "end"
        ]
    );
    assert!(got.iter().all(|l| l["t"].as_str().unwrap().ends_with('Z')));
    let first = &got[2]["blocks"];
    assert_eq!(first[0]["type"], "text");
    assert_eq!(first[1]["type"], "tool_use");
    assert_eq!(first[1]["name"], "ls");
    assert_eq!(first[1]["input"]["path"], ".");
    assert_eq!(got[3]["bytes"], 14);
    assert_eq!(got[6]["status"], "completed");

    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(rd.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["agent"], "explore");
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["turns"], 2);
    assert_eq!(meta["input_tokens"], 400);
    assert!(meta["ended_at"].is_string());
}

#[test]
fn a_registered_secret_never_reaches_the_transcript_or_msgs() {
    let dir = xdg_temp();
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), "sk-test-SECRET-0123456789");
    let mut a = app();
    let mut w = writer(dir.path(), redactor, &Persist::default());
    let msgs = recorded();
    let Msg::RunStarted { run, .. } = &msgs[0] else { panic!() };
    let run = *run;
    drive(&mut w, &mut a, msgs[..1].to_vec());
    w.prompt(run, SystemTime::now(), "use sk-test-SECRET-0123456789 please").unwrap();
    drive(
        &mut w,
        &mut a,
        [Msg::Stream {
            run,
            chunks: vec![
                StreamChunk::TextDelta("key is sk-test-SECRET-0123456789".into()),
                StreamChunk::Done(aibus_provider::StopReason::EndTurn),
            ],
        }],
    );
    w.finish(&mut a).unwrap();
    let t = std::fs::read_to_string(run_dir(dir.path()).join("transcript.jsonl")).unwrap();
    assert!(t.contains("[redacted:work]"), "{t}");
    for entry in walk(dir.path()) {
        let bytes = std::fs::read(&entry).unwrap();
        let raw = String::from_utf8_lossy(&bytes);
        assert!(!raw.contains("SECRET-0123456789"), "{}", entry.display());
    }
    let decoded: String = read_lines(&dir.path().join(MSGS)).unwrap().join("\n");
    assert!(!decoded.contains("SECRET-0123456789"));
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

#[test]
fn a_result_over_the_cap_is_cut_and_the_whole_text_goes_to_a_side_file() {
    let dir = xdg_temp();
    let mut a = app();
    let mut w = writer(dir.path(), Redactor::new(), &Persist::default());
    let msgs = recorded();
    let Some(Msg::ToolCallFinished { run, call_id, duration, .. }) =
        msgs.iter().find(|m| matches!(m, Msg::ToolCallFinished { .. })).cloned()
    else {
        panic!("the fixture has a tool call")
    };
    let big = "x".repeat(70 * 1024);
    drive(&mut w, &mut a, msgs[..1].to_vec());
    drive(
        &mut w,
        &mut a,
        [Msg::ToolCallFinished {
            run,
            call_id,
            result: aibus_core::ToolResult { content: big.clone(), ..Default::default() },
            duration,
        }],
    );
    let rd = run_dir(dir.path());
    let got = lines(&rd.join("transcript.jsonl"));
    let r = &got[0];
    assert_eq!(r["type"], "tool_result");
    assert_eq!(r["truncated"], true);
    assert_eq!(r["bytes"], 70 * 1024);
    assert_eq!(r["content"].as_str().unwrap().len(), 64 * 1024);
    let rel = r["full"].as_str().unwrap();
    let id = rd.file_name().unwrap().to_str().unwrap();
    assert_eq!(rel, format!("runs/{id}/results/{}.txt", call_id.0));
    assert_eq!(std::fs::read_to_string(dir.path().join(rel)).unwrap(), big);
}

#[test]
fn thinking_is_stored_only_when_store_thinking_is_on() {
    let thinking_in = |on: bool| {
        let dir = xdg_temp();
        let mut a = app();
        let persist = Persist { store_thinking: on, ..Persist::default() };
        let mut w = writer(dir.path(), Redactor::new(), &persist);
        let msgs = recorded();
        let Msg::RunStarted { run, .. } = &msgs[0] else { panic!() };
        let run: RunId = *run;
        drive(&mut w, &mut a, msgs[..1].to_vec());
        drive(
            &mut w,
            &mut a,
            [Msg::Stream {
                run,
                chunks: vec![
                    StreamChunk::ThinkingDelta("hmm, secretly".into()),
                    StreamChunk::TextDelta("answer".into()),
                    StreamChunk::Done(aibus_provider::StopReason::EndTurn),
                ],
            }],
        );
        std::fs::read_to_string(run_dir(dir.path()).join("transcript.jsonl")).unwrap()
    };
    assert!(!thinking_in(false).contains("hmm, secretly"));
    assert!(thinking_in(true).contains("hmm, secretly"));
}

#[test]
fn a_snapshot_survives_a_write_and_a_read() {
    let dir = xdg_temp();
    let mut a = app();
    a.step(Msg::Resize { cols: 100, rows: 30 }).unwrap();
    let snap = a.graph.snapshot();
    write_snapshot(dir.path(), &snap).unwrap();
    assert_eq!(aibus_core::persist::session::read_snapshot(dir.path()).unwrap(), snap);
}
