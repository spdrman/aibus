//! The README demo: a recorded fake-provider session played into the real dashboard on a
//! virtual clock. Nothing here reads the wall clock, a tty, a config file or the network, so two
//! plays draw the same frames. `aibus demo` (hidden) is the only caller; `tools/record-demo.sh`
//! explains how the recording and the GIF are made.
//!
//! The messages come from `docs/media/demo/msgs.jsonl`, which `aibus run --record` wrote when the
//! real engine ran `script.ron` on the fake provider. This module adds only what a recording
//! cannot hold: the pace (text arrives a few characters per tick), the typed prompt, and two
//! mouse gestures. Everything goes through the same event layer, reducer and renderer as a live
//! session.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibus_core::state::value::as_str;
use aibus_core::state::ANIMATING;
use aibus_core::{App, Command, Isolation, KeyCode, KeyEvent, Msg, Progress, RunId, ToolCallId};
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};
use aibus_provider::StreamChunk;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::hit::HitId;
use crate::input::{Context, Input, Out};
use crate::{view, Theme, Ui};

pub const COLS: u16 = 98;
pub const ROWS: u16 = 40;
/// The demo is this long, and a frame is drawn every `FRAME_MS`.
pub const LENGTH_MS: u64 = 10_000;
pub const FRAME_MS: u64 = 100;
/// The messages `aibus run --record` wrote for `docs/media/demo/script.ron`.
pub const RECORDING: &str = include_str!("../../../docs/media/demo/msgs.jsonl");
/// What the user types at the start; `tools/record-demo.sh` passes the same text to the lead.
pub const PROMPT: &str = "harden the login flow and add tests";

/// The tick timer of the live loop (30 Hz) on the virtual clock.
const TICK_MS: u64 = 33;
/// The virtual wall clock starts here (ms), as the testkit's fixtures do.
const EPOCH_MS: u64 = 1_700_000_000_000;
const TYPE_FROM_MS: u64 = 300;
const TYPE_EVERY_MS: u64 = 30;
/// Streamed text arrives this many characters per tick.
const STREAM_CHARS: usize = 6;
/// The recorded messages are paced from Enter and squeezed (never stretched) to be done by here;
/// the pointer gestures follow.
const SESSION_END_MS: u64 = 6_600;

enum Step {
    Key(KeyCode),
    Msg(Box<Msg>),
    /// A pointer event on the middle of card `.0` as drawn at that moment.
    Card(usize, MouseEventKind),
}

fn gap(msg: &Msg) -> u64 {
    match msg {
        Msg::RunStarted { .. } => 200,
        Msg::ToolCallRequested { .. } => 120,
        Msg::ToolCallFinished { .. } => 160,
        Msg::RunFinished { .. } => 200,
        Msg::AdvisorSpoke { .. } => 400,
        Msg::RouterAnswered { .. } => 150,
        _ => 40,
    }
}

/// The schedule: every step with the virtual time it happens at, in order.
fn schedule(jsonl: &str) -> Result<Vec<(u64, Step)>, String> {
    let mut msgs = Vec::new();
    for (n, line) in jsonl.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
        msgs.push(
            aibus_core::replay::msg_from_json(&v).map_err(|e| format!("line {}: {e}", n + 1))?,
        );
    }
    let mut steps = Vec::new();
    for (i, c) in PROMPT.chars().enumerate() {
        steps.push((
            TYPE_FROM_MS + TYPE_EVERY_MS * i as u64,
            Step::Key(KeyCode::Char(c)),
        ));
    }
    let enter = TYPE_FROM_MS + TYPE_EVERY_MS * PROMPT.chars().count() as u64 + 200;
    steps.push((enter, Step::Key(KeyCode::Enter)));
    // The recorded messages, with their own clock from 0 (squeezed into place at the end).
    let mut body: Vec<(u64, Step)> = Vec::new();
    let mut at = 0;

    let lead = msgs.iter().find_map(|m| match m {
        Msg::RunStarted {
            run, parent: None, ..
        } => Some(*run),
        _ => None,
    });
    let mut progress: Vec<(RunId, ToolCallId, f32)> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        match m {
            Msg::Stream { run, chunks } => {
                for ch in chunks {
                    let pieces: Vec<StreamChunk> = match ch {
                        StreamChunk::TextDelta(t) => {
                            let cs: Vec<char> = t.chars().collect();
                            cs.chunks(STREAM_CHARS)
                                .map(|p| StreamChunk::TextDelta(p.iter().collect()))
                                .collect()
                        }
                        other => vec![other.clone()],
                    };
                    for p in pieces {
                        let text = matches!(p, StreamChunk::TextDelta(_));
                        body.push((
                            at,
                            Step::Msg(Box::new(Msg::Stream {
                                run: *run,
                                chunks: vec![p],
                            })),
                        ));
                        if text {
                            at += TICK_MS;
                        }
                    }
                }
                continue;
            }
            // The recording keeps what a `delegate` call asked for but not the command the tool
            // issued, so it is issued here: the worker's card gets its task.
            Msg::ToolCallRequested { call, .. } if call.name == "delegate" => {
                let child = msgs[i..].iter().find_map(|n| match n {
                    Msg::RunStarted {
                        run,
                        parent: Some(_),
                        ..
                    } => Some(*run),
                    _ => None,
                });
                let s = |k: &str| {
                    call.input
                        .get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned()
                };
                if let (Ok(agent), Some(run)) = (s("agent").parse(), child) {
                    body.push((
                        at,
                        Step::Msg(Box::new(Msg::Command(Command::Delegate {
                            agent,
                            prompt: s("prompt"),
                            background: false,
                            isolation: Isolation::None,
                            parent: lead,
                            run: Some(run),
                            model: None,
                            account: None,
                        }))),
                    ));
                }
            }
            Msg::ToolCallRequested { run, call } if call.name == "report_artifact" => {
                if let Some(f) = call.input.get("progress").and_then(|p| p.as_f64()) {
                    progress.push((*run, call.id, f as f32));
                }
            }
            // The last checkpoint of the recording has nothing to say; an empty line in the rail
            // would only look like a bug.
            Msg::AdvisorSpoke { advice, .. } if advice.is_empty() => continue,
            _ => {}
        }
        let mut m = m.clone();
        // The recording keeps only the length of a tool's output; the card's percentage is the
        // one thing in it the dashboard shows, so it is put back.
        // Call ids restart every turn, so the newest request of that run with the id is the one.
        if let Msg::ToolCallFinished {
            run,
            call_id,
            result,
            ..
        } = &mut m
        {
            result.progress = progress
                .iter()
                .rev()
                .find(|(r, id, _)| r == run && id == call_id)
                .map(|(_, _, f)| Progress::Fraction(*f));
        }
        let wait = gap(&m);
        body.push((at, Step::Msg(Box::new(m))));
        at += wait;
    }
    let from = enter + 200;
    let squeeze = ((SESSION_END_MS - from) as f64 / at.max(1) as f64).min(1.0);
    steps.extend(
        body.into_iter()
            .map(|(t, s)| (from + (t as f64 * squeeze) as u64, s)),
    );
    // The pointer: rest on a card (its tooltip), select it, open the menu of the next one.
    steps.push((7_000, Step::Card(0, MouseEventKind::Moved)));
    steps.push((
        7_900,
        Step::Card(0, MouseEventKind::Down(MouseButton::Left)),
    ));
    steps.push((8_000, Step::Card(0, MouseEventKind::Up(MouseButton::Left))));
    steps.push((
        8_500,
        Step::Card(1, MouseEventKind::Down(MouseButton::Right)),
    ));
    steps.sort_by_key(|(t, _)| *t);
    Ok(steps)
}

/// Plays the demo on `term`. `frame` is called after each of the `LENGTH_MS / FRAME_MS` frames,
/// with the virtual time in ms. The terminal must be `COLS` by `ROWS`.
pub fn play<B: Backend>(
    term: &mut Terminal<B>,
    jsonl: &str,
    mut frame: impl FnMut(u64, &mut Terminal<B>),
) -> Result<(), String> {
    let steps = schedule(jsonl)?;
    let clock = Arc::new(AtomicU64::new(EPOCH_MS));
    let now = clock.clone();
    let mut app = App::new(MiniGraph::new(), move || now.load(Ordering::Relaxed));
    let keys = view::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    let fail = |e: aibus_core::AibusError| e.to_string();
    app.set_session_view(None, (Some("fake-large"), true), (Some("fake-small"), true))
        .map_err(fail)?;
    app.step(Msg::Resize {
        cols: COLS,
        rows: ROWS,
    })
    .map_err(fail)?;

    let mut ui = Ui::new(Theme::recording());
    ui.prompt.placeholder = "aibus".into();
    ui.prompt.cwd = "~/project".into();
    let mut input = Input::default();
    let base = Instant::now();
    let mut pending = steps.into_iter().peekable();
    let lead = lead_of(jsonl);

    for t in 0..LENGTH_MS {
        clock.store(EPOCH_MS + t, Ordering::Relaxed);
        let at = base + Duration::from_millis(t);
        while pending.peek().is_some_and(|(due, _)| *due <= t) {
            let Some((_, step)) = pending.next() else {
                break;
            };
            let outs: Vec<Out> = match step {
                Step::Key(code) => {
                    let cx = Context::read(&app.graph);
                    vec![input.key(
                        KeyEvent {
                            code,
                            ctrl: false,
                            alt: false,
                            shift: false,
                        },
                        &cx,
                        &ui.keys,
                        &ui.drawn,
                        &mut ui.prompt,
                    )]
                }
                Step::Card(i, kind) => match ui.drawn.hits.find(HitId::Card(i)).map(|r| r.rect) {
                    Some(r) => {
                        let ev = MouseEvent {
                            kind,
                            column: r.x + r.width / 2,
                            row: r.y + r.height / 2,
                            modifiers: KeyModifiers::NONE,
                        };
                        let cx = Context::read(&app.graph);
                        vec![input.mouse(ev, at, &ui.drawn, &cx, &mut ui.prompt)]
                    }
                    None => Vec::new(),
                },
                Step::Msg(m) => {
                    app.step(*m).map_err(fail)?;
                    Vec::new()
                }
            };
            for out in outs {
                for m in out.msgs {
                    match m {
                        // The prompt starts the lead the way the launcher would: as the run the
                        // recording names, so the recorded messages find it.
                        Msg::SubmitPrompt { text } => {
                            app.step(Msg::Command(Command::Delegate {
                                agent: "lead".parse().map_err(|e| format!("{e}"))?,
                                prompt: text,
                                background: false,
                                isolation: Isolation::None,
                                parent: None,
                                run: lead,
                                model: None,
                                account: None,
                            }))
                            .map_err(fail)?;
                        }
                        other => app.step(other).map_err(fail)?,
                    }
                }
            }
        }
        input.settle(at);
        let typing = app
            .graph
            .read_key(view::RAIL)
            .is_some_and(view::rail_typing);
        let animating = typing || matches!(app.graph.read_key(ANIMATING), Some(Value::Bool(true)));
        let blinking = app
            .graph
            .read_key(aibus_core::state::FOCUS)
            .and_then(as_str)
            == Some("input");
        if t % TICK_MS == 0 && (animating || blinking) {
            app.step(Msg::Tick).map_err(fail)?;
        }
        if t % FRAME_MS == 0 {
            crate::app::sync(&mut ui, &input);
            if let Some(key) = view::ensure_lines(&mut app.graph) {
                app.keys.push(key);
            }
            ui.note_commit(&app.graph);
            ui.animating = animating;
            ui.draw(term, &app.graph).map_err(|e| e.to_string())?;
            frame(t, term);
        }
    }
    Ok(())
}

/// The id of the lead run in the recording.
fn lead_of(jsonl: &str) -> Option<RunId> {
    jsonl.lines().find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        (v["msg"] == "RunStarted" && v["parent"].is_null())
            .then(|| v["run"].as_str()?.parse().ok())?
    })
}

/// What `aibus demo` does. With `text`, each frame is written as plain text under a `=== t=<ms>`
/// line. Without, the rows that changed since the last frame are drawn with ANSI at a fixed
/// `COLS` by `ROWS` (no tty is asked its size), each frame followed by the private sequence
/// `ESC ] 7777 ; <ms> BEL`. An emulator ignores it; `tools/demo-cast.py` takes it as the frame's
/// time, so the recording follows the virtual clock and not how fast this process ran.
pub fn print(text: bool, mut out: impl std::io::Write) -> Result<(), String> {
    // The test backend cannot fail (its error is `Infallible`), so there is no `?` to write.
    let Ok(mut term) = Terminal::new(ratatui::backend::TestBackend::new(COLS, ROWS));
    let mut failed = None;
    let mut prev: Option<ratatui::buffer::Buffer> = None;
    if !text {
        failed = out.write_all(b"\x1b[2J\x1b[H\x1b[?25l").err();
    }
    play(&mut term, RECORDING, |t, term| {
        if failed.is_some() {
            return;
        }
        let buf = term.backend().buffer();
        failed = if text {
            write!(out, "=== t={t}\n{}", text_of(buf)).err()
        } else {
            let frame = ansi_of(buf, prev.as_ref());
            prev = Some(buf.clone());
            write!(out, "{frame}\x1b]7777;{t}\x07")
                .and_then(|()| out.flush())
                .err()
        };
    })?;
    if !text && failed.is_none() {
        failed = out.write_all(b"\x1b[?25h").err();
    }
    failed.map_or(Ok(()), |e| Err(e.to_string()))
}

/// The rows of `buf` that differ from `prev` as ANSI: truecolor, 256-colour and the 16 named ones.
fn ansi_of(buf: &ratatui::buffer::Buffer, prev: Option<&ratatui::buffer::Buffer>) -> String {
    use ratatui::style::Modifier;
    use std::fmt::Write as _;
    let mut out = String::new();
    for y in 0..buf.area.height {
        if prev.is_some_and(|p| (0..buf.area.width).all(|x| p[(x, y)] == buf[(x, y)])) {
            continue;
        }
        let _ = write!(out, "\x1b[{};1H", y + 1);
        for x in 0..buf.area.width {
            let c = &buf[(x, y)];
            let mods: String = [
                (Modifier::BOLD, 1),
                (Modifier::DIM, 2),
                (Modifier::ITALIC, 3),
                (Modifier::UNDERLINED, 4),
                (Modifier::REVERSED, 7),
            ]
            .iter()
            .filter(|(m, _)| c.modifier.contains(*m))
            .map(|(_, n)| format!(";{n}"))
            .collect();
            let _ = write!(
                out,
                "\x1b[0{mods};{};{}m{}",
                crate::theme::sgr(c.fg, true),
                crate::theme::sgr(c.bg, false),
                c.symbol()
            );
        }
        out.push_str("\x1b[0m");
    }
    out
}

/// A frame as text: one line per row, trailing blanks trimmed.
pub fn text_of(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use super::*;

    use super::RECORDING as MSGS;

    fn frames() -> Vec<(u64, String)> {
        let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        let mut out = Vec::new();
        play(&mut term, MSGS, |t, term| {
            out.push((t, text_of(term.backend().buffer())))
        })
        .unwrap();
        out
    }

    #[test]
    fn two_plays_draw_the_same_frames() {
        let (a, b) = (frames(), frames());
        assert_eq!(a.len() as u64, LENGTH_MS / FRAME_MS);
        assert_eq!(a, b);
    }

    #[test]
    fn the_demo_shows_a_lead_delegating_and_ends_on_a_menu() {
        let f = frames();
        let all: String = f.iter().map(|(_, t)| t.as_str()).collect();
        for want in [PROMPT, "search", "grep", "audit", "fake-small"] {
            assert!(all.contains(want), "{want} never on screen");
        }
        let last = &f.last().unwrap().1;
        assert!(last.contains("Open transcript"), "{last}");
    }

    #[test]
    fn the_session_finishes_before_the_pointer_starts() {
        let steps = schedule(MSGS).unwrap();
        let last_msg = steps
            .iter()
            .filter(|(_, s)| matches!(s, Step::Msg(_)))
            .map(|(t, _)| *t)
            .max()
            .unwrap();
        let first_card = steps
            .iter()
            .filter(|(_, s)| matches!(s, Step::Card(..)))
            .map(|(t, _)| *t)
            .min()
            .unwrap();
        assert!(last_msg < first_card && steps.iter().all(|(t, _)| *t < LENGTH_MS));
    }
}
