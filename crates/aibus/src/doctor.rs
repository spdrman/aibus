//! `aibus doctor` (06 section 1, 07 section 4): the config, the credential store, the shell
//! sandbox, worktrees that were kept, the index and the XDG directories. Exit 11 if any check
//! failed. `--session <id>` adds the live memory numbers of that session.


use aibus_core::config::{self, Config};
use aibus_core::persist::paths::{project_dir, Paths};
use aibus_core::{AibusError, KeyRef, Keychain, Redactor, Sandbox, SessionId, StoreHealth};
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
                check("config", true, format!("{}{note}", paths.config_file().display())),
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
    let sandbox = Sandbox { kind: tools.sandbox, network: tools.sandbox_network };
    let name = tools.sandbox.as_str();
    let path = std::env::var("PATH").unwrap_or_default();
    if !sandbox.available(&path) {
        return check("shell sandbox", false, format!("{name} is configured but not on PATH"));
    }
    let net = if tools.sandbox_network { "" } else { ", network off" };
    check("shell sandbox", true, format!("{name}{net}"))
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
        for run in std::fs::read_dir(session.path()).into_iter().flatten().flatten() {
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
        Err(_) => check("index", true, "not built yet (aibus memory index creates it)"),
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
        let Ok(meta) = std::fs::metadata(dir) else { continue };
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
    let list = format!(
        "config {}, data {}, state {}, cache {}",
        paths.config.display(),
        paths.data.display(),
        paths.state.display(),
        paths.cache.display()
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
        Err(_) => check("session memory", true, "no live numbers recorded for this session"),
    }
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
    let mut checks = vec![
        check("build", true, format!("{} on {os}", build_id())),
        config,
        store_check(keychain, cfg.as_ref()),
        sandbox_check(cfg.as_ref()),
        worktree_check(&paths),
        index_check(&paths),
        dirs_check(&paths),
    ];
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
        assert!(!c.ok && c.detail.contains("not a directory"), "{}", c.detail);
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
        assert!(session_check(&paths, &id).detail.contains("no live numbers"));
        std::fs::write(dir.join("memory.json"), r#"{"rss_mb":12}"#).unwrap();
        assert_eq!(session_check(&paths, &id).detail, r#"{"rss_mb":12}"#);
    }
}
