//! The body of the `commands_send` fuzz target (G9): `aibus_tui::commands::send` over a random
//! line and a random session. It lives apart from `checks.rs` because the dashboard crate is not
//! a dependency of `aibus-core`'s tests. `fuzz/fuzz_targets/commands_send.rs` calls it with the
//! bytes libFuzzer made up, and `crates/aibus-tui/tests/security_commands.rs` calls it with every
//! corpus file, every crash fixture and a few thousand generated lines, on every push.
//!
//! Input: the first byte picks the session (see `session`), the rest is the line, as UTF-8.

use aibus_core::{Msg, RunId, RunStatus};
use aibus_tui::commands::{help_lines, send, Env};
use aibus_tui::prompt::Sent;

const RUNS: [(&str, &str); 3] = [
    ("01J00000000000000000000001", "lead"),
    ("01J00000000000000000000002", "grep"),
    ("01J00000000000000000000003", "review"),
];

/// A panic means the parser broke; an assert means it accepted something it must not.
pub fn commands_send(data: &[u8]) {
    let Some((&flags, rest)) = data.split_first() else {
        return;
    };
    let Ok(line) = std::str::from_utf8(rest) else {
        return;
    };

    let ids: Vec<RunId> = RUNS
        .iter()
        .map(|(id, _)| id.parse().expect("a fixed run id"))
        .collect();
    let with_runs = flags & 0b0000_0010 != 0;
    let runs: Vec<(RunId, String)> = if with_runs {
        ids.iter()
            .zip(RUNS)
            .map(|(id, (_, a))| (*id, a.to_owned()))
            .collect()
    } else {
        Vec::new()
    };
    let state = match (flags >> 5) & 0b11 {
        0 => RunStatus::Running,
        1 => RunStatus::Failed,
        2 => RunStatus::Completed,
        _ => RunStatus::Partial,
    };
    let states: Vec<(RunId, RunStatus)> = if with_runs {
        vec![
            (ids[0], RunStatus::Running),
            (ids[1], state),
            (ids[2], state),
        ]
    } else {
        Vec::new()
    };
    let agents = vec![
        "grep".to_owned(),
        "review".to_owned(),
        "workflow-author".to_owned(),
    ];
    let workflows: Vec<String> = if flags & 0b0000_1000 != 0 {
        ["fix", "build", "review", "explore"]
            .map(str::to_owned)
            .to_vec()
    } else {
        Vec::new()
    };
    let accounts = if flags & 0b0000_0100 != 0 {
        vec!["work".to_owned()]
    } else {
        Vec::new()
    };
    let env = Env {
        agents: &agents,
        workflows: &workflows,
        lead: (flags & 0b0000_0001 != 0).then_some(ids[0]),
        focused: (flags & 0b0001_0000 != 0 && with_runs).then_some(ids[1]),
        runs: &runs,
        states: &states,
        accounts: true,
        mcp: flags & 0b1000_0000 != 0,
        account_names: &accounts,
    };

    let sent = send(line, &env);
    let trimmed = line.trim();
    let word = trimmed
        .strip_prefix('/')
        .map(|r| r.split_whitespace().next().unwrap_or(""));
    let Some(word) = word else {
        // Not a slash line: one prompt, with the text as it is, and nothing else.
        let Sent::Msgs(msgs) = sent else {
            panic!("a plain line is a prompt: {line:?}")
        };
        assert!(
            matches!(&msgs[..], [Msg::SubmitPrompt { text }] if text == trimmed),
            "{msgs:?}"
        );
        return;
    };

    // The words after `/login <provider>` could be a key: they are never echoed and never become a
    // message, whatever else the line says.
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let key_words: Vec<&str> = if words.first() == Some(&"/login") {
        words.iter().skip(2).copied().collect()
    } else {
        Vec::new()
    };
    let echoes = |text: &str| {
        let template =
            "[redacted]: type the key in the masked field after the name, never on the line";
        key_words.iter().any(|w| {
            w.len() >= 8 && !template.contains(w) && !words[1].contains(w) && text.contains(w)
        })
    };

    match sent {
        Sent::Bad(why) => assert!(
            !echoes(&why),
            "a refusal repeats the words after the provider: {why}"
        ),
        Sent::Msgs(msgs) | Sent::Login(_, msgs) => {
            // Accepted: the command is one that /help lists, and a prompt only for the two the
            // reducer reads from a prompt.
            let name = format!("/{word}");
            assert!(
                help_lines()
                    .iter()
                    .any(|(entry, _)| entry.split(' ').next() == Some(name.as_str())),
                "{name} is accepted and not in /help"
            );
            for m in &msgs {
                if matches!(m, Msg::SubmitPrompt { .. }) {
                    assert!(
                        matches!(word, "fork" | "compact"),
                        "/{word} became a prompt: {m:?}"
                    );
                }
            }
            assert!(
                !echoes(&format!("{msgs:?}")),
                "{msgs:?} carries the words after the provider"
            );
            assert!(
                key_words.is_empty() || words.len() == 2,
                "a /login with a key was accepted: {line:?}"
            );
        }
    }
}
