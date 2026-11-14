//! `aibus doctor` (06 section 1, 07 section 4): the config, the credential store, the shell
//! sandbox, worktrees that were kept, the index and the XDG directories. Exit 11 if any check
//! failed. `--session <id>` adds the live memory numbers of that session.

use std::path::Path;
use std::time::SystemTime;

use aibus_core::config::{self, Config};
use aibus_core::persist::paths::{project_dir, Paths};
use aibus_core::{AibusError, KeyRef, Keychain, Redactor, Sandbox, SessionId, StoreHealth};
use aibus_provider::presets::{preset, resolve};
use aibus_provider::Api;
use serde_json::{json, Value};

use crate::cli::Global;

struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

fn check(name: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        ok,
        detail: detail.into(),
    }
}

/// `0.1.0 (tag v0.1.0, commit abc123def456)`; no tag part for an untagged build.
pub fn build_id() -> String {
    let (tag, commit) = (env!("AIBUS_BUILD_TAG"), env!("AIBUS_BUILD_COMMIT"));
    match tag {
        "" => format!("{} (commit {commit})", env!("CARGO_PKG_VERSION")),
        _ => format!("{} (tag {tag}, commit {commit})", env!("CARGO_PKG_VERSION")),
    }
}

fn config_check(g: &Global, paths: &Paths) -> (Check, Option<Config>) {
    let project = g
        .project_dir()
        .map(|d| project_dir(&d).join("settings.toml"))
        .ok();
    match config::load(&paths.config_file(), project.as_deref()) {
        Ok(l) => {
            let note = match l.warnings.len() {
                0 => String::new(),
                n => format!(", {n} warning(s): {}", l.warnings.join("; ")),
            };
            (
                check(
                    "config",
                    true,
                    format!("{}{note}", paths.config_file().display()),
                ),
                Some(l.config),
            )
        }
        Err(e) => (check("config", false, e.to_string()), None),
    }
}

/// `none` (the default), or the sandbox `[tools] sandbox` names. A kind whose program is not
/// on `PATH` fails the check, since `bash` would refuse every call.
fn sandbox_check(cfg: Option<&Config>) -> Check {
    let tools = cfg.map(|c| &c.tools);
    let Some(tools) = tools.filter(|t| t.sandbox != config::SandboxKind::None) else {
        return check("shell sandbox", true, "none");
    };
    let sandbox = Sandbox {
        kind: tools.sandbox,
        network: tools.sandbox_network,
    };
    let name = tools.sandbox.as_str();
    let path = std::env::var("PATH").unwrap_or_default();
    if !sandbox.available(&path) {
        return check(
            "shell sandbox",
            false,
            format!("{name} is configured but not on PATH"),
        );
    }
    let net = if tools.sandbox_network {
        ""
    } else {
        ", network off"
    };
    check("shell sandbox", true, format!("{name}{net}"))
}

/// The workflow author's connection to this program's own `aibus mcp`: the user's `[mcp.aibus]`
/// when there is one, else `<this program> mcp` (no `--allow`, so read-only). It fails when the
/// program's own path does not resolve, since the author could not start it.
fn author_check(cfg: Option<&Config>) -> Check {
    if let Some(own) = cfg.and_then(|c| c.mcp.get("aibus")) {
        return check(
            "workflow author",
            true,
            format!("your [mcp.aibus]: {} {}", own.command, own.args.join(" ")),
        );
    }
    match crate::author::server(false) {
        Ok(own) => check(
            "workflow author",
            true,
            format!("{} mcp (read-only, over stdio)", own.command),
        ),
        Err(e) => check("workflow author", false, e),
    }
}

/// `keyring (<backend>)` or `unavailable, env/file only`. It only fails when an account is
/// configured to use a keychain that is not there.
fn store_check(keychain: &dyn Keychain, cfg: Option<&Config>) -> Check {
    match keychain.probe() {
        StoreHealth::Available(name) => check("credential store", true, name),
        StoreHealth::Unavailable(_) => {
            let stuck: Vec<&str> = cfg
                .into_iter()
                .flat_map(|c| c.accounts.iter())
                .filter(|a| a.enabled && matches!(a.key_ref(), Some(KeyRef::Keyring { .. })))
                .map(|a| a.name.as_str())
                .collect();
            if stuck.is_empty() {
                check("credential store", true, "unavailable, env/file only")
            } else {
                check(
                    "credential store",
                    false,
                    format!(
                        "unavailable, env/file only; accounts that need the keychain: {}",
                        stuck.join(", ")
                    ),
                )
            }
        }
    }
}

/// Worktrees an isolated run left behind (`<cache>/worktrees/<session>/<run>`), with whether
/// each has uncommitted changes.
fn worktree_check(paths: &Paths) -> Check {
    let root = paths.cache.join("worktrees");
    let mut kept = Vec::new();
    for session in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for run in std::fs::read_dir(session.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let dirty = std::process::Command::new("git")
                .arg("-C")
                .arg(run.path())
                .args(["status", "--porcelain"])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            kept.push(format!(
                "{} ({})",
                run.path().display(),
                if dirty { "dirty" } else { "clean" }
            ));
        }
    }
    if kept.is_empty() {
        check("worktrees", true, "none kept")
    } else {
        check("worktrees", false, format!("kept: {}", kept.join(", ")))
    }
}

fn index_check(paths: &Paths) -> Check {
    match std::fs::metadata(paths.data.join("index.sqlite")) {
        Ok(m) => check("index", true, format!("{} bytes", m.len())),
        Err(_) => check(
            "index",
            true,
            "not built yet (aibus memory index creates it)",
        ),
    }
}

/// Each directory must be a directory (or not exist yet), writable, and for the state dir private.
fn dirs_check(paths: &Paths) -> Check {
    let mut problems = Vec::new();
    for (name, dir) in [
        ("config", &paths.config),
        ("data", &paths.data),
        ("state", &paths.state),
        ("cache", &paths.cache),
    ] {
        let Ok(meta) = std::fs::metadata(dir) else {
            continue;
        };
        if !meta.is_dir() {
            problems.push(format!("{name} {} is not a directory", dir.display()));
            continue;
        }
        if meta.permissions().readonly() {
            problems.push(format!("{name} {} is read-only", dir.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode() & 0o777;
            if name == "state" && mode & 0o077 != 0 {
                problems.push(format!(
                    "state {} has mode {mode:04o}, run: chmod 700 {}",
                    dir.display(),
                    dir.display()
                ));
            }
        }
    }
    // The MCP server's audit files (`aibus mcp`), one per process.
    let mcp = paths.state.join("logs").join("mcp");
    let audits = std::fs::read_dir(&mcp).map_or(0, |d| d.flatten().count());
    let list = format!(
        "config {}, data {}, state {}, cache {}, mcp audit {} ({audits} files)",
        paths.config.display(),
        paths.data.display(),
        paths.state.display(),
        paths.cache.display(),
        mcp.display()
    );
    if problems.is_empty() {
        check("xdg dirs", true, list)
    } else {
        check("xdg dirs", false, problems.join("; "))
    }
}

/// The live numbers C2.3 writes next to the session while it runs (`memory.json`).
fn session_check(paths: &Paths, id: &str) -> Check {
    if id.parse::<SessionId>().is_err() {
        return check("session memory", false, format!("{id}: not a session id"));
    }
    let dir = paths.state.join("sessions").join(id);
    if !dir.join("session.json").is_file() {
        return check("session memory", false, format!("no session {id}"));
    }
    match std::fs::read_to_string(dir.join("memory.json")) {
        Ok(text) => check("session memory", true, text.trim()),
        Err(_) => check(
            "session memory",
            true,
            "no live numbers recorded for this session",
        ),
    }
}

/// The auth an account is set up with, by the name the config uses; `env (default)` when it
/// names none and the key is read from `AIBUS_ACCOUNT_<NAME>_KEY`.
fn auth_kind(a: &config::Auth) -> &'static str {
    [
        (a.keyring, "keyring"),
        (a.env.is_some(), "env"),
        (a.file.is_some(), "file"),
        (a.none, "none"),
        (a.oauth, "oauth"),
        (a.sigv4, "sigv4"),
        (a.gcloud, "gcloud"),
        (a.aws_env, "aws_env"),
        (a.aws_profile.is_some(), "aws_profile"),
    ]
    .into_iter()
    .find_map(|(on, name)| on.then_some(name))
    .unwrap_or("env (default)")
}

/// One row per provider an enabled account uses and per `[[provider]]` table: the wire it
/// speaks, the URL it will be sent to, whether that came from a preset, a `[[provider]]` or
/// both, the auth its accounts use, how many rows the built-in catalog has for it and how old
/// the refreshed model list is. A `[[provider]]` no enabled account uses gets a warning row
/// (it still passes: nothing is broken, it is only dead weight). An old-style
/// `openai-compatible` account with a `base_url` gets the new form suggested.
fn providers_check(cfg: Option<&Config>, cache: &Path, now: SystemTime) -> Vec<Check> {
    let Some(cfg) = cfg else {
        return vec![check("provider", true, "no config")];
    };
    let used: std::collections::BTreeSet<&str> = cfg
        .accounts
        .iter()
        .filter(|a| a.enabled)
        .map(|a| a.provider.as_str())
        .filter(|id| *id != "fake")
        .collect();
    let ids: std::collections::BTreeSet<&str> = used
        .iter()
        .copied()
        .chain(cfg.providers.iter().map(|d| d.id.as_str()))
        .collect();
    let mut out = Vec::new();
    for id in ids {
        let user = cfg.providers.iter().find(|d| d.id == id);
        let def = match resolve(id, user) {
            Ok(Some(def)) => def,
            Ok(None) => {
                out.push(check("provider", true, format!("{id}: no definition")));
                continue;
            }
            Err(e) => {
                out.push(check("provider", false, e));
                continue;
            }
        };
        let from = match (preset(id).is_some(), user.is_some()) {
            (true, false) => format!("preset {id}"),
            (true, true) => format!("preset {id} + [[provider]]"),
            (false, _) => "custom [[provider]]".to_owned(),
        };
        let accounts: Vec<&config::Account> = cfg
            .accounts
            .iter()
            .filter(|a| a.enabled && a.provider == id)
            .collect();
        let legacy = accounts.iter().find(|a| a.base_url.is_some());
        let url = legacy
            .and_then(|a| a.base_url.as_deref())
            .or(def.base_url.as_deref());
        let api = def.api.map_or("?", Api::as_str);
        let cloud = matches!(def.api, Some(Api::AnthropicBedrock | Api::AnthropicVertex));
        let mut kinds: Vec<&str> = accounts.iter().map(|a| auth_kind(&a.auth)).collect();
        kinds.dedup();
        let rows = match &def.model_table {
            Some(aibus_provider::presets::ModelTableSpec::Named(n)) => {
                aibus_provider::catalog::table(n)
                    .and_then(|t| serde_json::from_str::<Value>(t).ok())
                    .and_then(|v| v["models"].as_array().map(Vec::len))
            }
            _ => None,
        };
        let age = match aibus_provider::models::cache_age(&cache.join("models"), id, now) {
            Some(d) => format!("cache {} d old", d.as_secs() / 86_400),
            None => "no cache".to_owned(),
        };
        let mut row = format!(
            "{id}: {api}, {}, {from}, auth {}, {} catalog rows, {age}",
            url.unwrap_or(if cloud { "default host" } else { "no base_url" }),
            if kinds.is_empty() {
                "no account".to_owned()
            } else {
                kinds.join(" + ")
            },
            rows.unwrap_or(0),
        );
        if cloud {
            // The account's own region and project win over the provider's.
            let account = accounts.first();
            let region = account
                .and_then(|a| a.region.clone())
                .or(def.region.clone());
            let project = account
                .and_then(|a| a.project.clone())
                .or(def.project.clone());
            for (what, value) in [("project", project), ("region", region)] {
                if let Some(v) = value {
                    row.push_str(&format!(", {what} {v}"));
                }
            }
        }
        if let (Some(a), "openai-compatible") = (legacy, id) {
            row.push_str(&format!(
                "; account {} could use [[provider]] id = \"{}\", api = \"openai-chat\", base_url = \"{}\"",
                a.name,
                a.name,
                a.base_url.as_deref().unwrap_or_default()
            ));
        }
        out.push(check("provider", true, row));
        if user.is_some() && !used.contains(id) {
            out.push(check(
                "provider",
                true,
                format!("warning: [[provider]] {id} is used by no enabled account"),
            ));
        }
    }
    if out.is_empty() {
        out.push(check("provider", true, "none configured"));
    }
    out
}

pub fn doctor(
    g: &Global,
    session: Option<&str>,
    redactor: &Redactor,
    keychain: &dyn Keychain,
) -> Result<u8, AibusError> {
    let paths = g.paths()?;
    let (config, cfg) = config_check(g, &paths);
    let os = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let now = SystemTime::now();
    let mut checks = vec![
        check("build", true, format!("{} on {os}", build_id())),
        config,
        store_check(keychain, cfg.as_ref()),
        sandbox_check(cfg.as_ref()),
        author_check(cfg.as_ref()),
        worktree_check(&paths),
        index_check(&paths),
        dirs_check(&paths),
    ];
    // The provider rows follow the config row.
    checks.splice(2..2, providers_check(cfg.as_ref(), &paths.cache, now));
    if let Some(id) = session {
        checks.push(session_check(&paths, id));
    }
    let mut rows: Vec<Value> = checks
        .iter()
        .map(|c| json!({ "name": c.name, "ok": c.ok, "detail": c.detail }))
        .collect();
    for row in &mut rows {
        redactor.redact_json(row);
    }
    if g.json {
        println!("{}", json!({ "checks": rows }));
    } else {
        for row in &rows {
            println!(
                "{}  {}: {}",
                if row["ok"] == true { "ok  " } else { "FAIL" },
                row["name"].as_str().unwrap_or_default(),
                row["detail"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(if checks.iter().all(|c| c.ok) { 0 } else { 11 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_core::MemoryKeychain;

    #[test]
    fn a_missing_keychain_only_matters_when_an_account_wants_it() {
        let gone = MemoryKeychain::unavailable("no Secret Service");
        let c = store_check(&gone, None);
        assert!(c.ok);
        assert_eq!(c.detail, "unavailable, env/file only");

        let mut cfg = Config::default();
        cfg.accounts.push(config::Account {
            name: "work".into(),
            provider: "fake".into(),
            auth: config::Auth {
                keyring: true,
                ..Default::default()
            },
            ..Default::default()
        });
        let c = store_check(&gone, Some(&cfg));
        assert!(!c.ok, "{}", c.detail);
        assert!(c.detail.contains("work"));
    }

    fn account(name: &str, provider: &str, url: Option<&str>) -> config::Account {
        config::Account {
            name: name.into(),
            provider: provider.into(),
            base_url: url.map(Into::into),
            auth: config::Auth {
                keyring: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn each_provider_gets_a_row_with_wire_url_source_auth_catalog_and_cache_and_an_old_compatible_account_gets_a_hint(
    ) {
        let mut cfg = Config::default();
        cfg.accounts.push(account("g", "groq", None));
        cfg.accounts.push(account(
            "home",
            "openai-compatible",
            Some("http://10.0.0.5:11434/v1"),
        ));
        cfg.accounts.push(account("f", "fake", None));
        let cache = tempfile::tempdir().unwrap();
        let rows = providers_check(Some(&cfg), cache.path(), SystemTime::UNIX_EPOCH);
        let details: Vec<&str> = rows.iter().map(|c| c.detail.as_str()).collect();
        assert!(
            rows.iter().all(|c| c.ok && c.name == "provider"),
            "{details:?}"
        );
        let groq = cfg_rows("groq");
        assert!(details.iter().any(|d| d == &format!("groq: openai-chat, https://api.groq.com/openai/v1, preset groq, auth keyring, {groq} catalog rows, no cache")), "{details:?}");
        assert!(details.iter().any(|d| d.starts_with("openai-compatible: openai-chat, http://10.0.0.5:11434/v1, preset openai-compatible, auth keyring, 0 catalog rows, no cache; account home could use [[provider]] id = \"home\"")), "{details:?}");
        assert!(!details.iter().any(|d| d.contains("fake")), "{details:?}");
    }

    /// The catalog rows the file for `name` holds, counted here from the file text.
    fn cfg_rows(name: &str) -> usize {
        let v: Value = serde_json::from_str(aibus_provider::catalog::table(name).unwrap()).unwrap();
        v["models"].as_array().unwrap().len()
    }

    #[test]
    fn a_custom_provider_no_account_uses_is_a_warning_row_and_a_used_one_shows_its_auth_and_cache_age(
    ) {
        let mut cfg = Config::default();
        cfg.providers.push(aibus_provider::ProviderDef {
            id: "lab".into(),
            api: Some(Api::OpenaiChat),
            base_url: Some("http://10.1.1.1:8000/v1".into()),
            ..Default::default()
        });
        cfg.providers.push(aibus_provider::ProviderDef {
            id: "kept".into(),
            api: Some(Api::OpenaiChat),
            base_url: Some("http://10.1.1.2:8000/v1".into()),
            ..Default::default()
        });
        let mut a = account("k", "kept", None);
        a.auth = config::Auth {
            none: true,
            ..Default::default()
        };
        cfg.accounts.push(a);
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cache.path().join("models")).unwrap();
        std::fs::write(
            cache.path().join("models/kept.json"),
            r#"{"fetched_at": 1000, "models": []}"#,
        )
        .unwrap();
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + 3 * 86_400 + 5);
        let rows = providers_check(Some(&cfg), cache.path(), now);
        let details: Vec<&str> = rows.iter().map(|c| c.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "kept: openai-chat, http://10.1.1.2:8000/v1, custom [[provider]], auth none, 0 catalog rows, cache 3 d old",
                "lab: openai-chat, http://10.1.1.1:8000/v1, custom [[provider]], auth no account, 0 catalog rows, no cache",
                "warning: [[provider]] lab is used by no enabled account",
            ]
        );
        assert!(
            rows.iter().all(|c| c.ok),
            "a warning does not fail the check"
        );
    }

    #[test]
    fn a_working_keychain_is_named() {
        let c = store_check(&MemoryKeychain::default(), None);
        assert!(c.ok, "{}", c.detail);
    }

    #[test]
    fn dirs_must_be_directories_and_state_must_be_private() {
        let tmp = tempfile::tempdir().unwrap();
        let p = |n: &str| tmp.path().join(n);
        let paths = Paths {
            config: p("c"),
            data: p("d"),
            state: p("s"),
            cache: p("k"),
            config_override: None,
        };
        // Nothing exists yet: fine, they are created on first use.
        assert!(dirs_check(&paths).ok);
        std::fs::write(p("d"), "").unwrap();
        let c = dirs_check(&paths);
        assert!(
            !c.ok && c.detail.contains("not a directory"),
            "{}",
            c.detail
        );
        std::fs::remove_file(p("d")).unwrap();
        std::fs::create_dir(p("s")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p("s"), std::fs::Permissions::from_mode(0o755)).unwrap();
            let c = dirs_check(&paths);
            assert!(!c.ok && c.detail.contains("chmod 700"), "{}", c.detail);
            std::fs::set_permissions(p("s"), std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(dirs_check(&paths).ok);
        }
    }

    #[test]
    fn kept_worktrees_are_listed_and_fail_the_check() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: tmp.path().join("c"),
            data: tmp.path().join("d"),
            state: tmp.path().join("s"),
            cache: tmp.path().join("k"),
            config_override: None,
        };
        assert!(worktree_check(&paths).ok);
        std::fs::create_dir_all(paths.cache.join("worktrees/S1/R1")).unwrap();
        let c = worktree_check(&paths);
        assert!(!c.ok && c.detail.contains("R1"), "{}", c.detail);
    }

    #[test]
    fn the_session_check_wants_a_real_session() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: tmp.path().join("c"),
            data: tmp.path().join("d"),
            state: tmp.path().join("s"),
            cache: tmp.path().join("k"),
            config_override: None,
        };
        assert!(!session_check(&paths, "../etc").ok);
        let id = SessionId::new().to_string();
        assert!(!session_check(&paths, &id).ok);
        let dir = paths.state.join("sessions").join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        assert!(session_check(&paths, &id)
            .detail
            .contains("no live numbers"));
        std::fs::write(dir.join("memory.json"), r#"{"rss_mb":12}"#).unwrap();
        assert_eq!(session_check(&paths, &id).detail, r#"{"rss_mb":12}"#);
    }
}
