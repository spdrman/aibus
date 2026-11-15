//! The flags a manifest may pass to each official CLI (#339). The host owns these lists: a manifest
//! names a program, and every flag in its argv, its auth argv and its optional flags has to be on
//! that program's list, with a value the list allows. A flag that is not listed is refused, whatever
//! its spelling. The denylist in [`crate::manifest`] stays as a second check over the same tokens.
//!
//! The lists are maintained per CLI version: each one is written against the flags the CLI documents
//! in the release the extension targets (`compat.cli_versions`), and a new CLI flag stays refused
//! until someone reads what it does and adds it here. A program the user added in their policy file (`extra_official_clis`) has no list,
//! so only the denylist applies to it.

use crate::manifest::ValueKind;

/// A flag, and the values it may have. An empty list means the flag is a switch or takes any value
/// that passes the denylist; otherwise the value (inline, or the next token) has to equal an entry,
/// or start with an entry that ends in `=` (a `--config key=value` setting by key).
type Allowed = &'static [(&'static str, &'static [&'static str])];

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
    ("--model", &[]),
    ("--max-turns", &[]),
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
    ("--model", &[]),
    ("-m", &[]),
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
    ("--model", &[]),
    ("-m", &[]),
    ("--approval-mode", &["default"]),
    ("--debug", &[]),
    ("-d", &[]),
];

const KIMI: Allowed = &[
    ("--print", &[]),
    ("--output-format", &["stream-json", "text"]),
    ("--input-format", &["stream-json", "text"]),
    ("--model", &[]),
    ("-m", &[]),
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

/// The kind of value the host fills for a typed flag of `cli` (schema 2, #334). Only flags listed
/// here can be typed, each with one kind, and a manifest's static argv never carries them.
pub fn typed(cli: &str, flag: &str) -> Option<ValueKind> {
    match (cli, flag) {
        ("claude" | "codex" | "gemini" | "kimi", "--model") => Some(ValueKind::ModelId),
        ("claude", "--agents") => Some(ValueKind::AgentList),
        ("codex", "--output-last-message") => Some(ValueKind::WorkdirPath),
        _ => None,
    }
}

/// Whether every flag in `tokens` is on `cli`'s list with an allowed value. A program with no list
/// passes (only the denylist applies). The error names the token and the CLI.
pub fn check<S: AsRef<str>>(cli: &str, tokens: &[S]) -> Result<(), String> {
    let Some(allowed) = list(cli) else {
        return Ok(());
    };
    for (i, t) in tokens.iter().enumerate() {
        let t = t.as_ref();
        if !t.starts_with('-') {
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
            continue;
        }
        let value = inline.or_else(|| {
            tokens
                .get(i + 1)
                .map(AsRef::as_ref)
                .filter(|n| !n.starts_with('-'))
        });
        let fine = value.is_some_and(|v| {
            values
                .iter()
                .any(|a| *a == v || (a.ends_with('=') && v.starts_with(a)))
        });
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
                if values.is_empty() {
                    tokens.push(vec![(*flag).to_owned()]);
                }
                for v in *values {
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

    #[test]
    fn a_program_with_no_list_is_left_to_the_denylist() {
        assert_eq!(check("mycli", &["--anything"]), Ok(()));
    }
}
