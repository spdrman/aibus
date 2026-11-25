//! #284: `/runs` in the dashboard. The prompt turns the line into `Command::Runs`, the terminal
//! loop answers through `runs::apply` (the rig stands in for it), and the screen shows the same
//! text `aibus runs` prints: the fleet, a run's planned and execution graph and timeline, the
//! inspector and the replay. The keys scroll it and never past its end; a hostile trace reaches
//! the buffer as text.

mod common;

use std::path::{Path, PathBuf};

use aibus_core::*;
use aibus_testkit::assert_frame;
use aibus_trace::view::{self, Recorded};
use aibus_tui::runs::apply;
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;

fn traces() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284")
}

fn rig() -> Rig {
    Rig::new(session(), (120, 40), Theme::recording())
}

/// What the loop does with the messages a key press produced.
fn line(r: &mut Rig, root: &Path, text: &str) {
    r.type_text(text);
    for m in r.key(K::Enter) {
        if let Msg::Command(Command::Runs(q)) = m {
            for a in apply(&q, root) {
                r.app.step(a).unwrap();
            }
        }
    }
    r.settle();
}

#[test]
fn slash_runs_opens_the_fleet() {
    let mut r = rig();
    line(&mut r, &traces(), "/runs");
    let text = r.text();
    assert!(text.contains("recorded runs"));
    assert!(
        text.contains("RUN")
            && text.contains("EDGES o/c/i/u")
            && text.contains("11-foreground.hooks"),
        "{text}"
    );
    assert_frame!(r.frame(), "runs_fleet");
}

#[test]
fn slash_runs_inspect_shows_the_graph_then_the_timeline_and_scrolls_to_the_end_and_no_further() {
    let mut r = rig();
    line(&mut r, &traces(), "/runs inspect 11-foreground.hooks");
    let text = r.text();
    assert!(
        text.contains("plan (the definition graph: intent, not proof of execution)"),
        "{text}"
    );
    assert!(
        text.contains("edge observed cross-checked (cli_stream, hook)"),
        "{text}"
    );
    assert_frame!(r.frame(), "runs_inspect_top");
    r.key(K::PageDown);
    r.key(K::Down);
    r.settle();
    assert!(r.text().contains("lines 22-"), "{}", r.text());
    // Past the end, the last line stays the first one shown.
    let rec = Recorded::load(&traces().join("11-foreground.hooks")).unwrap();
    let total = view::graph(&rec, "x").lines().count()
        + view::timeline(&rec, &Default::default()).lines().count();
    r.press('G');
    r.press('j');
    r.press(' ');
    r.settle();
    assert!(
        r.text()
            .contains(&format!("lines {total}-{total} of {total}")),
        "{}",
        r.text()
    );
    r.press('k');
    r.settle();
    assert!(
        r.text().contains(&format!("lines {}-", total - 1)),
        "{}",
        r.text()
    );
    r.press('g');
    r.settle();
    assert!(r.text().contains("lines 1-"));
    r.key(K::Esc);
    r.settle();
    assert!(
        !r.text().contains("plan (the definition graph"),
        "Esc closes the screen"
    );
}

#[test]
fn slash_runs_inspect_a_node_opens_the_inspector_and_replay_shows_the_transitions() {
    let mut r = rig();
    let rec = Recorded::load(&traces().join("11-foreground.hooks")).unwrap();
    let child = aibus_trace::project(&rec.events).edges[0].child.clone();
    line(
        &mut r,
        &traces(),
        &format!("/runs inspect 11-foreground.hooks {child}"),
    );
    let text = r.text();
    assert!(text.contains(&format!("agent {child}")), "{text}");
    assert!(
        text.contains("evidence (open one with --event <sequence>):"),
        "{text}"
    );
    assert_frame!(r.frame(), "runs_inspector");
    r.key(K::Esc);
    line(&mut r, &traces(), "/runs replay 07-deleg");
    let text = r.text();
    assert!(
        text.contains("replay of 07-deleg: 29 recorded events, offline"),
        "{text}"
    );
    assert!(
        text.contains(
            "#1 observed aibus_scheduler: attempt 00000000-0000-0000-0000-000002820002 started"
        ),
        "{text}"
    );
}

#[test]
fn slash_runs_compare_and_repro_open_what_the_commands_print() {
    let mut r = rig();
    line(&mut r, &traces(), "/runs compare 06-basic 07-deleg");
    let text = r.text();
    assert!(
        text.contains("comparison of 06-basic (a) and 07-deleg (b)"),
        "{text}"
    );
    assert!(text.contains("comparability unknown"), "{text}");
    r.key(K::Esc);
    line(&mut r, &traces(), "/runs repro 07-deleg");
    let text = r.text();
    assert!(text.contains("reproducibility of 07-deleg"), "{text}");
    assert!(text.contains("not a replay"), "{text}");
    r.key(K::Esc);
    line(&mut r, &traces(), "/runs compare 06-basic nope");
    assert!(r.text().contains("no run nope"), "{}", r.text());
}

#[test]
fn a_bad_run_or_argument_says_so_in_the_log_and_opens_nothing() {
    let mut r = rig();
    line(&mut r, &traces(), "/runs inspect ../etc");
    assert!(r.text().contains("not a run id"), "{}", r.text());
    line(&mut r, &traces(), "/runs inspect nope");
    assert!(r.text().contains("no run nope"), "{}", r.text());
    line(&mut r, &traces(), "/runs frobnicate");
    assert!(
        r.text()
            .contains("/runs takes no argument, inspect <run> [<node>], replay <run>, compare <a> <b> or repro <run>"),
        "{}",
        r.text()
    );
    assert!(!r.text().contains("plan (the definition graph"));
}

#[test]
fn a_hostile_trace_reaches_the_screen_as_text() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("evil");
    std::fs::create_dir_all(&dir).unwrap();
    let raw = std::fs::read_to_string(traces().join("07-deleg/events.jsonl")).unwrap();
    std::fs::write(
        dir.join("events.jsonl"),
        raw.replace(
            "reader-a",
            "reader\\u001b]52;c;cm0=\\u0007\\u001b[2J\\u009b\\u202e",
        ),
    )
    .unwrap();
    let mut r = rig();
    line(&mut r, home.path(), "/runs inspect evil");
    let text = r.text();
    assert!(
        text.contains("reader\\u{1b}]52;c;cm0=\\u{7}\\u{1b}[2J\\u{9b}\\u{202e}"),
        "{text}"
    );
    assert!(
        !text
            .chars()
            .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}'),
        "{text}"
    );
}

#[test]
fn slash_runs_compare_says_when_a_trace_was_only_partly_read() {
    let root = tempfile::tempdir().unwrap();
    for name in ["06-basic", "07-deleg"] {
        let dir = root.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        let mut events = std::fs::read(traces().join(name).join("events.jsonl")).unwrap();
        if name == "07-deleg" {
            events.extend_from_slice(b"not json\n");
        }
        std::fs::write(dir.join("events.jsonl"), events).unwrap();
    }
    let mut r = rig();
    line(&mut r, root.path(), "/runs compare 06-basic 07-deleg");
    let text = r.text();
    assert!(
        text.contains("comparison of 06-basic (a) and 07-deleg (b)"),
        "{text}"
    );
    assert!(text.contains("1 corrupt"), "{text}");
}
