//! G9: the rules that cut across the commands, checked as a set so they stay checked when a row is
//! added to the registry (`docs/SECURITY.md`, the rows for G9). The core half of the set is
//! `aibus-core/tests/security/commands.rs`; this half drives the dashboard.

mod common;

#[path = "../../../fuzz/commands_checks.rs"]
mod commands_checks;

use std::path::{Path, PathBuf};

use aibus_core::*;
use aibus_graph::Graph;
use aibus_tui::commands::{help_lines, send, Env, REGISTRY};
use aibus_tui::prompt::{Prompt, Sent};
use aibus_tui::{Layout, Theme};
use common::*;
use crossterm::event::KeyCode as K;

const KEY: &str = "sk-test-LOGIN-0123456789abcdef";

const LEAD: &str = "01J00000000000000000000001";
const GREP: &str = "01J00000000000000000000002";

fn run(id: &str) -> RunId {
    id.parse().unwrap()
}

/// A session with a lead and a worker running, an account called `work`, the `/fix` family
/// present and an MCP server configured: every command is on offer.
fn with_env<T>(f: impl FnOnce(&Env) -> T) -> T {
    let agents = vec!["grep".to_owned(), "workflow-author".to_owned()];
    let workflows = ["fix", "build", "review", "explore"]
        .map(str::to_owned)
        .to_vec();
    let runs = vec![
        (run(LEAD), "lead".to_owned()),
        (run(GREP), "grep".to_owned()),
    ];
    let states = vec![
        (run(LEAD), RunStatus::Running),
        (run(GREP), RunStatus::Running),
    ];
    let accounts = vec!["work".to_owned()];
    f(&Env {
        agents: &agents,
        workflows: &workflows,
        lead: Some(run(LEAD)),
        focused: None,
        runs: &runs,
        states: &states,
        accounts: true,
        mcp: true,
        account_names: &accounts,
    })
}

fn msgs(line: &str) -> Vec<Msg> {
    with_env(|env| match send(line, env) {
        Sent::Msgs(m) | Sent::Login(_, m) => m,
        Sent::Bad(why) => panic!("{line}: {why}"),
    })
}

// ---- the author child ---------------------------------------------------------------------------

/// The only thing a command can say about the workflow author's child is `write: bool`: the
/// program builds the child's arguments from that (`aibus/src/author.rs`, whose own tests pin that
/// they are `mcp` and, with the flag, `--allow write` and nothing else). So whatever else is typed
/// after `/workflow` has to stay the goal, in words, and never become a second flag.
#[test]
fn the_author_child_never_gets_gates_or_run() {
    let payloads = [
        "new --allow gates a goal",
        "new --allow run a goal",
        "new --write --allow gates a goal",
        "new --write --allow run a goal",
        "new --allow-tool bash a goal",
        "new --http 127.0.0.1:9 a goal",
        "new --no-provenance a goal",
        "new --writefoo a goal",
        "new --WRITE a goal",
        "edit review --allow gates tighten it",
        "edit review --write --allow run tighten it",
    ];
    let mut spawning = Vec::new();
    for spec in REGISTRY.iter().filter(|s| s.surfaces.slash > 0) {
        for payload in payloads {
            let line = format!("/{} {payload}", spec.name);
            let sent = with_env(|env| send(&line, env));
            let (Sent::Msgs(out) | Sent::Login(_, out)) = sent else {
                continue;
            };
            for m in out {
                let Msg::Command(Command::WorkflowAuthor { goal, write, edit }) = m else {
                    continue;
                };
                if !spawning.contains(&spec.name) {
                    spawning.push(spec.name);
                }
                // `write` is true for `--write` as its own first word after `new`, and for `edit`,
                // which writes to the one workflow it names. Nothing else turns it on.
                let want = payload.starts_with("edit") || payload.starts_with("new --write ");
                assert_eq!(write, want, "{line}");
                assert_eq!(edit.is_some(), payload.starts_with("edit"), "{line}");
                // Every word that looks like a server flag is still in the goal, as words.
                for flag in ["--allow", "--allow-tool", "--http", "--no-provenance"] {
                    if payload.contains(flag) {
                        assert!(
                            goal.contains(flag),
                            "{line}: the flag left the goal: {goal:?}"
                        );
                    }
                }
            }
        }
    }
    assert_eq!(spawning, ["workflow"], "the rows that start the author");
}

// ---- money and writes ask ---------------------------------------------------------------------

#[test]
fn cancel_lead_asks() {
    let ask = format!("Command(OpenScreen(ConfirmCancel({:?})))", run(LEAD));
    for line in ["/cancel", "/cancel @lead", "/cancel @LEAD "] {
        let got = with_env(|env| match send(line, env) {
            Sent::Msgs(m) => m.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>(),
            Sent::Bad(why) => vec![format!("BAD {why}")],
            Sent::Login(..) => vec!["LOGIN".into()],
        });
        // `@LEAD` is not a seat name: it matches no agent and is refused, which is also not a cancel.
        assert!(
            got == [ask.clone()] || got[0].starts_with("BAD"),
            "{line}: {got:?}"
        );
    }
    // The lead by its run id, too: the ask follows the run, not the spelling.
    let by_id = msgs(&format!("/cancel @{}", &LEAD[..26]));
    assert_eq!(format!("{:?}", by_id[0]), ask);
    // A worker is cancelled at once.
    assert!(matches!(
        &msgs("/cancel @grep")[..],
        [Msg::Command(Command::Cancel { .. })]
    ));
    // And nothing typed ever cancels the lead in one step.
    for line in ["/cancel", "/cancel @lead", "/cancel   @lead"] {
        assert!(
            !msgs(line)
                .iter()
                .any(|m| matches!(m, Msg::Command(Command::Cancel { run: r }) if *r == run(LEAD))),
            "{line}"
        );
    }
}

// ---- secrets ----------------------------------------------------------------------------------

#[test]
fn prompt_history_never_holds_a_login_line() {
    let mut p = Prompt::default();
    for text in [
        format!("/login anthropic {KEY}"),
        format!("  /login   anthropic\t{KEY}  "),
        format!("/login anthropic\n{KEY}"),
        format!("/login anthropic --key {KEY}"),
    ] {
        p.set(&text);
        assert!(p.take().is_some());
    }
    assert!(
        p.history
            .iter()
            .all(|h| !h.contains("sk-test") && !h.contains(KEY)),
        "{:?}",
        p.history
    );
    // The commands that carry no key are history like any other.
    for text in ["/login", "/login anthropic", "/logout work"] {
        p.set(text);
        p.take();
    }
    assert_eq!(p.history, ["/login", "/login anthropic", "/logout work"]);
}

#[test]
fn an_inline_key_is_redacted_in_the_notice_and_the_log() {
    let mut r = Rig::new(session(), (98, 46), Theme::recording());
    r.type_text(&format!("/login anthropic {KEY}"));
    let sent = r.key(K::Enter);
    assert!(sent.is_empty(), "nothing reaches the graph: {sent:?}");

    let notice = r.ui.prompt.notice.clone().expect("a notice says why");
    assert!(
        notice.contains("[redacted]") && !notice.contains("sk-test") && !notice.contains(KEY),
        "{notice}"
    );
    assert!(!r.text().contains("sk-test"), "{}", r.text());

    // The log, the graph and everything the session would save.
    let log = format!("{:?}", r.app.store.log_rows(1_000));
    let snapshot = format!("{:?}", r.app.graph.snapshot());
    for (what, text) in [
        ("the log", log),
        ("the snapshot", snapshot),
        ("the jobs", format!("{:?}", r.jobs)),
    ] {
        assert!(!text.contains("LOGIN-0123456789"), "the key is in {what}");
    }
}

#[test]
fn a_key_typed_on_the_login_line_is_drawn_as_dots() {
    let mut r = Rig::new(session(), (98, 46), Theme::recording());
    r.type_text("/login anthropic ");
    assert!(
        r.text().contains("/login anthropic ▌"),
        "the provider name is plain: {}",
        r.text()
    );
    r.type_text(KEY);
    let text = r.text();
    assert!(
        !text.contains("sk-test") && !text.contains("LOGIN"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "/login anthropic {}",
            "•".repeat(KEY.chars().count())
        )),
        "{text}"
    );
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

/// The whole flow with the keyboard, `/login` and then `/logout`, and everything it leaves: every
/// frame drawn, every notice, the history, the messages, the jobs, the graph, and `ui.toml`.
#[test]
fn no_login_secret_in_a_frame_a_notice_or_ui_toml() {
    let mut seen: Vec<String> = Vec::new();
    let line = |r: &mut Rig, text: &str, seen: &mut Vec<String>| {
        r.type_text(text);
        seen.push(r.text());
        seen.extend(names(&r.key(K::Enter)));
        seen.push(r.text());
        seen.push(format!("{:?}", r.ui.prompt.notice));
    };
    let finish = |r: &Rig, seen: &mut Vec<String>| {
        seen.push(format!("{:?}", r.jobs));
        seen.push(format!("{:?}", r.ui.prompt.history));
        seen.push(format!("{:?}", r.app.store.log_rows(1_000)));
        seen.push(format!("{:?}", r.app.graph.snapshot()));
    };

    // The key typed into its masked field, and sent.
    let mut r = Rig::new(session(), (98, 46), Theme::recording());
    line(&mut r, "/login anthropic", &mut seen);
    line(&mut r, "work2", &mut seen);
    r.type_text(KEY);
    seen.push(r.text());
    seen.extend(names(&r.key(K::Enter)));
    seen.push(r.text());
    finish(&r, &mut seen);

    // The key typed on the line, which is refused.
    let mut r = Rig::new(session(), (98, 46), Theme::recording());
    line(&mut r, &format!("/login anthropic {KEY}"), &mut seen);
    finish(&r, &mut seen);

    // A logout, confirmed and not.
    let mut r = Rig::new(session(), (98, 46), Theme::recording());
    r.app
        .step(Msg::AccountsReloaded {
            accounts: vec![AccountView {
                name: AccountName("work".into()),
                provider: "anthropic".into(),
                label: "work".into(),
                default_model: None,
                key_ref: "keyring:aibus/work".into(),
                models: vec![],
            }],
        })
        .unwrap();
    line(&mut r, "/logout work", &mut seen);
    r.press('n');
    seen.push(r.text());
    line(&mut r, "/logout work", &mut seen);
    r.press('y');
    seen.push(r.text());
    finish(&r, &mut seen);

    // `ui.toml` is the one file the dashboard writes on its own.
    let dir = tempfile::tempdir().unwrap();
    Layout {
        rail: Some(30),
        log: Some(8),
    }
    .save(&dir.path().join("state/ui.toml"))
    .unwrap();
    for file in walk(dir.path()) {
        seen.push(std::fs::read_to_string(file).unwrap());
    }
    for text in &seen {
        assert!(!text.contains("LOGIN-0123456789"), "the key leaked: {text}");
    }
}

// ---- the parser -------------------------------------------------------------------------------

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut all: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    all.sort();
    all
}

/// The fuzz target's seeds are every documented command, so a command added to the registry
/// without a seed fails here, and the corpus and crash fixtures run as a regression suite.
#[test]
fn commands_send_corpus_and_crashes_hold() {
    let mut n = 0;
    for dir in [
        root().join("fuzz/corpus/commands_send"),
        root().join("fixtures/fuzz/commands_send"),
    ] {
        for file in files(&dir) {
            let data = std::fs::read(&file).unwrap();
            assert!(
                std::panic::catch_unwind(|| commands_checks::commands_send(&data)).is_ok(),
                "{} breaks an invariant",
                file.display()
            );
            n += 1;
        }
    }
    assert!(n > 0, "no corpus for commands_send");
    let seeds: Vec<String> = files(&root().join("fuzz/corpus/commands_send"))
        .iter()
        .filter_map(|f| f.file_name()?.to_str().map(str::to_owned))
        .collect();
    for spec in REGISTRY.iter().filter(|s| s.surfaces.slash > 0) {
        assert!(
            seeds.iter().any(|s| s == spec.name),
            "/{} has no seed in fuzz/corpus/commands_send",
            spec.name
        );
    }
}

/// Any line `send()` accepts is a command that `/help` lists, and no line makes it echo the words
/// after `/login <provider>`. Generated lines, the same check as the fuzz target; the generator is
/// a fixed LCG so a failure repeats.
#[test]
fn any_line_send_accepts_is_in_the_help_text() {
    let words: Vec<String> = REGISTRY
        .iter()
        .map(|s| format!("/{}", s.name))
        .chain(
            [
                "/",
                "//",
                "/LOGIN",
                "/login",
                "/workflow",
                "new",
                "edit",
                "save",
                "show",
                "run",
                "list",
                "--write",
                "--force",
                "--user",
                "--allow",
                "gates",
                "@lead",
                "@grep",
                "@advisor",
                "@router",
                "@agent-grep",
                "@nobody",
                "@01J",
                "review",
                "work",
                "anthropic",
                "openrouter",
                "sk-ant-api03-abcdefghijklmnop",
                "a b",
                "\u{a0}",
                "\t",
                "\n",
                "é",
                "ß",
                "x",
            ]
            .map(str::to_owned),
        )
        .collect();
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move |n: usize| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) as usize) % n
    };
    let mut accepted = 0;
    for _ in 0..4_000 {
        let line: Vec<&str> = (0..1 + next(5))
            .map(|_| words[next(words.len())].as_str())
            .collect();
        let line = line.join([" ", "  ", "\t"][next(3)]);
        let mut data = vec![(next(256)) as u8];
        data.extend(line.bytes());
        commands_checks::commands_send(&data);
        if with_env(|env| !matches!(send(&line, env), Sent::Bad(_))) {
            accepted += 1;
        }
    }
    assert!(
        accepted > 100,
        "the generator should reach accepted lines too ({accepted})"
    );
    // Every accepted slash command word is a help line: the registry's slash rows are the help.
    let help: Vec<String> = help_lines()
        .into_iter()
        .map(|(entry, _)| entry.split(' ').next().unwrap().to_owned())
        .collect();
    for spec in REGISTRY.iter().filter(|s| s.surfaces.slash > 0) {
        assert!(
            help.contains(&format!("/{}", spec.name)),
            "/{} is not in /help",
            spec.name
        );
    }
}

// ---- unexpected paths into `send` -----------------------------------------------------------

/// The prompt is the only way a slash line becomes a command. A palette pick or a card-menu pick
/// names a registry row, not a line, so `pick` with a name that is not a row does nothing, and the
/// ones that are rows never take text from the screen.
#[test]
fn pick_runs_registry_rows_only() {
    let mut out = aibus_tui::input::Out::default();
    with_env(|env| {
        for name in [
            "login",
            "logout",
            "../x",
            "/login anthropic",
            "workflow save x --force",
            "",
        ] {
            aibus_tui::commands::pick(name, aibus_tui::commands::Via::Palette, None, env, &mut out);
        }
    });
    // A pick has no arguments to give: `login` cannot start a flow for a provider and `logout` has
    // no account, so neither does anything beyond opening the list.
    assert!(out.login.is_none(), "a pick started a login");
    assert!(
        !out.msgs.iter().any(|m| matches!(
            m,
            Msg::Command(
                Command::WorkflowFile(_)
                    | Command::WorkflowAuthor { .. }
                    | Command::SetModel { .. }
            )
        )),
        "{:?}",
        out.msgs
    );
}
