//! #274: a client of `aibus mcp` drives the live session through the same `Msg` path a key takes.
//! `remote::msgs` is what Enter on the prompt line does with the same text, `remote::handle` steps
//! those messages (tagged `via: "mcp"`), and the screen says who sent them: an `mcp` badge on the
//! session log row and the client's name on the status bar while it is connected. A permission
//! prompt stays the keyboard's unless the session opted in, and `/login` with a key stays refused.

use aibus_core::remote::{Ask, Remote};
use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::state::{run_key, PERMISSIONS_OPEN, RUNS_ORDER};
use aibus_core::{Command, Msg, RunId};
use aibus_graph::{Graph, Value};
use aibus_testkit::tui::run_id;
use aibus_tui::input::Context;
use aibus_tui::{remote, Theme};
use crossterm::event::KeyCode as K;
use rig::*;
use serde_json::Value as Json;

const KEY: &str = "sk-test-REMOTE-0123456789abcdef";
const SIZE: (u16, u16) = (120, 40);

fn rig() -> Rig {
    Rig::new(session(), SIZE, Theme::recording())
}

/// What the loop does with one remote event: hand it to `remote::handle` with a plain step, draw,
/// and give back the answer a call got.
fn deliver(r: &mut Rig, event: Remote, permission: bool) {
    remote::handle(&mut r.app, &mut r.ui, event, permission, &mut |app, m| {
        app.step(m)
    })
    .unwrap();
    r.settle();
}

fn call(r: &mut Rig, ask: Ask, permission: bool) -> Result<Json, String> {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let event = Remote::Call {
        client: "editor".into(),
        ask,
        reply: tx,
    };
    deliver(r, event, permission);
    rx.try_recv().expect("every call is answered at once")
}

fn send(text: &str) -> Ask {
    Ask::Send { text: text.into() }
}

fn command(line: &str) -> Ask {
    Ask::Command { line: line.into() }
}

fn runs(r: &Rig) -> Vec<String> {
    as_list(r.app.graph.read_key(RUNS_ORDER).unwrap())
        .iter()
        .filter_map(as_str)
        .map(str::to_owned)
        .collect()
}

fn meta(r: &Rig, run: &str, field: &str) -> Option<Value> {
    r.app
        .graph
        .read_key(&run_key(run, "meta"))
        .and_then(|m| get(m, field))
        .cloned()
}

fn tagged_rows(r: &Rig) -> Vec<String> {
    r.app
        .store
        .log_rows(200)
        .iter()
        .filter(|row| get(row, "via").and_then(as_str) == Some("mcp"))
        .filter_map(|row| get(row, "text").and_then(as_str).map(str::to_owned))
        .collect()
}

#[test]
fn a_client_line_takes_the_same_msg_path_as_a_typed_one() {
    for line in [
        "fix the login bug",
        "/model haiku",
        "@agent-search find the call sites",
    ] {
        let mut typed = rig();
        typed.type_text(line);
        let by_hand = typed.key(K::Enter);
        assert!(!by_hand.is_empty(), "{line}");

        let mut sent = rig();
        let ask = if line.starts_with('/') {
            command(line)
        } else {
            send(line)
        };
        let by_client = remote::msgs(&ask, &Context::read(&sent.app.graph), false).unwrap();
        assert_eq!(names(&by_hand), names(&by_client), "{line}");

        // Stepped by `handle`, the session lands where the typed one did, plus the tag.
        call(&mut sent, ask, false).unwrap();
        assert_eq!(runs(&typed), runs(&sent), "{line}");
        for run in runs(&typed) {
            for field in ["agent", "prompt", "background"] {
                assert_eq!(
                    meta(&typed, &run, field),
                    meta(&sent, &run, field),
                    "{line}: {field}"
                );
            }
        }
        assert!(!tagged_rows(&sent).is_empty(), "{line}: no tagged row");
        assert!(
            tagged_rows(&typed).is_empty(),
            "{line}: a typed line is not tagged"
        );
    }
}

#[test]
fn a_run_a_client_starts_carries_the_tag_in_its_meta() {
    let mut r = rig();
    call(&mut r, send("@agent-search find the call sites"), false).unwrap();
    let new = runs(&r).last().cloned().unwrap();
    assert_eq!(meta(&r, &new, "via").as_ref().and_then(as_str), Some("mcp"));
    assert_eq!(
        meta(&r, &new, "client").as_ref().and_then(as_str),
        Some("editor")
    );
    // The lead the user started is untouched.
    assert_eq!(meta(&r, &runs(&r)[0], "via"), None);
}

#[test]
fn the_screen_shows_the_badge_and_who_is_connected() {
    for (name, theme) in [("recording", Theme::recording()), ("light", Theme::light())] {
        let mut r = Rig::new(session(), SIZE, theme);
        deliver(
            &mut r,
            Remote::Connected {
                client: "editor".into(),
            },
            false,
        );
        assert!(
            r.text().contains("mcp editor"),
            "{name}: the status bar names the client:\n{}",
            r.text()
        );
        call(&mut r, send("fix the login bug"), false).unwrap();
        let log = r.ui.drawn.areas.log;
        let status = r.ui.drawn.areas.status;
        assert_crop(&r.frame(), log, &format!("remote_274_log_{name}"));
        assert_crop(&r.frame(), status, &format!("remote_274_status_{name}"));

        deliver(
            &mut r,
            Remote::Disconnected {
                client: "editor".into(),
            },
            false,
        );
        assert!(
            !r.text().contains("mcp editor"),
            "{name}: gone from the status bar:\n{}",
            r.text()
        );
        let rows = tagged_rows(&r);
        assert!(rows.iter().any(|t| t.contains("connected")), "{rows:?}");
        assert!(rows.iter().any(|t| t.contains("disconnected")), "{rows:?}");
    }
}

#[test]
fn a_client_name_cannot_put_control_characters_on_screen() {
    let mut r = rig();
    deliver(
        &mut r,
        Remote::Connected {
            client: "ed\u{1b}[31mitor\u{7}\n".into(),
        },
        false,
    );
    call(&mut r, send("hello"), false).unwrap();
    let text = r.text();
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text}"
    );
    for row in r.app.store.log_rows(200) {
        let client = get(&row, "client").and_then(as_str).unwrap_or("");
        assert!(client.chars().all(|c| !c.is_control()), "{client:?}");
    }
}

#[test]
fn a_permission_prompt_stays_the_keyboards_unless_the_session_opted_in() {
    let mut r = rig();
    let id = ask(&mut r.app, 1, "lead", 1, "cargo test");
    r.settle();
    let answer = Ask::Permission {
        id: id.to_string(),
        decision: "allow".into(),
    };
    let why = call(&mut r, answer.clone(), false).unwrap_err();
    assert!(why.contains("--control-permission"), "{why}");
    let open = as_list(r.app.graph.read_key(PERMISSIONS_OPEN).unwrap()).len();
    assert_eq!(open, 1, "the prompt is still open");

    // With the opt-in the answer is the one `y` gives, tagged.
    let msgs = remote::msgs(&answer, &Context::read(&r.app.graph), true).unwrap();
    assert_eq!(
        names(&msgs),
        names(&[Msg::Command(Command::Permission {
            id,
            decision: aibus_core::Decision::Allow
        })])
    );
    call(&mut r, answer, true).unwrap();
    let open = as_list(r.app.graph.read_key(PERMISSIONS_OPEN).unwrap()).len();
    assert_eq!(open, 0, "answered");
    assert!(
        tagged_rows(&r).iter().any(|t| t.contains("permission")),
        "{:?}",
        tagged_rows(&r)
    );

    // An id that is not open, and a decision that is not one of the three, are refused.
    let none = Ask::Permission {
        id: aibus_core::PermissionId::new().to_string(),
        decision: "allow".into(),
    };
    assert!(call(&mut r, none, true).is_err());
    let id = ask(&mut r.app, 1, "lead", 2, "rm -rf target");
    let odd = Ask::Permission {
        id: id.to_string(),
        decision: "maybe".into(),
    };
    assert!(call(&mut r, odd, true).is_err());
}

#[test]
fn login_with_a_key_stays_refused_and_login_without_one_is_for_the_keyboard() {
    let mut r = rig();
    let why = call(&mut r, command(&format!("/login anthropic {KEY}")), false).unwrap_err();
    assert!(why.contains("[redacted]"), "{why}");
    assert!(!why.contains(KEY) && !why.contains("sk-test"), "{why}");
    let why = call(&mut r, command("/login anthropic"), false).unwrap_err();
    assert!(why.contains("keyboard"), "{why}");
    assert!(
        r.jobs.is_empty() && modal(&r).is_none(),
        "no login flow started"
    );
    assert!(!r.text().contains(KEY));
    for row in r.app.store.log_rows(200) {
        assert!(!format!("{row:?}").contains(KEY), "{row:?}");
    }
    // The refusals are on screen, tagged.
    assert!(
        tagged_rows(&r).iter().any(|t| t.contains("refused")),
        "{:?}",
        tagged_rows(&r)
    );
}

#[test]
fn a_client_can_never_end_the_session() {
    for line in ["/quit", "/exit", "/q", "  /quit  ", "\t/q"] {
        let mut r = rig();
        // A modal is open: a quit would close it first, and it has to stay.
        let id = ask(&mut r.app, 1, "lead", 1, "cargo test");
        r.settle();
        let why = call(&mut r, command(line), false).unwrap_err();
        let word = line.trim();
        assert!(
            why.contains(word) && why.contains("keyboard"),
            "{line:?}: {why}"
        );
        assert!(!why.contains('\n'), "one line: {why}");
        assert!(!r.app.should_quit(), "{line:?}: the session ended");
        assert_eq!(
            modal(&r).as_deref(),
            Some("permission"),
            "{line:?}: the open modal was closed"
        );
        let open = as_list(r.app.graph.read_key(PERMISSIONS_OPEN).unwrap()).len();
        assert_eq!(open, 1, "{line:?}");
        let refused: Vec<String> = tagged_rows(&r)
            .into_iter()
            .filter(|t| t.contains("refused"))
            .collect();
        assert!(
            refused.iter().any(|t| t.contains(word)),
            "{line:?}: {refused:?}"
        );
        // And the session still takes the next line.
        call(&mut r, send("still here"), false).unwrap();
        assert!(!r.app.should_quit());
        let _ = id;
    }
}

#[test]
fn send_takes_no_slash_line_and_command_takes_only_one() {
    let mut r = rig();
    let why = call(&mut r, send("/model haiku"), false).unwrap_err();
    assert!(why.contains("session_command"), "{why}");
    let why = call(&mut r, command("fix the bug"), false).unwrap_err();
    assert!(why.contains("slash"), "{why}");
    assert!(call(&mut r, send("   "), false).is_err());
    // A command the parser refuses is refused with the parser's own words.
    let why = call(&mut r, command("/nosuch"), false).unwrap_err();
    assert!(why.contains("no command /nosuch"), "{why}");
}

#[test]
fn cancel_names_a_run_the_way_a_target_does() {
    let mut r = rig();
    worker(&mut r.app, 2, "search", "maps the repo", 0.5, "running");
    r.settle();
    let msgs = remote::msgs(
        &Ask::Cancel {
            run: "search".into(),
        },
        &Context::read(&r.app.graph),
        false,
    )
    .unwrap();
    assert_eq!(
        names(&msgs),
        names(&[Msg::Command(Command::Cancel { run: run_id(2) })])
    );
    let by_id = remote::msgs(
        &Ask::Cancel {
            run: run_id(2).to_string(),
        },
        &Context::read(&r.app.graph),
        false,
    )
    .unwrap();
    assert_eq!(names(&by_id), names(&msgs));
    assert!(call(
        &mut r,
        Ask::Cancel {
            run: "nobody".into()
        },
        false
    )
    .is_err());
    call(
        &mut r,
        Ask::Cancel {
            run: "search".into(),
        },
        false,
    )
    .unwrap();
}

#[test]
fn status_and_transcript_are_what_the_screen_holds() {
    let mut r = rig();
    worker(&mut r.app, 2, "search", "maps the repo", 0.5, "running");
    text_stream(&mut r.app, 1, "I'll run the tests first.");
    let id = ask(&mut r.app, 1, "lead", 1, "cargo test");
    r.settle();
    deliver(
        &mut r,
        Remote::Connected {
            client: "editor".into(),
        },
        false,
    );

    let status = call(&mut r, Ask::Status, false).unwrap();
    let lead: RunId = run_id(1);
    assert_eq!(status["lead"]["run"], lead.to_string(), "{status}");
    assert_eq!(status["lead"]["agent"], "lead", "{status}");
    let cards = status["cards"].as_array().unwrap();
    assert!(
        cards
            .iter()
            .any(|c| c["agent"] == "search" && c["status"] == "running"),
        "{status}"
    );
    let asks = status["permissions"].as_array().unwrap();
    assert_eq!(asks.len(), 1, "{status}");
    assert_eq!(asks[0]["id"], id.to_string());
    assert_eq!(asks[0]["tool"], "bash");
    assert_eq!(status["client"], "editor");

    let read = call(&mut r, Ask::Transcript { run: None }, false).unwrap();
    assert_eq!(read["run"], lead.to_string());
    assert!(
        read["transcript"]
            .as_str()
            .unwrap()
            .contains("I'll run the tests first."),
        "{read}"
    );
    assert!(read["log"].as_array().is_some(), "{read}");
    let other = call(
        &mut r,
        Ask::Transcript {
            run: Some("search".into()),
        },
        false,
    )
    .unwrap();
    assert_eq!(other["run"], run_id(2).to_string());
    assert!(call(
        &mut r,
        Ask::Transcript {
            run: Some("nobody".into())
        },
        false
    )
    .is_err());
}

/// The parts of `tests/common` this file needs, kept here so the file stands on its own.
mod rig {
    use std::time::Instant;

    use aibus_core::{
        App, Command, Isolation, Msg, PermissionId, PermissionRequest, Role, ToolCallId,
    };
    use aibus_graph::mini::MiniGraph;
    use aibus_graph::{Graph, Value};
    use aibus_testkit::tui::{frame_of, poke, run_id, start, Frame, NOW};
    use aibus_tui::app::key_from_crossterm;
    use aibus_tui::input::{Context, Input, Job, Out};
    use aibus_tui::{Theme, Ui};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    pub struct Rig {
        pub app: App<MiniGraph>,
        pub term: Terminal<TestBackend>,
        pub ui: Ui,
        pub input: Input,
        pub now: Instant,
        pub jobs: Vec<Job>,
    }

    impl Rig {
        pub fn new(mut app: App<MiniGraph>, size: (u16, u16), theme: Theme) -> Self {
            app.step(Msg::Resize {
                cols: size.0,
                rows: size.1,
            })
            .unwrap();
            let mut rig = Self {
                app,
                term: Terminal::new(TestBackend::new(size.0, size.1)).unwrap(),
                ui: Ui::new(theme),
                input: Input::default(),
                now: Instant::now(),
                jobs: Vec::new(),
            };
            rig.settle();
            rig
        }

        /// What the loop does after every step: tell the UI about the commit, draw.
        pub fn settle(&mut self) {
            self.input.settle(self.now);
            aibus_tui::app::sync(&mut self.ui, &self.input);
            aibus_tui::view::ensure_lines(&mut self.app.graph);
            self.ui.note_commit(&self.app.graph);
            self.ui.draw(&mut self.term, &self.app.graph).unwrap();
        }

        fn apply(&mut self, out: Out) -> Vec<Msg> {
            for m in out.msgs.iter().cloned() {
                self.app.step(m).unwrap();
            }
            self.jobs.extend(out.jobs);
            self.settle();
            out.msgs
        }

        pub fn key(&mut self, code: KeyCode) -> Vec<Msg> {
            let mods = match code {
                KeyCode::Char(c) if c.is_uppercase() => KeyModifiers::SHIFT,
                _ => KeyModifiers::NONE,
            };
            let ev = key_from_crossterm(KeyEvent::new(code, mods)).unwrap();
            let cx = Context::read(&self.app.graph);
            let out = self
                .input
                .key(ev, &cx, &self.ui.keys, &self.ui.drawn, &mut self.ui.prompt);
            self.apply(out)
        }

        pub fn type_text(&mut self, text: &str) {
            for c in text.chars() {
                self.key(KeyCode::Char(c));
            }
        }

        pub fn frame(&self) -> Frame {
            frame_of(self.term.backend().buffer())
        }

        pub fn text(&self) -> String {
            self.frame().text
        }
    }

    /// `Msg`s as their Debug text, which is how they are compared.
    pub fn names(msgs: &[Msg]) -> Vec<String> {
        msgs.iter().map(|m| format!("{m:?}")).collect()
    }

    /// A session with the views registered and a running lead.
    pub fn session() -> App<MiniGraph> {
        let mut app = App::new(MiniGraph::new(), || NOW);
        let keys = aibus_tui::register(&mut app.graph, app.store.clone());
        app.keys.extend(keys);
        app.set_session_view(
            Some("--model sonnet"),
            (Some("opus 5.5"), true),
            (Some("jev"), true),
        )
        .unwrap();
        app.step(Msg::SubmitPrompt {
            text: "edits + runs tests".into(),
        })
        .unwrap();
        start(&mut app, 1, "lead", Role::Lead, "Sonnet 5.5");
        app
    }

    /// Worker `seq` (2 and up) with its task, progress and status word.
    pub fn worker(
        app: &mut App<MiniGraph>,
        seq: u128,
        agent: &str,
        task: &str,
        progress: f64,
        status: &str,
    ) {
        app.step(Msg::Command(Command::Delegate {
            agent: agent.parse().unwrap(),
            prompt: task.into(),
            background: false,
            isolation: Isolation::None,
            parent: None,
            run: None,
            model: None,
            account: None,
        }))
        .unwrap();
        start(app, seq, agent, Role::Worker, "Haiku 5.5");
        poke(
            app,
            &format!("model:run:{}:progress", run_id(seq)),
            Value::F64(progress),
        );
        poke(
            app,
            &format!("model:run:{}:status", run_id(seq)),
            Value::Str(status.into()),
        );
    }

    pub fn text_stream(app: &mut App<MiniGraph>, seq: u128, text: &str) {
        let chunks = vec![aibus_provider::StreamChunk::TextDelta(text.into())];
        app.step(Msg::Stream {
            run: run_id(seq),
            chunks,
        })
        .unwrap();
    }

    pub fn ask(
        app: &mut App<MiniGraph>,
        seq: u128,
        agent: &str,
        n: u128,
        summary: &str,
    ) -> PermissionId {
        let id = PermissionId::new();
        let request = PermissionRequest {
            id,
            agent: agent.parse().unwrap(),
            run: run_id(seq),
            call: ToolCallId(ulid::Ulid::from_parts(NOW, n)),
            tool: "bash".into(),
            summary: summary.into(),
        };
        app.step(Msg::PermissionNeeded { request }).unwrap();
        id
    }

    pub fn modal(r: &Rig) -> Option<String> {
        r.app
            .graph
            .read_key("ctrl:modal")
            .and_then(aibus_core::state::value::as_str)
            .map(str::to_owned)
    }

    /// Compares the part of `frame` inside `area` with `tests/snapshots/<name>.txt` and its style
    /// runs with `<name>.styles.txt`.
    pub fn assert_crop(frame: &Frame, area: Rect, name: &str) {
        let (x0, x1) = (usize::from(area.x), usize::from(area.right()));
        let rows = area.y..area.bottom();
        let text: String = frame
            .text
            .lines()
            .enumerate()
            .filter(|(y, _)| rows.contains(&(*y as u16)))
            .map(|(_, l)| {
                format!(
                    "{}\n",
                    l.chars()
                        .skip(x0)
                        .take(x1 - x0)
                        .collect::<String>()
                        .trim_end()
                )
            })
            .collect();
        let mut styles = String::new();
        for line in frame.styles.lines() {
            let (row, rest) = line.split_at(2);
            let y: u16 = row.parse().unwrap();
            if !rows.contains(&y) {
                continue;
            }
            let (span, style) = rest.trim_start().split_once(' ').unwrap();
            let (a, b) = span.split_once("..").unwrap();
            let (a, b) = (
                a.parse::<usize>().unwrap().max(x0),
                b.parse::<usize>().unwrap().min(x1),
            );
            if a < b {
                styles.push_str(&format!(
                    "{:02} {:02}..{:02} {style}\n",
                    y - area.y,
                    a - x0,
                    b - x0
                ));
            }
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots");
        aibus_testkit::tui::check_snapshot(dir, name, ".txt", &text);
        aibus_testkit::tui::check_snapshot(dir, name, ".styles.txt", &styles);
    }
}
