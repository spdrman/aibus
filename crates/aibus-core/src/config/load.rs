//! Reading the user config and the project overrides, merging them and checking the result.
//!
//! Each file is checked on its own first, so a mistake is reported against the file and line that
//! hold it: a syntax or type error, a `key = "..."` under `auth`, and unknown keys (warnings).
//! The merge works on the TOML tables before they become a `Config`: tables merge, arrays
//! replace, `[[account]]` and `[[provider]]` come from the user file only and `[hooks]` arrays concatenate.

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
                message:
                    "credentials do not go in config; run: aibus account add <name> --key-stdin"
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
            ("account" | "provider", _) => {}
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
            for table in ["account", "provider"] {
                if t.contains_key(table) {
                    warnings.push(format!(
                        "{}:{}: [[{table}]] is user-only, ignored here",
                        path.display(),
                        find_line(&text, table).unwrap_or(1)
                    ));
                }
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
            path: found
                .as_ref()
                .map_or_else(|| user.to_owned(), |f| f.0.clone()),
            line: found.map(|f| f.1),
            message: format!("{key}: {message}"),
        });
    }
    // A key sent over plain http to an address that is neither this machine nor a private
    // network can be read on the way. A warning, not an error: a lab VPN is a legitimate case.
    let user_text = texts
        .first()
        .filter(|(p, _)| p == user)
        .map(|(p, t)| (p.as_path(), t.as_str()));
    let urls = config
        .providers
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            Some((
                format!("provider.{i}.base_url"),
                d.id.as_str(),
                d.base_url.as_deref()?,
            ))
        })
        .chain(config.accounts.iter().enumerate().filter_map(|(i, a)| {
            Some((
                format!("account.{i}.base_url"),
                a.name.as_str(),
                a.base_url.as_deref()?,
            ))
        }));
    for (key, who, url) in urls.filter(|(_, _, url)| is_open_http(url)) {
        let at = user_text.map_or(String::new(), |(path, text)| {
            format!(
                "{}:{}: ",
                path.display(),
                find_line(text, &key).unwrap_or(1)
            )
        });
        warnings.push(format!(
            "{at}{who}: base_url {url} is plain http to an address that is not loopback or a private network, so a key sent there can be read on the way"
        ));
    }
    Ok(Loaded { config, warnings })
}

/// `http://` to a host that is not `localhost`, a loopback address or an RFC 1918 address. A
/// name other than `localhost` counts as public, since it could resolve anywhere.
fn is_open_http(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    if host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !(ip.is_loopback() || ip.is_private()),
        Ok(std::net::IpAddr::V6(ip)) => !ip.is_loopback(),
        Err(_) => true,
    }
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
    // An index after an array of tables (`account.1.fallback`) picks that table's block, so the
    // line is the second `[[account]]`'s and not the first one that has the key.
    let segs: Vec<&str> = dotted.split('.').collect();
    if let Some(at) = segs
        .iter()
        .position(|s| s.parse::<usize>().is_ok())
        .filter(|at| *at > 0)
    {
        let (head, n) = (segs[..at].join("."), segs[at].parse::<usize>().unwrap_or(0));
        let header = format!("[[{head}]]");
        let lines: Vec<&str> = text.lines().collect();
        let starts: Vec<usize> = (0..lines.len())
            .filter(|i| lines[*i].trim() == header)
            .collect();
        if let Some(&from) = starts.get(n) {
            let to = starts.get(n + 1).copied().unwrap_or(lines.len());
            let block = lines[from..to].join("\n");
            let rest = std::iter::once(head.as_str())
                .chain(segs[at + 1..].iter().copied())
                .collect::<Vec<_>>()
                .join(".");
            return find_line_flat(&block, &rest).map(|l| from + l);
        }
    }
    find_line_flat(text, dotted)
}

fn find_line_flat(text: &str, dotted: &str) -> Option<usize> {
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

    const KEY_MSG: &str =
        "credentials do not go in config; run: aibus account add <name> --key-stdin";

    fn p() -> &'static Path {
        Path::new("config.toml")
    }

    #[test]
    fn a_key_under_auth_fails_with_the_spec_message_and_its_line() {
        let text =
            "[[account]]\nname = \"a\"\nprovider = \"anthropic\"\nauth = { key = \"sk-x\" }\n";
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
            [
                "config.toml:3: unknown key limits.max_turn",
                "config.toml:5: unknown key bogus"
            ]
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
            c.hooks.events[e]
                .iter()
                .map(|h| h.command.as_str())
                .collect()
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

    #[test]
    fn plain_http_to_a_public_address_warns_with_its_line_and_loopback_and_private_do_not() {
        let provider = |url: &str| {
            format!("[[provider]]\nid = \"p\"\napi = \"openai-chat\"\nbase_url = \"{url}\"\n")
        };
        for ok in [
            "https://api.example.com/v1",
            "http://127.0.0.1:11434/v1",
            "http://localhost:8000/v1",
            "http://[::1]:8000/v1",
            "http://10.0.0.5:8000/v1",
            "http://172.16.3.4/v1",
            "http://192.168.1.20:8080/v1",
        ] {
            assert!(
                load_text(&provider(ok)).unwrap().warnings.is_empty(),
                "{ok}"
            );
        }
        for bad in [
            "http://203.0.113.9/v1",
            "http://api.example.com/v1",
            "http://172.32.0.1/v1",
            "http://user@8.8.8.8/v1",
        ] {
            let w = load_text(&provider(bad)).unwrap().warnings;
            assert_eq!(w.len(), 1, "{bad}: {w:?}");
            assert!(
                w[0].contains(":4: p: base_url") && w[0].contains("plain http"),
                "{}",
                w[0]
            );
        }
        // The old account-level form is checked too.
        let w = load_text("[[account]]\nname = \"old\"\nprovider = \"openai-compatible\"\nbase_url = \"http://203.0.113.9/v1\"\n").unwrap().warnings;
        assert!(w.len() == 1 && w[0].contains(":4: old: base_url"), "{w:?}");
    }

    #[test]
    fn an_index_picks_the_nth_table_of_an_array() {
        let text = "[[account]]\nname = \"a\"\nfallback = [\"b\"]\n[[account]]\nname = \"b\"\nfallback = [\"zz\"]\n";
        assert_eq!(find_line(text, "account.0.fallback"), Some(3));
        assert_eq!(find_line(text, "account.1.fallback"), Some(6));
    }

    fn fallback_config(second: &str) -> String {
        format!("[[account]]\nname = \"a\"\nprovider = \"fake\"\nfallback = [\"b\"]\n[[account]]\nname = \"b\"\nprovider = \"fake\"\n{second}\n")
    }

    #[test]
    fn fallback_and_fallback_on_parse_with_their_defaults() {
        use super::super::FallbackKind::*;
        let c = load_text(&fallback_config("")).unwrap().config;
        assert_eq!(c.accounts[0].fallback, ["b"]);
        assert!(c.accounts[1].fallback.is_empty());
        assert_eq!(c.defaults.fallback_on, [Quota, Auth, Overloaded]);
        let c = load_text("[defaults]\nfallback_on = [\"quota\", \"rate_limit\"]\n")
            .unwrap()
            .config;
        assert_eq!(c.defaults.fallback_on, [Quota, RateLimit]);
    }

    #[test]
    fn a_fallback_that_names_no_account_or_itself_is_a_config_error_with_its_line() {
        let e = load_text(&fallback_config("fallback = [\"nope\"]")).unwrap_err();
        assert_eq!(e.line, Some(8));
        assert!(e.message.contains("nope"), "{}", e.message);
        let e = load_text(&fallback_config("fallback = [\"b\"]")).unwrap_err();
        assert_eq!(e.line, Some(8));
        assert!(e.message.contains("itself"), "{}", e.message);
    }

    #[test]
    fn a_provider_table_parses_and_merges_over_its_preset() {
        let text = r#"
            [[provider]]
            id = "ollama-home"
            api = "openai-chat"
            base_url = "http://10.0.0.5:11434/v1"
            headers = { "x-team" = "home" }

            [[provider]]
            id = "groq"
            base_url = "https://gw.example/openai/v1"

            [[account]]
            name = "home"
            provider = "ollama-home"
            auth = { none = true }
        "#;
        let (table, warnings) = parse_file(p(), text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let c: Config = Value::Table(table).try_into().unwrap();
        assert!(c.problems().is_empty(), "{:?}", c.problems());
        assert_eq!(c.providers.len(), 2);
        let home = aibus_provider::presets::resolve("ollama-home", Some(&c.providers[0]))
            .unwrap()
            .unwrap();
        assert_eq!(home.headers["x-team"], "home");
        let groq = aibus_provider::presets::resolve("groq", Some(&c.providers[1]))
            .unwrap()
            .unwrap();
        assert_eq!(
            groq.base_url.as_deref(),
            Some("https://gw.example/openai/v1")
        );
        assert_eq!(groq.env_vars, ["GROQ_API_KEY"]);
    }

    #[test]
    fn a_provider_table_is_rejected_for_an_unknown_api_a_bad_url_a_credential_header_and_a_repeat()
    {
        let problem = |text: &str| -> String {
            match parse_file(p(), text) {
                Err(e) => e.message,
                Ok((table, _)) => {
                    let c: Config = Value::Table(table).try_into().unwrap();
                    let (key, why) = c.problems().into_iter().next().expect("a problem");
                    format!("{key}: {why}")
                }
            }
        };
        assert!(problem("[[provider]]\nid = \"p\"\napi = \"grpc\"\n").contains("grpc"));
        assert!(problem(
            "[[provider]]\nid = \"p\"\napi = \"openai-chat\"\nbase_url = \"ftp://x\"\n"
        )
        .contains("base_url"));
        assert!(problem("[[provider]]\nid = \"p\"\napi = \"openai-chat\"\nheaders = { Authorization = \"Bearer x\" }\n")
            .starts_with("provider.headers:"));
        assert!(problem("[[provider]]\nid = \"p\"\n").contains("no api"));
        assert!(problem("[[provider]]\nid = \"p\"\napi = \"openai-chat\"\n[[provider]]\nid = \"p\"\napi = \"openai-chat\"\n")
            .contains("defined twice"));
    }

    #[test]
    fn a_project_file_cannot_add_providers() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config.toml");
        let project = dir.path().join("settings.toml");
        std::fs::write(&user, "").unwrap();
        std::fs::write(&project, "[[provider]]\nid = \"evil\"\napi = \"openai-chat\"\nbase_url = \"http://evil.example\"\n").unwrap();
        let l = load(&user, Some(&project)).unwrap();
        assert!(l.config.providers.is_empty());
        assert!(
            l.warnings[0].contains("[[provider]] is user-only"),
            "{:?}",
            l.warnings
        );
    }

    fn load_text(text: &str) -> Result<Loaded, ConfigFileError> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, text).unwrap();
        load(&file, None)
    }

    #[test]
    fn the_bedrock_and_vertex_forms_load_with_region_and_project() {
        let text = r#"
[[account]]
name = "bedrock"
provider = "bedrock"
auth = { sigv4 = true }
region = "us-east-1"

[[account]]
name = "vertex"
provider = "vertex"
auth = { gcloud = true }
project = "my-project"
region = "us-east5"

[[account]]
name = "bedrock-env"
provider = "bedrock"
auth = { aws_env = true }
region = "eu-west-1"

[[account]]
name = "bedrock-profile"
provider = "bedrock"
auth = { aws_profile = "work" }
region = "us-west-2"
"#;
        let c = load_text(text).unwrap().config;
        let [b, v, e, p] = &c.accounts[..] else {
            panic!("four accounts")
        };
        assert!(b.auth.sigv4 && b.region.as_deref() == Some("us-east-1"));
        assert!(
            v.auth.gcloud
                && v.project.as_deref() == Some("my-project")
                && v.region.as_deref() == Some("us-east5")
        );
        assert!(e.auth.aws_env && p.auth.aws_profile.as_deref() == Some("work"));
    }

    #[test]
    fn the_wrong_auth_for_an_api_is_a_config_error_that_names_the_line() {
        let cases = [
            (
                "anthropic",
                "auth = { sigv4 = true }",
                "sigv4",
                "anthropic-bedrock",
            ),
            (
                "vertex",
                "auth = { sigv4 = true }",
                "sigv4",
                "anthropic-bedrock",
            ),
            (
                "bedrock",
                "auth = { gcloud = true }",
                "gcloud",
                "anthropic-vertex",
            ),
            (
                "openai",
                "auth = { gcloud = true }",
                "gcloud",
                "anthropic-vertex",
            ),
            (
                "vertex",
                "auth = { aws_env = true }",
                "aws_env",
                "anthropic-bedrock",
            ),
            (
                "anthropic",
                "auth = { aws_profile = \"work\" }",
                "aws_profile",
                "anthropic-bedrock",
            ),
        ];
        for (provider, auth, key, api) in cases {
            let text = format!("[[account]]\nname = \"x\"\nprovider = \"{provider}\"\n{auth}\n");
            let e = load_text(&text).unwrap_err();
            assert_eq!(e.line, Some(4), "{provider} {auth}: {e}");
            assert!(
                e.message.contains(key) && e.message.contains(api),
                "{provider} {auth}: {}",
                e.message
            );
        }
    }

    #[test]
    fn two_credential_sources_on_one_cloud_account_are_refused() {
        let text = "[[account]]\nname = \"x\"\nprovider = \"bedrock\"\nauth = { sigv4 = true, aws_env = true }\nregion = \"us-east-1\"\n";
        let e = load_text(text).unwrap_err();
        assert!(e.message.contains("pick one"), "{}", e.message);
    }

    #[test]
    fn a_region_or_project_on_the_provider_table_is_the_default_and_the_account_wins() {
        let text = r#"
[[provider]]
id = "bedrock"
region = "eu-central-1"

[[account]]
name = "a"
provider = "bedrock"
auth = { sigv4 = true }

[[account]]
name = "b"
provider = "bedrock"
auth = { sigv4 = true }
region = "us-east-1"
"#;
        let c = load_text(text).unwrap().config;
        assert_eq!(c.providers[0].region.as_deref(), Some("eu-central-1"));
        assert_eq!(c.accounts[1].region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn a_cloud_account_needs_its_region_and_vertex_its_project() {
        let bedrock =
            "[[account]]\nname = \"x\"\nprovider = \"bedrock\"\nauth = { sigv4 = true }\n";
        assert!(load_text(bedrock).unwrap_err().message.contains("region"));
        let vertex = "[[account]]\nname = \"x\"\nprovider = \"vertex\"\nauth = { gcloud = true }\nregion = \"us-east5\"\n";
        assert!(load_text(vertex).unwrap_err().message.contains("project"));
    }
}
