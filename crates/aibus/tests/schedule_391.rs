//! `aibus schedule print` (#391, H1.4): the `[[schedule]]` table rendered as a crontab line, a
//! launchd plist and a systemd service and timer. It prints; it never installs.
//!
//! The three forms are compared with the files in `schedule_391/`. The same command each form
//! carries is run on the fake provider, and has to exit 0.

mod common;

use std::fs;
use std::path::Path;

use aibus_testkit::xdg_temp;
use common::{aibus, command, stderr};
use serde_json::Value;

const BIN: &str = "/usr/local/bin/aibus";

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn config(home: &Path, text: &str) -> String {
    let path = home.join("schedule.toml");
    fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

fn print(home: &Path, cfg: &str, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["--config", cfg, "schedule", "print", "--bin", BIN];
    args.extend(extra);
    aibus(home, &args)
}

#[test]
fn each_schedule_prints_the_three_forms_as_in_the_snapshot() {
    let home = xdg_temp();
    let cfg = config(home.path(), include_str!("schedule_391/config.toml"));
    let morning = include_str!("schedule_391/morning-triage.txt");
    let quarter = include_str!("schedule_391/quarter-hour.txt");
    for (name, want) in [("morning-triage", morning), ("quarter-hour", quarter)] {
        let out = print(home.path(), &cfg, &[name]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert_eq!(stdout(&out), want, "{name}");
    }
    // No name: every schedule, a blank line between.
    let out = print(home.path(), &cfg, &[]);
    assert_eq!(stdout(&out), format!("{morning}\n{quarter}"));
}

#[test]
fn print_says_it_installs_nothing_and_writes_nothing() {
    let home = xdg_temp();
    let cfg = config(home.path(), include_str!("schedule_391/config.toml"));
    // The test harness makes the working directory on first use.
    fs::create_dir_all(home.path().join("proj")).unwrap();
    let before: Vec<_> = fs::read_dir(home.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let out = print(home.path(), &cfg, &[]);
    assert!(
        stderr(&out).contains("nothing is installed"),
        "{}",
        stderr(&out)
    );
    let mut after: Vec<_> = fs::read_dir(home.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(before, after, "print created files");
    for gone in ["Library", ".config/systemd"] {
        assert!(!home.path().join(gone).exists(), "{gone} was created");
    }
}

#[test]
fn the_command_of_every_form_runs_on_the_fake_provider_and_exits_zero() {
    let home = xdg_temp();
    let proj = home.path().join("proj");
    fs::create_dir_all(&proj).unwrap();
    fs::write(proj.join("README.md"), "hello").unwrap();
    let cfg = config(
        home.path(),
        &format!(
            "[[schedule]]\nname = \"one\"\nagent = \"explore\"\nprompt = \"map this repo\"\ncron = \"0 6 * * *\"\nproject = {p:?}\n\n\
             [[schedule]]\nname = \"two\"\nagent = \"general\"\nprompt = \"say hi\"\ncron = \"15 9 * * 1\"\nproject = {p:?}\nmax_cost_usd = 1.0\n",
            p = proj.to_str().unwrap()
        ),
    );
    let exe = env!("CARGO_BIN_EXE_aibus");
    let out = aibus(
        home.path(),
        &[
            "--config", &cfg, "schedule", "print", "--json", "--bin", exe,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rows: Vec<Value> = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        let argv: Vec<&str> = row["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(argv[0], exe);
        assert!(argv.contains(&"dont-ask"), "{argv:?}");
        for form in ["crontab", "launchd"] {
            assert!(row[form].as_str().is_some_and(|t| !t.is_empty()), "{form}");
        }
        assert!(row["systemd"]["service"]
            .as_str()
            .unwrap()
            .contains("Type=oneshot"));
        assert!(row["systemd"]["timer"]
            .as_str()
            .unwrap()
            .contains("OnCalendar="));
        let mut cmd = command(home.path());
        cmd.args(&argv[1..]);
        for (k, v) in row["env"].as_object().unwrap() {
            cmd.env(k, v.as_str().unwrap());
        }
        let run = cmd.output().unwrap();
        assert_eq!(
            run.status.code(),
            Some(0),
            "{}: {}",
            row["name"],
            stderr(&run)
        );
    }
}

#[test]
fn a_bad_schedule_is_a_config_error_with_the_name_of_the_problem() {
    let home = xdg_temp();
    let one = |body: &str| {
        format!(
            "[[schedule]]\nname = \"a\"\nagent = \"explore\"\nprompt = \"x\"\ncron = \"0 6 * * *\"\nproject = \"/p\"\n{body}"
        )
    };
    let swap = |from: &str, to: &str| one("").replace(from, to);
    let cases: Vec<(String, &str)> = vec![
        (swap("0 6 * * *", "61 6 * * *"), "cron"),
        (swap("0 6 * * *", "0 6 * *"), "cron"),
        (
            swap("0 6 * * *", "0 6 1 * 1"),
            "day of month and day of week",
        ),
        (swap("\"/p\"", "\"rel/p\""), "project"),
        (swap("name = \"a\"", "name = \"A b\""), "name"),
        (swap("prompt = \"x\"", "prompt = \" \""), "prompt"),
        (swap("agent = \"explore\"", "agent = \"../x\""), "agent"),
        (one("max_cost_usd = 0\n"), "max_cost_usd"),
        (format!("{}\n{}", one(""), one("")), "twice"),
    ];
    for (text, word) in cases {
        let cfg = config(home.path(), &text);
        for args in [
            vec!["--config", &cfg, "config", "validate"],
            vec!["--config", &cfg, "schedule", "print"],
        ] {
            let out = aibus(home.path(), &args);
            assert_eq!(
                out.status.code(),
                Some(3),
                "{word}: {args:?}: {}",
                stderr(&out)
            );
            let text = format!("{}{}", stdout(&out), stderr(&out));
            assert!(
                text.contains("schedule") && text.contains(word),
                "{word}: {text}"
            );
        }
    }
}

#[test]
fn an_unknown_name_is_a_usage_error_and_an_empty_config_prints_a_note() {
    let home = xdg_temp();
    let cfg = config(home.path(), include_str!("schedule_391/config.toml"));
    let out = print(home.path(), &cfg, &["nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
    let empty = config(home.path(), "");
    let out = print(home.path(), &empty, &[]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("no [[schedule]]"), "{}", stderr(&out));
}
