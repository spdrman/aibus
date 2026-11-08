//! Shell command analysis for `bash` (09 sections 2 and 3). An allow-list glob such as
//! `bash(git *)` matches the whole command string, so `git status; rm x` would pass it. Every
//! command is therefore split into the simple commands the shell would run, and a call is
//! admitted only when every part is. Anything the splitter cannot be sure about is refused:
//! the cost of a false refusal is a re-worded command, the cost of a false pass is a hole.

use globset::GlobBuilder;

use super::policy::normalise_command;

/// Assignments that change which program runs or how the shell behaves. `normalise_command`
/// strips leading assignments before matching, so `PATH=/tmp/evil git log` would otherwise
/// look like plain `git log`.
const DANGEROUS_ASSIGNMENTS: [&str; 12] = [
    "PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "BASH_ENV",
    "ENV",
    "IFS",
    "SHELLOPTS",
    "PROMPT_COMMAND",
    "PS4",
    "CDPATH",
];

/// Splits `cmd` into the simple commands it contains: pieces between `;`, `&`, `&&`, `|`,
/// `||`, `|&` and newlines, the bodies of subshells `( )`, command substitutions `$( )` and
/// backticks, and process substitutions `<( )`, `>( )`. A part that holds a substitution keeps
/// its text, and the substitution's own commands follow as parts of their own. Each part is
/// normalised the way `bash(...)` specifiers expect (assignments dropped, whitespace
/// collapsed).
///
/// `Err` carries the reason when the text is ambiguous: an unterminated quote or bracket, a
/// heredoc, arithmetic or `$'...'` quoting, a nested backtick, a command word that is itself an
/// expansion, or an assignment to something like `PATH`.
pub(crate) fn split(cmd: &str) -> Result<Vec<String>, String> {
    if cmd.contains('\0') {
        return Err("command contains a NUL byte".into());
    }
    // `normalise_command` splits words on any Unicode whitespace, the shell only on blanks, so
    // a no-break space would let `git\u{a0}status` read as two words to the matcher and one to `sh`.
    if cmd.chars().any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n')) {
        return Err("unusual whitespace".into());
    }
    let chars: Vec<char> = cmd.chars().collect();
    let mut raw = Vec::new();
    let mut i = 0;
    scan(&chars, &mut i, Close::End, &mut raw)?;

    let mut parts = Vec::with_capacity(raw.len());
    for part in raw {
        // Leading assignments are dropped by `normalise_command`, so look at them first.
        let mut rest = part.trim_start();
        while let Some(word) = rest.split_whitespace().next() {
            let Some((name, _)) = word.split_once('=') else {
                break;
            };
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                break;
            }
            if DANGEROUS_ASSIGNMENTS.contains(&name) {
                return Err(format!("assignment to {name}"));
            }
            // `normalise_command` ends an assignment at the first space, which a substitution
            // can contain, so what follows would be misread as the command.
            if word.contains("$(") || word.contains('`') {
                return Err(format!("assignment to {name} holds a substitution"));
            }
            rest = rest.trim_start()[word.len()..].trim_start();
        }
        let norm = normalise_command(&part);
        let word = norm.split(' ').next().unwrap_or("");
        if word.is_empty() {
            continue;
        }
        if word.contains('$') {
            return Err(format!("command word `{word}` is an expansion"));
        }
        parts.push(norm);
    }
    Ok(parts)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    End,
    Paren,
    Backtick,
}

/// Reads commands from `chars[*i..]` up to `close`, pushing each finished part to `out`. The
/// closing character is consumed.
fn scan(chars: &[char], i: &mut usize, close: Close, out: &mut Vec<String>) -> Result<(), String> {
    let mut cur = String::new();
    // The previous character was an unquoted `<` or `>`, so `|`, `&` and `(` after it belong
    // to a redirection or a process substitution instead of ending the command.
    let mut last_redir = false;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.trim().is_empty() {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    // A nested region is scanned for its own parts, and its text stays in the part around it.
    let nested = |chars: &[char], i: &mut usize, close: Close, cur: &mut String, out: &mut Vec<String>| {
        let start = *i;
        scan(chars, i, close, out)?;
        cur.extend(&chars[start..*i]);
        Ok::<(), String>(())
    };

    while *i < chars.len() {
        let c = chars[*i];
        let next = chars.get(*i + 1).copied();
        let prev_redir = std::mem::replace(&mut last_redir, false);
        match c {
            '\\' => match next {
                None => return Err("trailing backslash".into()),
                Some('`') if close == Close::Backtick => return Err("nested backticks".into()),
                Some('\n') => *i += 2,
                Some(n) => {
                    cur.push(c);
                    cur.push(n);
                    *i += 2;
                }
            },
            '\'' => {
                let end = chars[*i + 1..]
                    .iter()
                    .position(|&q| q == '\'')
                    .ok_or("unterminated single quote")?;
                cur.extend(&chars[*i..=*i + 1 + end]);
                *i += end + 2;
            }
            '"' => {
                cur.push(c);
                *i += 1;
                loop {
                    let Some(&q) = chars.get(*i) else {
                        return Err("unterminated double quote".into());
                    };
                    match q {
                        '"' => {
                            cur.push(q);
                            *i += 1;
                            break;
                        }
                        '\\' => {
                            let n = chars.get(*i + 1).ok_or("trailing backslash")?;
                            if *n == '`' && close == Close::Backtick {
                                return Err("nested backticks".into());
                            }
                            cur.push(q);
                            cur.push(*n);
                            *i += 2;
                        }
                        '`' => {
                            cur.push(q);
                            *i += 1;
                            nested(chars, i, Close::Backtick, &mut cur, out)?;
                        }
                        '$' if chars.get(*i + 1) == Some(&'(') => {
                            dollar_paren(chars, i, &mut cur, out, &nested)?;
                        }
                        _ => {
                            cur.push(q);
                            *i += 1;
                        }
                    }
                }
            }
            '`' => {
                if close == Close::Backtick {
                    *i += 1;
                    flush(&mut cur, out);
                    return Ok(());
                }
                cur.push(c);
                *i += 1;
                nested(chars, i, Close::Backtick, &mut cur, out)?;
            }
            '$' => match next {
                Some('(') => dollar_paren(chars, i, &mut cur, out, &nested)?,
                Some('\'') => return Err("$'...' quoting".into()),
                _ => {
                    cur.push(c);
                    *i += 1;
                }
            },
            '(' => {
                // `<( )` and `>( )` are process substitutions and stay inside their part. A
                // bare `(` opens a subshell, which is a command boundary.
                if prev_redir {
                    cur.push(c);
                    *i += 1;
                    nested(chars, i, Close::Paren, &mut cur, out)?;
                } else {
                    flush(&mut cur, out);
                    *i += 1;
                    scan(chars, i, Close::Paren, out)?;
                }
            }
            ')' => {
                if close == Close::Paren {
                    *i += 1;
                    flush(&mut cur, out);
                    return Ok(());
                }
                return Err("unbalanced `)`".into());
            }
            '<' if next == Some('<') => {
                if chars.get(*i + 2) == Some(&'<') {
                    cur.push_str("<<<");
                    *i += 3;
                } else {
                    return Err("heredoc".into());
                }
            }
            ';' | '\n' => {
                flush(&mut cur, out);
                *i += 1;
            }
            '<' | '>' => {
                cur.push(c);
                *i += 1;
                last_redir = true;
            }
            '|' if prev_redir && cur.ends_with('>') => {
                // `>|` is a redirection.
                cur.push(c);
                *i += 1;
            }
            '|' => {
                flush(&mut cur, out);
                *i += if matches!(next, Some('|' | '&')) { 2 } else { 1 };
            }
            '&' if prev_redir || next == Some('>') => {
                // `2>&1`, `<&3` and `&>file` are redirections.
                cur.push(c);
                *i += 1;
            }
            '&' => {
                flush(&mut cur, out);
                *i += if next == Some('&') { 2 } else { 1 };
            }
            _ => {
                cur.push(c);
                *i += 1;
            }
        }
    }
    if close != Close::End {
        return Err("unterminated substitution or subshell".into());
    }
    flush(&mut cur, out);
    Ok(())
}

/// At `$(`: arithmetic `$((` is refused, anything else is a nested command list.
fn dollar_paren(
    chars: &[char],
    i: &mut usize,
    cur: &mut String,
    out: &mut Vec<String>,
    nested: &impl Fn(&[char], &mut usize, Close, &mut String, &mut Vec<String>) -> Result<(), String>,
) -> Result<(), String> {
    if chars.get(*i + 2) == Some(&'(') {
        return Err("arithmetic expansion".into());
    }
    cur.push_str("$(");
    *i += 2;
    nested(chars, i, Close::Paren, cur, out)
}

/// A glob over the normalised command, with the name it is logged under.
#[derive(Clone, Debug)]
struct DenyRule {
    name: String,
    glob: globset::GlobMatcher,
}

/// The destructive-command list (09 section 3): `tools/shell_deny.txt` plus
/// `[tools] deny_patterns`. A match is refused before any prompt, in every mode.
#[derive(Clone, Debug)]
pub struct ShellDeny {
    rules: Vec<DenyRule>,
}

/// The list the spec names, shipped with the binary.
const BUILTIN: &str = include_str!("../../../../tools/shell_deny.txt");

impl ShellDeny {
    /// The built-in list followed by `extra`. A line is `name: glob`; a bare glob in `extra`
    /// is named after itself. `#` lines and blank lines are skipped, and an invalid glob is an
    /// error so a typo cannot silently weaken the list.
    pub fn new(extra: &[String]) -> Result<Self, String> {
        let mut rules = Vec::new();
        for line in BUILTIN.lines().chain(extra.iter().map(String::as_str)) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, glob) = line.split_once(": ").unwrap_or((line, line));
            let (name, glob) = (name.trim(), glob.trim());
            let matcher = GlobBuilder::new(glob)
                .literal_separator(false)
                .build()
                .map_err(|e| format!("deny pattern `{glob}`: {e}"))?
                .compile_matcher();
            rules.push(DenyRule {
                name: name.to_owned(),
                glob: matcher,
            });
        }
        Ok(Self { rules })
    }

    /// The name of the first rule `text` trips, tried against the whole text and against every
    /// word-boundary suffix of it, so `sudo rm -rf /` and `xargs rm -rf /` are caught as well.
    pub fn hit(&self, text: &str) -> Option<&str> {
        let text = text.trim();
        let starts = std::iter::once(0).chain(
            text.char_indices()
                .filter(|(_, c)| c.is_whitespace())
                .map(|(i, _)| i + 1),
        );
        for start in starts {
            let tail = &text[start..];
            if let Some(rule) = self.rules.iter().find(|r| r.glob.is_match(tail)) {
                return Some(&rule.name);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    fn parts(cmd: &str) -> Vec<String> {
        sorted(split(cmd).unwrap_or_else(|e| panic!("{cmd:?} should parse, got: {e}")))
    }

    #[test]
    fn simple_commands_and_every_separator_split() {
        // (command, the simple commands it contains, in any order)
        let table: &[(&str, &[&str])] = &[
            ("git status", &["git status"]),
            ("  git   status ", &["git status"]),
            ("A=1 B='x y' git log", &["git log"]),
            ("git status; git log", &["git log", "git status"]),
            ("git status;", &["git status"]),
            ("git status;; git log", &["git log", "git status"]),
            ("git status && git log || git diff", &["git diff", "git log", "git status"]),
            ("git status | grep x", &["git status", "grep x"]),
            ("git status |& grep x", &["git status", "grep x"]),
            ("git status & git log", &["git log", "git status"]),
            ("git status &\ngit log", &["git log", "git status"]),
            ("git status\ngit log", &["git log", "git status"]),
            ("git status\n\n\ngit log\n", &["git log", "git status"]),
            ("", &[]),
            ("  ;; \n ", &[]),
            // Substitutions and subshells: the inner commands are parts of their own, and the
            // outer part keeps its text.
            (
                "echo $(git rev-parse HEAD)",
                &["echo $(git rev-parse HEAD)", "git rev-parse HEAD"],
            ),
            (
                "echo \"$(git rev-parse HEAD)\"",
                &["echo \"$(git rev-parse HEAD)\"", "git rev-parse HEAD"],
            ),
            (
                "echo `git rev-parse HEAD`",
                &["echo `git rev-parse HEAD`", "git rev-parse HEAD"],
            ),
            (
                "echo \"`git rev-parse HEAD`\"",
                &["echo \"`git rev-parse HEAD`\"", "git rev-parse HEAD"],
            ),
            (
                "echo $(echo $(git log))",
                &["echo $(echo $(git log))", "echo $(git log)", "git log"],
            ),
            ("(cd sub && make)", &["cd sub", "make"]),
            ("( git status )", &["git status"]),
            ("echo a; (git status; git log)", &["echo a", "git log", "git status"]),
            ("echo <(git diff)", &["echo <(git diff)", "git diff"]),
            ("diff <(git show a) <(git show b)", &["diff <(git show a) <(git show b)", "git show a", "git show b"]),
            ("tee >(grep x) < in", &["grep x", "tee >(grep x) < in"]),
            // Quoting keeps separators inside a word.
            ("echo 'a; b'", &["echo 'a; b'"]),
            ("echo \"a; b | c && d\"", &["echo \"a; b | c && d\""]),
            ("echo '$(touch x)'", &["echo '$(touch x)'"]),
            ("echo a\\;b", &["echo a\\;b"]),
            ("echo \"a\nb\"", &["echo \"a b\""]),
            ("echo \\\"; git log", &["echo \\\"", "git log"]),
            ("echo a \\\n b", &["echo a b"]),
            // Redirections are not separators.
            ("git log 2>&1", &["git log 2>&1"]),
            ("git log &> out", &["git log &> out"]),
            ("git log >| out", &["git log >| out"]),
            ("git log > out 2>&1", &["git log > out 2>&1"]),
            ("cat <<< hi", &["cat <<< hi"]),
            ("echo $HOME ${HOME}", &["echo $HOME ${HOME}"]),
            // An escaped `$` followed by a bare paren is not a substitution, but the paren
            // still opens a subshell, so the result is the cautious reading.
            ("echo \\$(touch x)", &["echo \\$", "touch x"]),
        ];
        for (cmd, want) in table {
            let want: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(parts(cmd), sorted(want), "{cmd:?}");
        }
    }

    #[test]
    fn ambiguous_text_is_refused() {
        let table = [
            ("echo 'a", "unterminated single quote"),
            ("echo \"a", "unterminated double quote"),
            ("echo $(a", "unterminated"),
            ("echo `a", "unterminated"),
            ("(a", "unterminated"),
            ("echo \"$(a\"", "unterminated"),
            ("echo a)", "unbalanced"),
            ("echo a\\", "trailing backslash"),
            ("cat <<EOF\nrm x\nEOF", "heredoc"),
            ("cat <<-EOF\nrm x\nEOF", "heredoc"),
            ("cat << EOF", "heredoc"),
            ("echo $((1+1))", "arithmetic"),
            ("echo $((1+$(rm x)))", "arithmetic"),
            ("echo $'\\x27'; rm x", "quoting"),
            ("echo `echo \\`rm x\\``", "nested backticks"),
            ("echo \"`echo \\`rm x\\``\"", "nested backticks"),
            ("$CMD x", "expansion"),
            ("\"$CMD\" x", "expansion"),
            ("A=1 ${CMD} x", "expansion"),
            ("VAR=$(git log) env", "substitution"),
            ("VAR=`git log` env", "substitution"),
            ("VAR=\"$(git log)\" env", "substitution"),
            ("git status; $X", "expansion"),
            ("PATH=/tmp/evil git log", "assignment to PATH"),
            ("A=1 LD_PRELOAD=/x.so ls", "assignment to LD_PRELOAD"),
            ("BASH_ENV=/x ls", "assignment to BASH_ENV"),
            ("IFS=x ls", "assignment to IFS"),
            ("echo a\0; rm x", "NUL"),
            ("git\u{a0}status", "whitespace"),
            ("echo a\u{2003}b", "whitespace"),
            ("echo a\r\nrm x", "whitespace"),
            ("echo a\x0b; rm x", "whitespace"),
        ];
        for (cmd, why) in table {
            match split(cmd) {
                Ok(p) => panic!("{cmd:?} should be refused, got {p:?}"),
                Err(e) => assert!(e.contains(why), "{cmd:?}: {e:?} does not mention {why:?}"),
            }
        }
    }

    #[test]
    fn the_destructive_list_matches_the_09_examples() {
        let deny = ShellDeny::new(&[]).unwrap();
        let hit = [
            ("rm -rf /", "rm-root"),
            ("rm -rf /*", "rm-root-glob"),
            ("rm -fr /", "rm-root"),
            ("rm -rf --no-preserve-root /", "rm-root"),
            ("sudo rm -rf /", "rm-root"),
            ("echo hi && rm -rf /", "rm-root"),
            ("rm -rf ~", "rm-home"),
            ("rm -rf ~/", "rm-home-slash"),
            ("rm -rf $HOME", "rm-home-var"),
            ("rm -rf *", "rm-star"),
            ("rm -fr *", "rm-star"),
            ("rm -r -f *", "rm-star"),
            ("git push --force origin main", "force-push-main"),
            ("git push -f origin master", "force-push-short-master"),
            ("git push origin main --force", "force-push-flag-last-main"),
            ("git push origin +main", "force-push-plus-main"),
            ("git push --force-with-lease origin main", "force-push-main"),
            ("git reset --hard", "reset-hard-no-ref"),
            (":(){ :|:& };:", "fork-bomb"),
            ("mkfs", "mkfs"),
            ("mkfs /dev/sda", "mkfs-args"),
            ("mkfs.ext4 /dev/sda1", "mkfs-fs"),
            ("dd if=/dev/zero of=/dev/sda", "dd-to-device-args"),
            ("dd of=/dev/sda", "dd-to-device"),
        ];
        for (cmd, rule) in hit {
            assert_eq!(deny.hit(cmd), Some(rule), "{cmd:?}");
        }
        let fine = [
            "rm -rf build",
            "rm -rf ./target",
            "rm file.txt",
            "rm -rf /tmp/x/",
            "git push origin feature --force",
            "git push origin main",
            "git reset --hard HEAD~1",
            "git reset --hard origin/main",
            "dd if=a of=b",
            "ls /",
        ];
        for cmd in fine {
            assert_eq!(deny.hit(cmd), None, "{cmd:?}");
        }
    }

    #[test]
    fn deny_patterns_extend_the_list_and_bad_globs_are_errors() {
        let deny = ShellDeny::new(&["no-curl: curl *".into(), "wget *".into()]).unwrap();
        assert_eq!(deny.hit("curl http://x"), Some("no-curl"));
        assert_eq!(deny.hit("sudo wget http://x"), Some("wget *"));
        assert_eq!(deny.hit("rm -rf /"), Some("rm-root"));
        assert_eq!(deny.hit("echo curl"), None);
        assert!(ShellDeny::new(&["bad: [".into()]).is_err());
    }
}
