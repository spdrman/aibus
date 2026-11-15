//! Version and flag probing, discovery among several installs, the allowlisted `auth status`, the
//! argv and the bounded environment.

mod common;

use std::ffi::OsString;
use std::fs;

use aibus_supervisor::cli::{self, ProbeError, Request, Version, MIN_VERSION, TESTED_VERSION};

fn spike(name: &str) -> String {
    fs::read_to_string(common::spike_dir().join(name)).unwrap()
}

fn request() -> Request {
    Request {
        model: Some("haiku".into()),
        setting_sources: "user".into(),
        max_budget_usd: None,
        include_hook_events: false,
        forward_subagent_text: false,
        agents_json: None,
    }
}

#[test]
fn the_spike_version_parses_and_is_the_tested_one() {
    assert_eq!(
        cli::parse_version(&spike("01-version.txt")),
        Some(Version(2, 1, 311))
    );
    let p = cli::probe(&spike("01-version.txt"), &spike("02-help.txt")).unwrap();
    assert_eq!(p.version, TESTED_VERSION);
    assert!(p.tested);
    for f in [
        "--print",
        "--output-format",
        "--verbose",
        "--permission-prompts",
        "--setting-sources",
        "--forward-subagent-text",
        "--include-hook-events",
        "--agents",
        "--bare",
    ] {
        assert!(p.flags.contains(f), "{f} missing from {:?}", p.flags);
    }
    assert!(
        !p.flags.contains("--max-turns"),
        "the spike found --max-turns missing from --help"
    );
}

#[test]
fn garbage_and_stale_versions_are_refused() {
    assert_eq!(cli::parse_version("claude"), None);
    assert!(matches!(
        cli::probe("nonsense", &spike("02-help.txt")),
        Err(ProbeError::Version(_))
    ));
    // The npm 1.0.34 build the spike found third on PATH.
    assert_eq!(
        cli::probe("1.0.34 (Claude Code)", &spike("02-help.txt")),
        Err(ProbeError::TooOld(Version(1, 0, 34)))
    );
    assert!(MIN_VERSION > Version(1, 0, 34));
    let newer = cli::probe("2.2.0 (Claude Code)", &spike("02-help.txt")).unwrap();
    assert!(!newer.tested, "an untested version runs, flagged");
}

#[test]
fn a_missing_required_flag_is_refused() {
    let help = spike("02-help.txt").replace("--permission-prompts", "--permission-whatever");
    assert_eq!(
        cli::probe(&spike("01-version.txt"), &help),
        Err(ProbeError::MissingFlag("--permission-prompts".into()))
    );
}

#[test]
fn bare_as_the_default_for_print_is_refused_loudly() {
    let help = spike("02-help.txt").replace(
        "  --bare                                Minimal mode: skip hooks",
        "  --bare                                Minimal mode (the default for -p): skip hooks",
    );
    assert_eq!(
        cli::probe(&spike("01-version.txt"), &help),
        Err(ProbeError::BareDefault)
    );
}

#[test]
fn argv_never_carries_the_prompt_or_a_permission_bypass() {
    let p = cli::probe(&spike("01-version.txt"), &spike("02-help.txt")).unwrap();
    let mut req = request();
    req.include_hook_events = true;
    req.forward_subagent_text = true;
    req.agents_json = Some(r#"{"reader-a":{"description":"d","prompt":"p"}}"#.into());
    let argv: Vec<String> = cli::argv(&p, &req)
        .unwrap()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    let pos = |f: &str| {
        argv.iter()
            .position(|a| a == f)
            .unwrap_or_else(|| panic!("{f} not in {argv:?}"))
    };
    assert_eq!(argv[0], "-p");
    assert_eq!(argv[pos("--output-format") + 1], "stream-json");
    assert_eq!(argv[pos("--permission-prompts") + 1], "none");
    assert_eq!(argv[pos("--setting-sources") + 1], "user");
    pos("--verbose");
    pos("--include-hook-events");
    pos("--forward-subagent-text");
    pos("--agents");
    for banned in [
        "--dangerously-skip-permissions",
        "bypassPermissions",
        "--permission-mode",
        "--bare",
    ] {
        assert!(
            !argv.iter().any(|a| a.contains(banned)),
            "{banned} in {argv:?}"
        );
    }
    // `-p` takes no prompt argument: the next element is a flag.
    assert!(argv[1].starts_with("--"));
}

#[test]
fn an_optional_flag_the_cli_lacks_is_an_error_not_silently_dropped() {
    let help = spike("02-help.txt").replace("--forward-subagent-text", "--something-else");
    let p = cli::probe(&spike("01-version.txt"), &help).unwrap();
    let mut req = request();
    req.forward_subagent_text = true;
    assert_eq!(
        cli::argv(&p, &req),
        Err(ProbeError::MissingFlag("--forward-subagent-text".into()))
    );
    req.forward_subagent_text = false;
    assert!(cli::argv(&p, &req).is_ok());
}

#[test]
fn auth_status_keeps_four_fields_and_never_identity() {
    let s = cli::auth_status(spike("05-auth-status.redacted.json").as_bytes()).unwrap();
    assert_eq!(s.logged_in, Some(true));
    assert_eq!(s.auth_method.as_deref(), Some("claude.ai"));
    assert_eq!(s.api_provider.as_deref(), Some("firstParty"));
    assert_eq!(s.subscription_type.as_deref(), Some("max"));
    let real = br#"{"loggedIn":true,"authMethod":"claude.ai","email":"someone@example.com","orgId":"org-123","orgName":"Acme","token":"sk-ant-oat01-SECRETSECRETSECRETSECRET"}"#;
    let s = cli::auth_status(real).unwrap();
    let shown = format!("{s:?}");
    for leaked in ["someone@example.com", "org-123", "Acme", "SECRET"] {
        assert!(!shown.contains(leaked), "{leaked} kept: {shown}");
    }
    assert_eq!(cli::auth_status(b"not json"), None);
}

fn parent() -> Vec<(OsString, OsString)> {
    [
        ("HOME", "/home/u"),
        ("USER", "u"),
        ("LANG", "C.UTF-8"),
        ("PATH", "/evil/bin:/usr/bin"),
        ("CLAUDE_CONFIG_DIR", "/home/u/.claude-b"),
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("CLAUDE_CODE_SSE_PORT", "1234"),
        (
            "ANTHROPIC_API_KEY",
            "sk-ant-api03-PARENTKEYPARENTKEYPARENTKEY",
        ),
        (
            "CLAUDE_CODE_OAUTH_TOKEN",
            "sk-ant-oat01-PARENTTOKENPARENTTOKEN",
        ),
        ("GITHUB_TOKEN", "ghp_parent"),
        ("AWS_SECRET_ACCESS_KEY", "aws"),
        ("EDITOR", "vim"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}

fn names(env: &[(OsString, OsString)]) -> Vec<String> {
    env.iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_environment_is_the_allowlist_with_the_config_dir_and_a_fixed_path() {
    let env = cli::bounded_env(parent(), &[], &[]).unwrap();
    let mut n = names(&env);
    n.sort();
    assert_eq!(n, ["CLAUDE_CONFIG_DIR", "HOME", "LANG", "PATH", "USER"]);
    let get = |k: &str| {
        env.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    assert_eq!(get("PATH").as_deref(), Some("/usr/bin:/bin"));
    assert_eq!(
        get("CLAUDE_CONFIG_DIR").as_deref(),
        Some("/home/u/.claude-b")
    );
}

#[test]
fn the_parent_sessions_claude_code_variables_never_pass() {
    for name in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
    ] {
        assert_eq!(
            cli::bounded_env(parent(), &[name], &[]),
            Err(cli::EnvError::Refused(name.into()))
        );
    }
    // An explicit value the caller sets on purpose (the spike's agent-teams switch) is allowed.
    let env = cli::bounded_env(
        parent(),
        &[],
        &[("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1")],
    )
    .unwrap();
    assert!(names(&env).contains(&"CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".to_string()));
    let env = cli::bounded_env(parent(), &["EDITOR"], &[]).unwrap();
    assert!(names(&env).contains(&"EDITOR".to_string()));
}

#[test]
fn credential_shaped_names_are_refused_whether_passed_or_set() {
    for name in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "MY_PASSWORD",
        "SESSION_COOKIE",
        "ANTHROPIC_AUTH_TOKEN",
    ] {
        assert_eq!(
            cli::bounded_env(parent(), &[name], &[]),
            Err(cli::EnvError::Refused(name.into())),
            "pass {name}"
        );
        assert_eq!(
            cli::bounded_env(parent(), &[], &[(name, "x")]),
            Err(cli::EnvError::Refused(name.into())),
            "set {name}"
        );
    }
    let env = cli::bounded_env(parent(), &[], &[]).unwrap();
    assert!(
        !env.iter()
            .any(|(_, v)| v.to_string_lossy().contains("PARENT")),
        "no parent credential value in {env:?}"
    );
}

#[cfg(unix)]
mod discovery {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn fake(dir: &Path, version: &str, mode: u32) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join("claude");
        fs::write(&p, format!("#!/bin/sh\necho '{version} (Claude Code)'\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    fn env() -> Vec<(OsString, OsString)> {
        cli::bounded_env(Vec::<(OsString, OsString)>::new(), &[], &[]).unwrap()
    }

    #[tokio::test]
    async fn several_installs_are_all_listed_and_the_first_safe_current_one_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let stale = root.join("npm-global");
        fake(&stale, "1.0.34", 0o755);
        let open = root.join("open");
        fake(&open, "2.1.311", 0o777);
        let no_exec = root.join("noexec");
        fake(&no_exec, "2.1.311", 0o644);
        let cask = root.join("cask/2.1.311");
        let real = fake(&cask, "2.1.311", 0o755);
        let brew = root.join("brew-bin");
        fs::create_dir_all(&brew).unwrap();
        std::os::unix::fs::symlink(&real, brew.join("claude")).unwrap();
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let dirs = vec![
            PathBuf::from("relative/bin"),
            stale.clone(),
            open.clone(),
            no_exec.clone(),
            empty,
            brew.clone(),
        ];
        let d = cli::discover(&dirs, "claude", &env()).await;

        let by_dir = |dir: &Path| {
            d.candidates
                .iter()
                .find(|c| c.path.parent() == Some(dir))
                .unwrap_or_else(|| panic!("{} not listed: {:#?}", dir.display(), d.candidates))
        };
        assert!(by_dir(Path::new("relative/bin"))
            .verdict
            .as_ref()
            .is_err_and(|e| e.contains("relative")));
        assert!(
            by_dir(&stale)
                .verdict
                .as_ref()
                .is_err_and(|e| e.contains("1.0.34")),
            "{:#?}",
            by_dir(&stale)
        );
        assert!(
            by_dir(&open)
                .verdict
                .as_ref()
                .is_err_and(|e| e.contains("writable")),
            "{:#?}",
            by_dir(&open)
        );
        assert!(by_dir(&no_exec)
            .verdict
            .as_ref()
            .is_err_and(|e| e.contains("executable")));
        let chosen = &d.candidates[d.chosen.expect("one is chosen")];
        assert_eq!(chosen.path, brew.join("claude"));
        assert_eq!(
            chosen.resolved.as_deref(),
            Some(real.as_path()),
            "the link is resolved to the real file"
        );
        assert_eq!(chosen.verdict, Ok(Version(2, 1, 311)));
    }

    /// #337: the discovery tests pass whatever umask the developer's shell has. They run again
    /// in a child under umask 002 (a login user's default on Debian), where a directory made
    /// without an explicit mode comes out group-writable and discovery rightly refuses it.
    #[test]
    fn issue_337_the_discovery_tests_pass_under_umask_002() {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("umask 002; exec \"$0\" discovery:: --skip issue_337_the_discovery_tests --test-threads=1")
            .arg(std::env::current_exe().unwrap())
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    /// #337: a directory the group can write to is refused, which is why the tests above set
    /// their modes instead of inheriting the umask.
    #[tokio::test]
    async fn issue_337_a_group_writable_directory_is_still_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let shared = root.join("group-dir");
        fake(&shared, "2.1.311", 0o755);
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o775)).unwrap();
        let d = cli::discover(std::slice::from_ref(&shared), "claude", &env()).await;
        assert!(d.chosen.is_none());
        assert!(
            d.candidates[0]
                .verdict
                .as_ref()
                .is_err_and(|e| e.contains("directory writable")),
            "{:#?}",
            d.candidates
        );
    }

    #[tokio::test]
    async fn a_safe_binary_in_a_directory_others_can_write_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let open = root.join("shared-dir");
        fake(&open, "2.1.311", 0o755);
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let d = cli::discover(std::slice::from_ref(&open), "claude", &env()).await;
        assert!(d.chosen.is_none());
        assert!(
            d.candidates[0]
                .verdict
                .as_ref()
                .is_err_and(|e| e.contains("directory writable")),
            "{:#?}",
            d.candidates
        );
    }

    #[tokio::test]
    async fn a_binary_owned_by_a_third_user_is_refused() {
        // Needs root to chown; as an ordinary user there is no third uid to use.
        if rustix::process::geteuid().as_raw() != 0 {
            eprintln!("skipped: not root");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dir = root.join("theirs");
        let p = fake(&dir, "2.1.311", 0o755);
        rustix::fs::chown(
            &p,
            Some(rustix::fs::Uid::from_raw(54321)),
            Some(rustix::fs::Gid::from_raw(54321)),
        )
        .unwrap();
        let d = cli::discover(std::slice::from_ref(&dir), "claude", &env()).await;
        assert!(d.chosen.is_none());
        assert!(
            d.candidates[0]
                .verdict
                .as_ref()
                .is_err_and(|e| e.contains("owned by uid 54321")),
            "{:#?}",
            d.candidates
        );
    }

    #[tokio::test]
    async fn nothing_usable_means_nothing_chosen() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().canonicalize().unwrap().join("old");
        fake(&stale, "1.0.34", 0o755);
        let d = cli::discover(&[stale], "claude", &env()).await;
        assert_eq!(d.chosen, None);
        assert_eq!(d.candidates.len(), 1);
    }
}
