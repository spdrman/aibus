//! The flags a manifest may pass to each official CLI (#339). The host owns these lists: a manifest
//! names a program, and every flag in its argv, its auth argv and its optional flags has to be on
//! that program's list, with a value the list allows. A flag that is not listed is refused, whatever
//! its spelling. The denylist in [`crate::manifest`] stays as a second check over the same tokens.
//!
//! The lists are maintained per CLI version: each one is written against the flags the CLI documents
//! in the release the extension targets (`compat.cli_versions`), and a new CLI flag stays refused
//! until someone reads what it does and adds it here. A program the user added in their policy file (`extra_official_clis`) has no list,
//! so only the denylist applies to it.

/// A flag, and the values it may have. An empty list means the flag is a switch: it takes no value
/// and nothing may be attached to it (`-pc` is refused, not read as `-p` and `-c`). `["*"]` means
/// any value that passes the denylist. Otherwise the value (inline, attached or the next token) has
/// to equal an entry, or start with an entry that ends in `=` (a `--config key=value` setting by key).
type Allowed = &'static [(&'static str, &'static [&'static str])];

const ANY: &[&str] = &["*"];

/// The words that are not flags and not a flag's value: the subcommands a manifest may name, per
/// CLI. Everything else is refused, so `claude mcp add` (which writes an MCP server into the user's
/// config), `claude update` and `codex apply` never get through. `login` and `status` are the
/// auth argv of an extension; `claude auth` is its own form of them, `codex exec` is Codex's
/// headless mode.
fn words(cli: &str) -> &'static [&'static str] {
    match cli {
        "claude" => &["auth", "login", "status"],
        "codex" => &["exec", "login", "status"],
        _ => &["login", "status"],
    }
}

const OUTPUT: &[&str] = &["stream-json", "json", "text"];
const INPUT: &[&str] = &["text", "stream-json"];
const CODEX_SANDBOX: &[&str] = &["read-only", "workspace-write"];
const CODEX_APPROVAL: &[&str] = &["untrusted"];
const CODEX_CONFIG: &[&str] = &[
    "model=",
    "model_reasoning_effort=",
    "model_reasoning_summary=",
    "model_verbosity=",
];

const CLAUDE: Allowed = &[
    ("-p", &[]),
    ("--print", &[]),
    ("--output-format", OUTPUT),
    ("--input-format", INPUT),
    ("--verbose", &[]),
    // `claude auth status --json`.
    ("--json", &[]),
    ("--include-partial-messages", &[]),
    ("--include-hook-events", &[]),
    ("--model", ANY),
    ("--max-turns", ANY),
    ("--no-session-persistence", &[]),
    // `user` only: `project` and `local` load the repository's own hooks and MCP servers.
    ("--setting-sources", &["user"]),
    ("--permission-prompts", &["none"]),
];

const CODEX: Allowed = &[
    ("--json", &[]),
    ("--skip-git-repo-check", &[]),
    ("--ephemeral", &[]),
    ("--color", &["never"]),
    ("--model", ANY),
    ("-m", ANY),
    ("--sandbox", CODEX_SANDBOX),
    ("-s", CODEX_SANDBOX),
    ("--ask-for-approval", CODEX_APPROVAL),
    ("-a", CODEX_APPROVAL),
    // Settings by key: no `profile=`, no `sandbox_workspace_write.*`, no approval or sandbox mode.
    ("--config", CODEX_CONFIG),
    ("-c", CODEX_CONFIG),
];

const GEMINI: Allowed = &[
    ("--output-format", OUTPUT),
    ("--model", ANY),
    ("-m", ANY),
    ("--approval-mode", &["default"]),
    ("--debug", &[]),
    ("-d", &[]),
];

const KIMI: Allowed = &[
    ("--print", &[]),
    ("--output-format", &["stream-json", "text"]),
    ("--input-format", &["stream-json", "text"]),
    ("--model", ANY),
    ("-m", ANY),
    ("--thinking", &[]),
    ("--no-thinking", &[]),
];

fn list(cli: &str) -> Option<Allowed> {
    match cli {
        "claude" => Some(CLAUDE),
        "codex" => Some(CODEX),
        "gemini" => Some(GEMINI),
        "kimi" => Some(KIMI),
        _ => None,
    }
}

/// Whether every token in `tokens` is on `cli`'s list: a flag with an allowed value, or one of the
/// words in [`words`]. A program with no list passes (only the denylist applies). The error names
/// the token and the CLI.
pub fn check<S: AsRef<str>>(cli: &str, tokens: &[S]) -> Result<(), String> {
    let Some(allowed) = list(cli) else {
        return Ok(());
    };
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i].as_ref();
        i += 1;
        if !t.starts_with('-') {
            if !words(cli).contains(&t) {
                return Err(format!(
                    "{t:?} is not on the allowlist of subcommands for {cli} (allowed: {:?}); the host lists what a manifest may pass to each CLI",
                    words(cli)
                ));
            }
            continue;
        }
        // `--flag`, `--flag=value`, `-f`, `-fvalue`, `-f=value`. A single-dash word is a short flag
        // with its value attached, so `-sandbox` is `-s` with the value `andbox`.
        let (flag, inline) = if let Some(long) = t.strip_prefix("--") {
            let (n, v) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            (format!("--{n}"), v)
        } else {
            let (short, rest) = t[1..].split_at(t[1..].chars().next().map_or(0, char::len_utf8));
            let rest = rest.strip_prefix('=').unwrap_or(rest);
            (format!("-{short}"), Some(rest).filter(|r| !r.is_empty()))
        };
        let Some((_, values)) = allowed.iter().find(|(f, _)| *f == flag) else {
            return Err(format!(
                "{t:?} is not on the allowlist of flags for {cli}; the host lists what a manifest may pass to each CLI and this flag is not there"
            ));
        };
        if values.is_empty() {
            if inline.is_some() {
                return Err(format!(
                    "{t:?}: {flag} is a switch on {cli} and takes nothing attached (grouped short switches are not read)"
                ));
            }
            continue;
        }
        let value = inline.or_else(|| {
            let next = tokens
                .get(i)
                .map(AsRef::as_ref)
                .filter(|n| !n.starts_with('-'));
            // The next word is this flag's value, so it is not read again as a subcommand.
            i += usize::from(next.is_some());
            next
        });
        let fine = match value {
            None => *values == ANY,
            Some(v) => values
                .iter()
                .any(|a| *a == "*" || *a == v || (a.ends_with('=') && v.starts_with(a))),
        };
        if !fine {
            return Err(format!(
                "{t:?}: the value {:?} is not on the allowlist for {flag} on {cli} (allowed: {values:?})",
                value.unwrap_or("none")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::OFFICIAL_CLIS;

    /// The two checks agree: nothing the list allows is something the denylist refuses, or a
    /// manifest could never use it.
    #[test]
    fn every_listed_flag_and_value_passes_the_denylist() {
        for cli in OFFICIAL_CLIS {
            for (flag, values) in list(cli).unwrap() {
                let mut tokens = Vec::new();
                if values.is_empty() || *values == ANY {
                    tokens.push(vec![(*flag).to_owned()]);
                }
                for v in *values {
                    let v = if *v == "*" { &"x" } else { v };
                    let v = if v.ends_with('=') {
                        format!("{v}x")
                    } else {
                        (*v).to_owned()
                    };
                    tokens.push(vec![(*flag).to_owned(), v.clone()]);
                    if flag.starts_with("--") {
                        tokens.push(vec![format!("{flag}={v}")]);
                    }
                }
                for t in tokens {
                    assert_eq!(check(cli, &t), Ok(()), "{cli} {t:?}");
                    assert_eq!(crate::manifest::unattended(&t), None, "{cli} {t:?}");
                }
            }
        }
    }

    #[test]
    fn every_official_cli_has_a_list() {
        for cli in OFFICIAL_CLIS {
            assert!(list(cli).is_some(), "{cli}");
        }
    }

    /// Test gaps the review found in the allowlist on its own (the denylist behind it hides them).
    #[test]
    fn a_value_attached_to_a_short_flag_is_checked_and_the_next_word_has_to_be_a_value() {
        assert_eq!(check("codex", &["-sread-only"]), Ok(()));
        assert!(check("codex", &["-sdanger-full-access"]).is_err());
        assert!(check("codex", &["-s=danger-full-access"]).is_err());
        assert!(check("codex", &["--sandbox", "--json"]).is_err());
        assert!(check("codex", &["-s", "-m", "x"]).is_err());
    }

    #[test]
    fn a_grouped_short_switch_and_a_switch_with_a_value_are_refused() {
        assert!(check("gemini", &["-dy"]).is_err());
        assert!(check("gemini", &["-d"]).is_ok());
        assert!(check("claude", &["-pc"]).is_err());
        assert!(check("claude", &["-p"]).is_ok());
        assert!(check("claude", &["--verbose=true"]).is_err());
        assert!(check("claude", &["--print=x"]).is_err());
    }

    #[test]
    fn subcommands_are_on_the_list_too() {
        for (cli, argv) in [
            ("claude", &["mcp", "add", "evil", "npx", "evilpkg"][..]),
            ("claude", &["update"][..]),
            ("claude", &["-p", "mcp"][..]),
            ("codex", &["apply", "task1"][..]),
            (
                "gemini",
                &["--output-format", "json", "mcp", "add", "x"][..],
            ),
            ("kimi", &["--print", "web"][..]),
        ] {
            assert!(check(cli, argv).is_err(), "{cli} {argv:?}");
        }
        for (cli, argv) in [
            ("claude", &["auth", "status", "--json"][..]),
            ("claude", &["auth", "login"][..]),
            ("codex", &["exec", "--json", "--model", "gpt-5"][..]),
            ("gemini", &["--model", "x", "login"][..]),
        ] {
            assert_eq!(check(cli, argv), Ok(()), "{cli} {argv:?}");
        }
        // A value is not a subcommand, and a flag that takes none leaves the next word a subcommand.
        assert_eq!(check("claude", &["--output-format", "json"]), Ok(()));
        assert!(check("claude", &["--verbose", "update"]).is_err());
    }

    #[test]
    fn a_program_with_no_list_is_left_to_the_denylist() {
        assert_eq!(check("mycli", &["--anything"]), Ok(()));
    }
}
