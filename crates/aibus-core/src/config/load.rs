//! Reading the user config and the project overrides, merging them and checking the result.
//!
//! Each file is checked on its own first, so a mistake is reported against the file and line that
//! hold it: a syntax or type error, a `key = "..."` under `auth`, and unknown keys (warnings).
//! The merge works on the TOML tables before they become a `Config`: tables merge, arrays
//! replace, `[[account]]` comes from the user file only and `[hooks]` arrays concatenate.

use std::fmt;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use super::Config;

/// A config problem with where it is. `aibus config validate` prints it and exits 3.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigFileError {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for ConfigFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ConfigFileError {}

#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// `path:line: unknown key ...` and the like. Printed, never fatal.
    pub warnings: Vec<String>,
}

/// Parses one file: its table, plus the warnings for keys the schema does not know.
pub fn parse_file(path: &Path, text: &str) -> Result<(Table, Vec<String>), ConfigFileError> {
    let fail = |e: &toml::de::Error| ConfigFileError {
        path: path.to_owned(),
        line: e
            .span()
            .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1),
        message: e.message().to_owned(),
    };
    let table: Table = toml::from_str(text).map_err(|e| fail(&e))?;
    let mut unknown = Vec::new();
    serde_ignored::deserialize::<_, _, Config>(toml::de::Deserializer::new(text), |p| {
        unknown.push(p.to_string())
    })
    .map_err(|e| fail(&e))?;

    let mut warnings = Vec::new();
    for key in unknown {
        let line = find_line(text, &key);
        if key.ends_with("auth.key") {
            return Err(ConfigFileError {
                path: path.to_owned(),
                line,
                // The spec's exact words (07 section 3).
                message: "credentials do not go in config; run: aibus account add <name> --key-stdin"
                    .to_owned(),
            });
        }
        warnings.push(match line {
            Some(l) => format!("{}:{l}: unknown key {key}", path.display()),
            None => format!("{}: unknown key {key}", path.display()),
        });
    }
    Ok((table, warnings))
}

fn read(path: &Path) -> Result<Option<(Table, Vec<String>, String)>, ConfigFileError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ConfigFileError {
                path: path.to_owned(),
                line: None,
                message: e.to_string(),
            })
        }
    };
    let (table, warnings) = parse_file(path, &text)?;
    Ok(Some((table, warnings, text)))
}

fn deep(base: &mut Table, key: String, value: Value) {
    match (base.get_mut(&key), value) {
        (Some(Value::Table(b)), Value::Table(p)) => {
            for (k, v) in p {
                deep(b, k, v);
            }
        }
        (_, v) => {
            base.insert(key, v);
        }
    }
}

fn merge(user: Table, project: Table) -> Table {
    let mut out = user;
    for (key, value) in project {
        match (key.as_str(), value) {
            ("account", _) => {}
            ("hooks", Value::Table(hooks)) => {
                let slot = out
                    .entry("hooks")
                    .or_insert_with(|| Value::Table(Table::new()));
                if !slot.is_table() {
                    *slot = Value::Table(Table::new());
                }
                let Some(mine) = slot.as_table_mut() else {
                    continue;
                };
                for (event, list) in hooks {
                    match (mine.get_mut(&event), list) {
                        (Some(Value::Array(a)), Value::Array(b)) => a.extend(b),
                        (_, list) => {
                            mine.insert(event, list);
                        }
                    }
                }
            }
            (_, v) => deep(&mut out, key, v),
        }
    }
    out
}

/// Loads the user config and, when given, the project's `settings.toml`. A missing file is an
/// empty one.
pub fn load(user: &Path, project: Option<&Path>) -> Result<Loaded, ConfigFileError> {
    let mut warnings = Vec::new();
    let mut texts = Vec::new();
    let mut table = Table::new();
    if let Some((t, w, text)) = read(user)? {
        table = t;
        warnings.extend(w);
        texts.push((user.to_owned(), text));
    }
    if let Some(path) = project {
        if let Some((t, w, text)) = read(path)? {
            if t.contains_key("account") {
                warnings.push(format!(
                    "{}:{}: [[account]] is user-only, ignored here",
                    path.display(),
                    find_line(&text, "account").unwrap_or(1)
                ));
            }
            table = merge(table, t);
            warnings.extend(w);
            texts.push((path.to_owned(), text));
        }
    }
    let config: Config = Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| ConfigFileError {
            path: user.to_owned(),
            line: None,
            message: e.message().to_owned(),
        })?;
    if let Some((key, message)) = config.problems().into_iter().next() {
        // The project file wins a merge, so look there first.
        let found = texts
            .iter()
            .rev()
            .find_map(|(p, text)| find_line(text, &key).map(|l| (p.clone(), l)));
        return Err(ConfigFileError {
            path: found.as_ref().map_or_else(|| user.to_owned(), |f| f.0.clone()),
            line: found.map(|f| f.1),
            message: format!("{key}: {message}"),
        });
    }
    Ok(Loaded { config, warnings })
}

fn split_key(k: &str) -> Vec<String> {
    k.split('.')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .collect()
}

/// The 1-based line of a dotted key path such as `limits.max_turn` or `account.0.auth.key`
/// (array indexes are skipped). A header that names the path, or a table below it, counts, and
/// so does the line of an inline table that holds the last part.
pub fn find_line(text: &str, dotted: &str) -> Option<usize> {
    let want: Vec<String> = dotted
        .split('.')
        .filter(|s| s.parse::<usize>().is_err())
        .map(str::to_owned)
        .collect();
    // `key` followed by `=` and not the tail of a longer name.
    let has_key = |s: &str, key: &str| {
        s.match_indices(key).any(|(i, _)| {
            let before = s[..i].chars().next_back();
            let after = s[i + key.len()..].trim_start();
            before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
                && after.starts_with('=')
        })
    };
    let mut section: Vec<String> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let name = line
                .split('#')
                .next()
                .unwrap_or("")
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']');
            section = split_key(name);
            if section.starts_with(&want) {
                return Some(i + 1);
            }
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let mut full = section.clone();
        full.extend(split_key(key));
        if full == want {
            return Some(i + 1);
        }
        if want.len() > full.len()
            && want.starts_with(&full)
            && rest.contains('{')
            && want[full.len()..].iter().all(|seg| has_key(rest, seg))
        {
            return Some(i + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_MSG: &str = "credentials do not go in config; run: aibus account add <name> --key-stdin";

    fn p() -> &'static Path {
        Path::new("config.toml")
    }

    #[test]
    fn a_key_under_auth_fails_with_the_spec_message_and_its_line() {
        let text = "[[account]]\nname = \"a\"\nprovider = \"anthropic\"\nauth = { key = \"sk-x\" }\n";
        let e = parse_file(p(), text).unwrap_err();
        assert_eq!(e.message, KEY_MSG);
        assert_eq!(e.line, Some(4));
        assert_eq!(e.to_string(), format!("config.toml:4: {KEY_MSG}"));
    }

    #[test]
    fn a_key_in_an_auth_table_header_fails_too() {
        let text = "[[account]]\nname = \"a\"\nprovider = \"x\"\n[account.auth]\nkey = \"abc\"\n";
        let e = parse_file(p(), text).unwrap_err();
        assert_eq!(e.message, KEY_MSG);
        assert_eq!(e.line, Some(5));
    }

    #[test]
    fn unknown_keys_warn_with_line_numbers() {
        let text = "[limits]\nmax_turns = 5\nmax_turn = 6\n\n[bogus]\nx = 1\n";
        let (_, warnings) = parse_file(p(), text).unwrap();
        assert_eq!(
            warnings,
            ["config.toml:3: unknown key limits.max_turn", "config.toml:5: unknown key bogus"]
        );
    }

    #[test]
    fn a_syntax_error_reports_its_line() {
        let e = parse_file(p(), "[limits]\nmax_turns = \n").unwrap_err();
        assert_eq!(e.line, Some(2));
    }

    #[test]
    fn a_wrong_type_reports_its_line() {
        let e = parse_file(p(), "[ui]\nmouse = true\ntick_hz = \"fast\"\n").unwrap_err();
        assert_eq!(e.line, Some(3));
    }

    #[test]
    fn tables_merge_arrays_replace_accounts_stay_with_the_user_and_hooks_concatenate() {
        let user: Table = toml::from_str(
            r#"
            [limits]
            max_turns = 10
            max_cost_usd = 5.0
            [agents]
            extra_dirs = ["a", "b"]
            [[account]]
            name = "u"
            provider = "fake"
            [hooks]
            session_start = [{ command = "user.sh" }]
            stop = [{ command = "only-user.sh" }]
            pass_env = ["GH_TOKEN"]
            "#,
        )
        .unwrap();
        let project: Table = toml::from_str(
            r#"
            [limits]
            max_turns = 99
            [agents]
            extra_dirs = ["c"]
            [[account]]
            name = "p"
            provider = "fake"
            [hooks]
            session_start = [{ command = "project.sh" }]
            pre_tool_use = [{ command = "guard.sh" }]
            pass_env = ["NPM_TOKEN"]
            "#,
        )
        .unwrap();
        let c: Config = Value::Table(merge(user, project)).try_into().unwrap();
        assert_eq!(c.limits.max_turns, 99);
        assert_eq!(c.limits.max_cost_usd, 5.0);
        assert_eq!(c.agents.extra_dirs, ["c"]);
        assert_eq!(c.accounts.len(), 1);
        assert_eq!(c.accounts[0].name, "u");
        let cmds = |e: &str| -> Vec<&str> {
            c.hooks.events[e].iter().map(|h| h.command.as_str()).collect()
        };
        assert_eq!(c.hooks.pass_env, ["GH_TOKEN", "NPM_TOKEN"]);
        assert!(c.problems().is_empty());
        assert_eq!(cmds("session_start"), ["user.sh", "project.sh"]);
        assert_eq!(cmds("stop"), ["only-user.sh"]);
        assert_eq!(cmds("pre_tool_use"), ["guard.sh"]);
    }

    #[test]
    fn load_reads_both_files_and_warns_about_project_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config.toml");
        let project = dir.path().join("settings.toml");
        std::fs::write(&user, "[limits]\nmax_turns = 7\n").unwrap();
        std::fs::write(&project, "[[account]]\nname = \"x\"\nprovider = \"fake\"\n").unwrap();
        let l = load(&user, Some(&project)).unwrap();
        assert_eq!(l.config.limits.max_turns, 7);
        assert!(l.config.accounts.is_empty());
        assert_eq!(l.warnings.len(), 1);
        assert!(l.warnings[0].contains("settings.toml:1: [[account]] is user-only"));
    }

    #[test]
    fn missing_files_give_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let l = load(&dir.path().join("none.toml"), None).unwrap();
        assert_eq!(l.config, Config::default());
    }

    #[test]
    fn a_bad_value_is_reported_against_the_file_that_set_it() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config.toml");
        let project = dir.path().join("settings.toml");
        std::fs::write(&user, "[router]\nthreshold = 0.5\n").unwrap();
        std::fs::write(&project, "# project\n[router]\nthreshold = 7.0\n").unwrap();
        let e = load(&user, Some(&project)).unwrap_err();
        assert_eq!(e.path, project);
        assert_eq!(e.line, Some(3));
        assert!(e.message.starts_with("router.threshold:"));
    }

    #[test]
    fn find_line_handles_headers_dotted_keys_and_inline_tables() {
        let text = "a = 1\n[x.y]\nz = 2\n[[acct]]\nauth = { keyring = true }\n[w]\nq.r = 3\n";
        assert_eq!(find_line(text, "x.y.z"), Some(3));
        assert_eq!(find_line(text, "x.y"), Some(2));
        assert_eq!(find_line(text, "acct.0.auth.keyring"), Some(5));
        assert_eq!(find_line(text, "w.q.r"), Some(7));
        assert_eq!(find_line(text, "nope"), None);
    }
}
