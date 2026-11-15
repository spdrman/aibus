//! #284: `aibus runs list | inspect | replay | rerun` over the recorded spike traces. The binary
//! prints exactly what `aibus_trace::view` renders, replays the stored events byte for byte, never
//! needs a provider, a network or a CLI on PATH, and refuses a rerun without claiming it would
//! come out the same.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use aibus_trace::view::{self, Filter, Recorded};
use common::*;

fn traces() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284")
}

/// `aibus runs <args> --root <traces>` with no provider config, an empty PATH and a HOME of its
/// own: whatever it prints, it printed without reaching anything.
fn runs(home: &Path, args: &[&str]) -> std::process::Output {
    let root = traces();
    let mut c = command(home);
    c.env("PATH", "")
        .env("AIBUS_CONFIG", home.join("none.toml"));
    c.arg("runs").args(args).arg("--root").arg(&root);
    c.output().unwrap()
}

fn stdout(o: &std::process::Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn runs_list_prints_the_fleet_and_its_json_rows() {
    let home = aibus_testkit::xdg_temp();
    let o = runs(home.path(), &["list"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), view::render_fleet(&view::fleet(&traces())));
    assert_eq!(
        stdout(&o),
        fs::read_to_string(traces().join("../views-284/fleet.txt")).unwrap()
    );

    let mut c = command(home.path());
    let o = c
        .args(["--json", "runs", "list", "--root"])
        .arg(traces())
        .output()
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rows.len(), 20);
    for r in &rows {
        assert!(
            r["state"].is_string() && r["edges"].as_array().unwrap().len() == 4,
            "{r}"
        );
        assert!(
            ["estimated", "unknown"].contains(&r["cost"]["quality"].as_str().unwrap()),
            "{r}"
        );
    }

    let o = runs(home.path(), &["list", "--names"]);
    let names: Vec<&str> = std::str::from_utf8(&o.stdout).unwrap().lines().collect();
    assert_eq!(names.len(), 20);
    assert!(names.contains(&"11-foreground.hooks"));

    // No traces yet: an empty list, exit 0.
    let o = command(home.path())
        .args(["runs", "list"])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(stdout(&o), "no recorded runs\n");
}

#[test]
fn runs_inspect_prints_the_graph_and_timeline_a_node_and_an_event() {
    let home = aibus_testkit::xdg_temp();
    let rec = Recorded::load(&traces().join("11-foreground.hooks")).unwrap();
    let o = runs(home.path(), &["inspect", "11-foreground.h"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let mut want = view::graph(&rec, "11-foreground.hooks");
    want.push_str(&view::timeline(&rec, &Filter::default()));
    assert_eq!(stdout(&o), want);

    let child = aibus_trace::project(&rec.events).edges[0].child.clone();
    let o = runs(
        home.path(),
        &["inspect", "11-foreground.hooks", "--node", &child],
    );
    assert_eq!(stdout(&o), view::inspect(&rec, &child).unwrap());
    let o = runs(
        home.path(),
        &["inspect", "11-foreground.hooks", "--event", "4"],
    );
    assert_eq!(stdout(&o), view::event(&rec, 4).unwrap());

    let o = runs(
        home.path(),
        &[
            "inspect",
            "11-foreground.hooks",
            "--level",
            "correlated",
            "--kind",
            "tool_started",
        ],
    );
    let timeline = stdout(&o);
    let rows: Vec<&str> = timeline
        .split("timeline (")
        .nth(1)
        .unwrap()
        .lines()
        .skip(1)
        .collect();
    assert!(
        !rows.is_empty()
            && rows
                .iter()
                .all(|l| l.contains(" tool_started correlated hook ")),
        "{rows:#?}"
    );

    for (args, says) in [
        (
            &["inspect", "11-foreground.hooks", "--node", "nobody"][..],
            "has no agent, tool use or attempt nobody",
        ),
        (
            &["inspect", "11-foreground.hooks", "--event", "100000"][..],
            "has no event #100000",
        ),
        (
            &["inspect", "11-foreground.hooks", "--kind", "made_up"][..],
            "--kind made_up: not one of the trace's names",
        ),
        (&["inspect", "../traces-284/06-basic"][..], "not a run id"),
        (&["inspect", "11"][..], "names 3 runs"),
        (&["inspect", "nope"][..], "no run nope"),
    ] {
        let o = runs(home.path(), args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(stderr(&o).contains(says), "{args:?}: {}", stderr(&o));
    }
}

#[test]
fn runs_replay_prints_the_transitions_and_with_events_the_recorded_bytes() {
    let home = aibus_testkit::xdg_temp();
    for name in [
        "06-basic",
        "07-deleg.hooks",
        "10-killed",
        "11-foreground.retained",
        "15-killed-midrun.hooks",
    ] {
        let rec = Recorded::load(&traces().join(name)).unwrap();
        let o = runs(home.path(), &["replay", name, "--offline"]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert_eq!(stdout(&o), view::replay(&rec, name));
        let o = runs(home.path(), &["replay", name, "--events"]);
        assert!(o.status.success());
        assert_eq!(
            o.stdout,
            fs::read(traces().join(name).join("events.jsonl")).unwrap(),
            "{name}: not byte for byte"
        );
    }
}

#[test]
fn runs_rerun_refuses_and_never_claims_the_outcome_would_be_the_same() {
    let home = aibus_testkit::xdg_temp();
    let before: Vec<_> = fs::read_dir(traces())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    let o = runs(home.path(), &["rerun", "07-deleg", "--isolated"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    let e = stderr(&o).to_lowercase();
    assert!(
        e.contains("not available yet") && e.contains("may differ"),
        "{e}"
    );
    assert!(e.contains("aibus runs replay 07-deleg"), "{e}");
    for claim in [
        "deterministic",
        "identical",
        "same result",
        "same output",
        "reproduces",
    ] {
        assert!(!e.contains(claim), "rerun claims {claim}: {e}");
    }
    let after: Vec<_> = fs::read_dir(traces())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(before.len(), after.len(), "a rerun wrote nothing");
    let o = runs(home.path(), &["rerun", "nope"]);
    assert!(stderr(&o).contains("no run nope"));
}

#[test]
fn a_hostile_trace_reaches_the_terminal_without_a_single_escape_sequence() {
    let home = aibus_testkit::xdg_temp();
    let root = home.path().join("hostile");
    let dir = root.join("run1");
    fs::create_dir_all(&dir).unwrap();
    let mut line = fs::read_to_string(traces().join("07-deleg/events.jsonl")).unwrap();
    line = line.replace(
        "reader-a",
        "reader\\u001b]52;c;cm0gLXJm\\u0007\\u001b[2J\\u009b31m\\u202egnp",
    );
    line.push_str("{\"schema_version\":1,\"kind\":\"\u{fffd}\"}\n\u{1b}[31mnot json\n");
    fs::write(dir.join("events.jsonl"), line).unwrap();
    for args in [
        &["list"][..],
        &["inspect", "run1"],
        &["replay", "run1"],
        &["inspect", "run1", "--event", "4"],
    ] {
        let mut c = command(home.path());
        let o = c
            .arg("runs")
            .args(args)
            .arg("--root")
            .arg(&root)
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
        let text = stdout(&o);
        assert!(
            !text
                .chars()
                .any(|c| (c.is_control() && c != '\n') || ('\u{202a}'..='\u{202e}').contains(&c)),
            "{args:?}: {text}"
        );
        match args.len() {
            2 => assert!(
                text.contains("reader\\u{1b}]52;c;cm0gLXJm\\u{7}\\u{1b}[2J\\u{9b}31m\\u{202e}gnp"),
                "{text}"
            ),
            // The event as JSON: serde writes the C0 controls as JSON escapes, the view the rest.
            4 => assert!(
                text.contains("reader\\u001b]52;c;cm0gLXJm\\u0007\\u001b[2J\\u{9b}31m\\u{202e}gnp"),
                "{text}"
            ),
            _ => {}
        }
    }
}
