//! `aibus completions <shell>` (06 section 6): clap's static script, plus names that complete
//! dynamically. Agent, account, session and workflow names come from the hidden
//! `aibus <kind> list --names` helpers, which the scripts call each time.

use clap::CommandFactory;
use clap_complete::Shell as Target;

use crate::cli::{Cli, Shell};

/// Subcommands whose next word is a name, and the kind of name (the `<kind> list --names`
/// helper that lists them).
const POSITIONAL: [(&str, &[&str], &str); 4] = [
    ("agent", &["show"], "agent"),
    ("account", &["show", "test", "remove", "enable", "disable"], "account"),
    ("session", &["show", "resume", "export", "log", "replay"], "session"),
    ("workflow", &["run", "show", "validate"], "workflow"),
];

/// Global flags whose value is a name.
const FLAGS: [(&str, &str); 2] = [("agent", "agent"), ("account", "account")];

fn generated(shell: Target) -> String {
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "aibus", &mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn bash() -> String {
    let mut flags = String::new();
    for (flag, kind) in FLAGS {
        flags.push_str(&format!("        --{flag}) kind={kind} ;;\n"));
    }
    let mut pairs = String::new();
    for (sub, verbs, kind) in POSITIONAL {
        for v in verbs {
            pairs.push_str(&format!("            \"{sub} {v}\") kind={kind} ;;\n"));
        }
    }
    format!(
        r#"{}
# Names come from the hidden `aibus <kind> list --names` helpers.
_aibus_dynamic() {{
    local cur="${{COMP_WORDS[COMP_CWORD]}}" prev="" kind="" i
    local -a words=()
    (( COMP_CWORD > 0 )) && prev="${{COMP_WORDS[COMP_CWORD-1]}}"
    for (( i = 1; i < COMP_CWORD; i++ )); do
        case "${{COMP_WORDS[i]}}" in -*) ;; *) words+=("${{COMP_WORDS[i]}}") ;; esac
    done
    case "$prev" in
{flags}    esac
    if [ -z "$kind" ] && [ "${{#words[@]}}" -eq 2 ]; then
        case "${{words[0]}} ${{words[1]}}" in
{pairs}        esac
    fi
    if [ -n "$kind" ]; then
        COMPREPLY=( $(compgen -W "$(command aibus "$kind" list --names 2>/dev/null)" -- "$cur") )
        return 0
    fi
    _aibus "$@"
}}
complete -F _aibus_dynamic -o bashdefault -o default aibus
"#,
        generated(Target::Bash)
    )
}

/// clap's function is renamed `_aibus_static` and a wrapper takes the name `_aibus`.
fn zsh() -> String {
    let text = generated(Target::Zsh);
    let body = text
        .split("if [ \"$funcstack[1]\" = \"_aibus\" ]")
        .next()
        .unwrap_or_default()
        .replace("_aibus", "_aibus_static");
    let mut flags = String::new();
    for (flag, kind) in FLAGS {
        flags.push_str(&format!("        --{flag}) _aibus_names {kind}; return ;;\n"));
    }
    let mut pairs = String::new();
    for (sub, verbs, kind) in POSITIONAL {
        for v in verbs {
            pairs.push_str(&format!("            \"{sub} {v}\") _aibus_names {kind}; return ;;\n"));
        }
    }
    format!(
        r#"{body}
# Names come from the hidden `aibus <kind> list --names` helpers.
_aibus_names() {{
    local -a names
    names=(${{(f)"$(command aibus "$1" list --names 2>/dev/null)"}})
    compadd -a names
}}
_aibus() {{
    local -a plain=(${{words[2,CURRENT-1]:#-*}})
    case "$words[CURRENT-1]" in
{flags}    esac
    if (( ${{#plain}} == 2 )); then
        case "$plain[1] $plain[2]" in
{pairs}        esac
    fi
    _aibus_static "$@"
}}
if [ "$funcstack[1]" = "_aibus" ]; then
    _aibus "$@"
else
    compdef _aibus aibus
fi
"#
    )
}

fn fish() -> String {
    let mut extra = String::from("\n# Names come from the hidden `aibus <kind> list --names` helpers.\n");
    for (flag, kind) in FLAGS {
        extra.push_str(&format!(
            "complete -c aibus -l {flag} -x -a '(aibus {kind} list --names 2>/dev/null)'\n"
        ));
    }
    for (sub, verbs, kind) in POSITIONAL {
        extra.push_str(&format!(
            "complete -c aibus -n '__fish_seen_subcommand_from {sub}; and __fish_seen_subcommand_from {}' -f -a '(aibus {kind} list --names 2>/dev/null)'\n",
            verbs.join(" ")
        ));
    }
    generated(Target::Fish) + &extra
}

/// clap's completer becomes `$aibusStatic` and a second one tries names first.
fn powershell() -> String {
    let text = generated(Target::PowerShell).replace(
        "Register-ArgumentCompleter -Native -CommandName 'aibus' -ScriptBlock {",
        "$aibusStatic = {",
    );
    let mut flags = String::new();
    for (flag, kind) in FLAGS {
        flags.push_str(&format!("        '--{flag}' {{ $kind = '{kind}' }}\n"));
    }
    let mut pairs = String::new();
    for (sub, verbs, kind) in POSITIONAL {
        for v in verbs {
            pairs.push_str(&format!("            '{sub} {v}' {{ $kind = '{kind}' }}\n"));
        }
    }
    format!(
        r#"{text}
# Names come from the hidden `aibus <kind> list --names` helpers.
Register-ArgumentCompleter -Native -CommandName 'aibus' -ScriptBlock {{
    param($wordToComplete, $commandAst, $cursorPosition)
    $words = @($commandAst.CommandElements | ForEach-Object {{ $_.ToString() }})
    $done = if ($wordToComplete -eq '') {{ $words }} else {{ $words[0..($words.Count - 2)] }}
    $plain = @($done | Select-Object -Skip 1 | Where-Object {{ $_ -notlike '-*' }})
    $kind = $null
    switch ($done[-1]) {{
{flags}    }}
    if (-not $kind -and $plain.Count -eq 2) {{
        switch ($plain -join ' ') {{
{pairs}        }}
    }}
    if ($kind) {{
        & aibus $kind list --names 2>$null | Where-Object {{ $_ -like "$wordToComplete*" }} |
            ForEach-Object {{ [CompletionResult]::new($_, $_, 'ParameterValue', $_) }}
        return
    }}
    & $aibusStatic $wordToComplete $commandAst $cursorPosition
}}
"#
    )
}

pub fn script(shell: Shell) -> String {
    match shell {
        Shell::Bash => bash(),
        Shell::Zsh => zsh(),
        Shell::Fish => fish(),
        Shell::Powershell => powershell(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_completes_every_kind_of_name() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Powershell] {
            let text = script(shell);
            assert!(text.contains("list --names"), "{shell:?}");
            for (sub, verbs, _) in POSITIONAL {
                let wanted = match shell {
                    Shell::Fish => verbs.join(" "),
                    _ => format!("{sub} {}", verbs[0]),
                };
                assert!(text.contains(&wanted), "{shell:?} misses {wanted}");
            }
            for (flag, _) in FLAGS {
                assert!(text.contains(&format!("--{flag}")) || text.contains(&format!("-l {flag}")), "{shell:?} {flag}");
            }
        }
    }

    #[test]
    fn the_static_part_is_still_there() {
        assert!(script(Shell::Bash).contains("_aibus()"));
        assert!(script(Shell::Zsh).contains("_aibus_static"));
        assert!(script(Shell::Fish).contains("complete -c aibus"));
        assert!(script(Shell::Powershell).contains("$aibusStatic"));
    }

    #[test]
    fn the_helpers_exist_and_are_hidden() {
        let root = Cli::command();
        let list = |sub: &str| {
            root.find_subcommand(sub)
                .and_then(|c| c.find_subcommand("list"))
                .and_then(|l| l.get_arguments().find(|a| a.get_long() == Some("names")))
                .map(clap::Arg::is_hide_set)
        };
        for (sub, _, _) in POSITIONAL {
            assert_eq!(list(sub), Some(true), "{sub} list --names");
        }
    }
}
