//! Invariant 2: aibus never reads, copies or stores the CLI's credentials. Two checks: a run whose
//! config directory holds credential files leaves none of them anywhere in the trace, and the
//! supervisor's source never opens a file at all (it starts processes and reads their pipes).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

fn src(krate: &str) -> Vec<(PathBuf, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(krate)
        .join("src");
    fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| {
            let text = fs::read_to_string(&p).unwrap();
            // Comments may name what is forbidden; only code counts.
            let code = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            (p, code)
        })
        .collect()
}

#[test]
fn the_supervisor_never_opens_a_file_or_names_a_credential_store() {
    for (path, code) in src("aibus-supervisor") {
        for banned in [
            "File::open",
            "fs::read",
            "read_to_string",
            "OpenOptions",
            ".credentials",
            "Keychain",
            "keychain",
            "CredentialStore",
            "find-generic-password",
            "aibus_core::credentials",
        ] {
            assert!(!code.contains(banned), "{} uses {banned}", path.display());
        }
    }
    // The store opens files, but only its own: nothing about credentials or the CLI's config.
    for (path, code) in src("aibus-trace") {
        for banned in [
            ".credentials",
            "Keychain",
            "keychain",
            "CredentialStore",
            "CLAUDE_CONFIG_DIR",
            "aibus_core::credentials",
        ] {
            assert!(!code.contains(banned), "{} uses {banned}", path.display());
        }
    }
}

#[test]
fn the_config_dir_is_only_ever_passed_through_by_name() {
    let uses: Vec<_> = src("aibus-supervisor")
        .into_iter()
        .flat_map(|(p, code)| {
            code.lines()
                .filter(|l| l.contains("CLAUDE_CONFIG_DIR"))
                .map(|l| format!("{}: {}", p.display(), l.trim()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(uses.len(), 1, "one mention, in the allowlist: {uses:#?}");
    assert!(uses[0].contains("ALLOW"), "{uses:#?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_leaves_no_credential_from_the_cli_config_anywhere_in_the_trace() {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;

    use aibus_core::Redactor;
    use aibus_supervisor::{cli, supervise, Launch, Limits, Normalizer};
    use aibus_trace::{PayloadPolicy, StoreLimits, TraceStore};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // A config dir like the owner's, with the files the CLI keeps its sign-in in.
    let config = root.join("home/.claude-b");
    fs::create_dir_all(&config).unwrap();
    let canary = "CANARY-OAUTH-REFRESH-7f3a9c";
    fs::write(
        config.join(".credentials.json"),
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{canary}"}}}}"#),
    )
    .unwrap();
    fs::write(
        config.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{canary}@example.com"}}}}"#),
    )
    .unwrap();
    let exe = root.join("fake-cli");
    let fixture = common::spike_dir().join("11-foreground.stdout.jsonl");
    fs::write(
        &exe,
        format!("#!/bin/sh\ncat > /dev/null\ncat '{}'\n", fixture.display()),
    )
    .unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    let parent: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), root.join("home").into()),
        ("CLAUDE_CONFIG_DIR".into(), config.clone().into()),
    ];
    let launch = Launch {
        exe,
        args: vec!["-p".into()],
        cwd: root.join("work"),
        env: cli::bounded_env(parent, &[], &[]).unwrap(),
        stdin: b"prompt".to_vec(),
        limits: Limits::default(),
    };
    let trace = root.join("trace");
    let (mut store, _) = TraceStore::open(
        &trace,
        Redactor::new(),
        PayloadPolicy::Retain { max_bytes: 1 << 24 },
        StoreLimits::default(),
    )
    .unwrap();
    let mut norm = Normalizer::new(common::attempt());
    supervise(launch, &mut norm, &mut store, Default::default())
        .await
        .unwrap();
    drop(store);
    assert!(
        grep(&trace, canary).is_empty(),
        "a credential reached the trace"
    );
    assert!(
        fs::metadata(trace.join("events.jsonl")).unwrap().len() > 0,
        "the run was traced"
    );
}

fn grep(dir: &Path, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            hits.extend(grep(&p, needle));
        } else if String::from_utf8_lossy(&fs::read(&p).unwrap()).contains(needle) {
            hits.push(p.display().to_string());
        }
    }
    hits
}
