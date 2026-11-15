//! #274: what a client of `aibus mcp` sends into a live session carries a tag, `via: "mcp"` and
//! the client's name, in the session log, the run's meta, `model:last_msg`, the commit log and
//! the run's transcript. What the user typed carries none, and both go through the same reducer
//! arms.

use std::path::Path;
use std::time::SystemTime;

use aibus_core::config::Persist;
use aibus_core::persist::commits;
use aibus_core::persist::session::SessionWriter;
use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::state::{run_key, LAST_MSG, RUNS_ORDER};
use aibus_core::{AccountName, App, ClientEvent, ModelId, Msg, Redactor, Role, RunId};
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};
use aibus_testkit::xdg_temp;

fn app() -> App<MiniGraph> {
    let mut app = App::new(MiniGraph::new(), || 5_000);
    app.record_commits();
    app
}

fn via(m: Msg) -> Msg {
    Msg::Via {
        client: "editor".into(),
        msg: Box::new(m),
    }
}

fn prompt(text: &str) -> Msg {
    Msg::SubmitPrompt { text: text.into() }
}

fn lead_of(a: &App<MiniGraph>) -> RunId {
    let order = as_list(a.graph.read_key(RUNS_ORDER).unwrap());
    as_str(&order[0]).unwrap().parse().unwrap()
}

fn started(run: RunId) -> Msg {
    Msg::RunStarted {
        run,
        agent: "lead".parse().unwrap(),
        role: Role::Lead,
        parent: None,
        model: ModelId("claude-sonnet-4-5".into()),
        account: AccountName("work".into()),
    }
}

fn tag(v: &Value) -> (Option<&str>, Option<&str>) {
    (
        get(v, "via").and_then(as_str),
        get(v, "client").and_then(as_str),
    )
}

#[test]
fn a_client_prompt_is_tagged_in_the_log_the_run_meta_and_the_last_msg() {
    let mut a = app();
    a.step(via(prompt("map the repo"))).unwrap();
    let run = lead_of(&a);
    let meta = a.graph.read_key(&run_key(run, "meta")).unwrap().clone();
    assert_eq!(tag(&meta), (Some("mcp"), Some("editor")), "{meta:?}");
    // The run keeps the tag once it has started.
    a.step(started(run)).unwrap();
    let meta = a.graph.read_key(&run_key(run, "meta")).unwrap().clone();
    assert_eq!(tag(&meta), (Some("mcp"), Some("editor")), "{meta:?}");

    let last = a.graph.read_key(LAST_MSG).unwrap().clone();
    assert_eq!(get(&last, "msg").and_then(as_str), Some("RunStarted"));
    a.step(via(prompt("and the tests"))).unwrap();
    let last = a.graph.read_key(LAST_MSG).unwrap().clone();
    // The intent is the message the keyboard would have sent; the tag sits next to it.
    assert_eq!(get(&last, "msg").and_then(as_str), Some("SubmitPrompt"));
    assert_eq!(tag(&last), (Some("mcp"), Some("editor")), "{last:?}");

    let rows = a.store.log_rows(100);
    let tagged: Vec<&Value> = rows.iter().filter(|r| tag(r).0 == Some("mcp")).collect();
    assert!(
        tagged.len() >= 2,
        "a tagged row per client message: {rows:?}"
    );
    assert!(tagged.iter().all(|r| tag(r).1 == Some("editor")));
}

#[test]
fn a_typed_prompt_carries_no_tag_and_takes_the_same_arms() {
    let mut typed = app();
    typed.step(prompt("map the repo")).unwrap();
    let mut sent = app();
    sent.step(via(prompt("map the repo"))).unwrap();

    let (t, s) = (lead_of(&typed), lead_of(&sent));
    let meta_t = typed.graph.read_key(&run_key(t, "meta")).unwrap().clone();
    let meta_s = sent.graph.read_key(&run_key(s, "meta")).unwrap().clone();
    assert_eq!(tag(&meta_t), (None, None));
    // Everything but the tag is the same run.
    for field in [
        "agent",
        "role",
        "prompt",
        "label",
        "background",
        "isolation",
    ] {
        assert_eq!(get(&meta_t, field), get(&meta_s, field), "{field}");
    }
    let last = typed.graph.read_key(LAST_MSG).unwrap().clone();
    assert_eq!(tag(&last), (None, None));
    assert!(typed.store.log_rows(100).iter().all(|r| tag(r).0.is_none()));
}

#[test]
fn connect_disconnect_and_a_refusal_are_tagged_rows_of_the_session_log() {
    let mut a = app();
    for event in [
        ClientEvent::Connected,
        ClientEvent::Refused("/login is only for the keyboard".into()),
        ClientEvent::Disconnected,
    ] {
        a.step(Msg::Client {
            name: "editor".into(),
            event,
        })
        .unwrap();
    }
    let rows = a.store.log_rows(100);
    let text: Vec<&str> = rows
        .iter()
        .filter(|r| tag(r) == (Some("mcp"), Some("editor")))
        .filter_map(|r| get(r, "text").and_then(as_str))
        .collect();
    assert_eq!(text.len(), 3, "{rows:?}");
    assert!(text[0].contains("connected"), "{text:?}");
    assert!(
        text[1].contains("refused") && text[1].contains("/login"),
        "{text:?}"
    );
    assert!(text[2].contains("disconnected"), "{text:?}");
}

fn drive(w: &mut SessionWriter, app: &mut App<MiniGraph>, msgs: impl IntoIterator<Item = Msg>) {
    for msg in msgs {
        w.msg(&msg, SystemTime::now()).unwrap();
        app.step(msg).unwrap();
        w.after_step(app).unwrap();
    }
}

fn transcript(dir: &Path) -> Vec<serde_json::Value> {
    let runs: Vec<_> = std::fs::read_dir(dir.join("runs"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(runs.len(), 1);
    std::fs::read_to_string(runs[0].path().join("transcript.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn the_transcript_and_the_commit_log_say_which_lines_a_client_sent() {
    let dir = xdg_temp();
    let mut a = app();
    let mut w =
        SessionWriter::open(dir.path(), Redactor::new(), &Persist::default(), 64 * 1024).unwrap();
    drive(&mut w, &mut a, [via(prompt("map the repo"))]);
    let run = lead_of(&a);
    drive(&mut w, &mut a, [started(run)]);
    // What the loop does after `RunStarted`: the task, with the tag the run's meta carries.
    w.prompt(run, SystemTime::now(), "map the repo", Some("editor"))
        .unwrap();
    // A message to the running lead, from the client and then from the keyboard.
    drive(
        &mut w,
        &mut a,
        [via(prompt("and the tests")), prompt("typed by me")],
    );
    w.finish(&mut a).unwrap();

    let users: Vec<serde_json::Value> = transcript(dir.path())
        .into_iter()
        .filter(|l| l["type"] == "message" && l["role"] == "user")
        .collect();
    let said: Vec<(&str, Option<&str>, Option<&str>)> = users
        .iter()
        .map(|l| {
            (
                l["blocks"][0]["text"].as_str().unwrap(),
                l["via"].as_str(),
                l["client"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            ("map the repo", Some("mcp"), Some("editor")),
            ("and the tests", Some("mcp"), Some("editor")),
            ("typed by me", None, None),
        ]
    );

    let records = commits::read_all(dir.path()).unwrap();
    let last_msgs: Vec<&Value> = records
        .iter()
        .filter(|r| r.intent == "SubmitPrompt")
        .flat_map(|r| r.changed.iter())
        .filter(|(k, _)| k.as_str() == LAST_MSG)
        .map(|(_, v)| v)
        .collect();
    let tags: Vec<_> = last_msgs.iter().map(|v| tag(v).0).collect();
    assert_eq!(tags, [Some("mcp"), Some("mcp"), None], "{last_msgs:?}");
}
