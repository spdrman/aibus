//! The sandbox host (#286): an untrusted extension cannot read the vault, launch an undeclared
//! program or reach arbitrary paths, and the limits are the host's. Every test runs a stand-in CLI
//! (a shell script) under the real supervisor, offline.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aibus_core::Redactor;
use aibus_extension::manifest::{Capability, Manifest};
use aibus_extension::sandbox::{check_launch, engine, prepare, Grant, SandboxError};
use aibus_supervisor::{supervise, Ending, Limits};
use aibus_trace::{AttemptRef, EventKind, PayloadPolicy, StoreLimits, TraceStore};
use common::{edit, template_manifest};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "1.4.0"; exit 0;;
  --help) printf 'Usage: examplecli\n\nOptions:\n  --json     json lines\n  --usage    report usage\n'; exit 0;;
esac
env > ./seen-env.txt
echo "$@" > ./seen-args.txt
IFS= read -r prompt
echo "{\"type\":\"text\",\"text\":\"got a prompt of ${#prompt} bytes\"}"
echo '{"type":"tool_start","id":"t1"}'
echo '{"type":"tool_end","id":"t1"}'
echo '{"type":"usage","usage":{"in":5,"out":2}}'
"#;

struct Bench {
    t: tempfile::TempDir,
    bin: PathBuf,
    work: PathBuf,
}

impl Bench {
    fn new(script: &str) -> Self {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        let work = t.path().join("work");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&work).unwrap();
        // Whatever the umask, the search directory is not writable by others.
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let exe = bin.join("examplecli");
        fs::write(&exe, script).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        Self { t, bin, work }
    }

    fn grant(&self) -> Grant {
        Grant {
            workdir: self.work.clone(),
            search_dirs: vec![self.bin.clone()],
            parent_env: vec![
                (OsString::from("HOME"), OsString::from("/home/someone")),
                (
                    OsString::from("ANTHROPIC_API_KEY"),
                    OsString::from("sk-secret-1"),
                ),
                (
                    OsString::from("OPENROUTER_API_KEY"),
                    OsString::from("sk-secret-2"),
                ),
                (
                    OsString::from("AIBUS_MASTER_KEY"),
                    OsString::from("vault-secret"),
                ),
                (OsString::from("LD_PRELOAD"), OsString::from("/tmp/evil.so")),
                (OsString::from("GITHUB_TOKEN"), OsString::from("ghp_secret")),
                (OsString::from("PATH"), OsString::from("/tmp/evil:/usr/bin")),
            ],
            ceiling: Limits {
                wall_time: Duration::from_secs(30),
                grace: Duration::from_millis(300),
                ..Limits::default()
            },
            extra_programs: vec!["examplecli".into()],
            os: if cfg!(target_os = "macos") {
                "macos".into()
            } else {
                "linux".into()
            },
        }
    }
}

fn extra() -> Vec<String> {
    vec!["examplecli".to_owned()]
}

fn manifest() -> Manifest {
    Manifest::parse(&template_manifest()).unwrap()
}

fn attempt(m: &Manifest) -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: m.id.clone(),
    }
}

async fn run(
    b: &Bench,
    m: &Manifest,
    want: &[Capability],
) -> (aibus_supervisor::Outcome, Vec<aibus_trace::TraceEvent>) {
    let mut prepared = prepare(m, &b.grant(), want).await.unwrap();
    prepared.launch.stdin = b"review this please\n".to_vec();
    let dir = b.t.path().join("trace");
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let mut eng = engine(m, attempt(m));
    let out = supervise(
        prepared.launch,
        eng.as_mut(),
        &mut store,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    drop(store);
    let (events, _) = TraceStore::read(&dir).unwrap();
    (out, events)
}

#[tokio::test]
async fn a_third_party_extension_runs_end_to_end_with_no_core_change() {
    let b = Bench::new(SCRIPT);
    let m = manifest();
    let (out, events) = run(&b, &m, &[]).await;
    assert_eq!(out.ending, Ending::Exited(0));
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    for k in [
        EventKind::AttemptStarted,
        EventKind::MessageChunk,
        EventKind::ToolStarted,
        EventKind::ToolCompleted,
        EventKind::UsageReported,
        EventKind::AttemptCompleted,
    ] {
        assert!(kinds.contains(&k), "{k:?} missing from {kinds:?}");
    }
    // Event order is the uuid v7 order (D-319).
    assert!(events.windows(2).all(|w| w[0].event_id < w[1].event_id));
    assert!(events.iter().all(|e| e.provider_extension == m.id));
}

#[tokio::test]
async fn the_prompt_reaches_the_cli_on_stdin_and_never_in_its_arguments() {
    let b = Bench::new(SCRIPT);
    run(&b, &manifest(), &[]).await;
    let args = fs::read_to_string(b.work.join("seen-args.txt")).unwrap();
    assert_eq!(args.trim(), "run --json");
    assert!(!args.contains("review"));
}

/// The vault: nothing credential-shaped, nothing of aibus's own and no loader variable is in the
/// CLI's environment, and PATH is the host's, not the parent's.
#[tokio::test]
async fn the_cli_gets_no_ambient_credential_vault_key_loader_variable_or_parent_path() {
    let b = Bench::new(SCRIPT);
    run(&b, &manifest(), &[]).await;
    let seen = fs::read_to_string(b.work.join("seen-env.txt")).unwrap();
    for leaked in [
        "sk-secret-1",
        "sk-secret-2",
        "vault-secret",
        "ghp_secret",
        "evil",
        "ANTHROPIC_API_KEY",
        "AIBUS_",
        "LD_PRELOAD",
    ] {
        assert!(!seen.contains(leaked), "{leaked} reached the CLI:\n{seen}");
    }
    assert!(seen.contains("PATH=/usr/bin:/bin"), "{seen}");
    assert!(seen.contains("HOME=/home/someone"), "{seen}");
}

#[tokio::test]
async fn an_undeclared_program_is_never_found_or_launched() {
    let b = Bench::new(SCRIPT);
    fs::rename(b.bin.join("examplecli"), b.bin.join("othercli")).unwrap();
    let err = prepare(&manifest(), &b.grant(), &[]).await.unwrap_err();
    assert!(matches!(err, SandboxError::NoExecutable { .. }), "{err:?}");
}

#[tokio::test]
async fn check_launch_refuses_a_launch_of_something_the_manifest_did_not_declare() {
    let b = Bench::new(SCRIPT);
    let m = manifest();
    let good = prepare(&m, &b.grant(), &[]).await.unwrap().launch;
    check_launch(&m, &good, &extra()).unwrap();

    let mut other_exe = good.clone();
    other_exe.exe = PathBuf::from("/bin/sh");
    assert!(matches!(
        check_launch(&m, &other_exe, &extra()),
        Err(SandboxError::Refused(_))
    ));

    let mut relative = good.clone();
    relative.exe = PathBuf::from("examplecli");
    assert!(check_launch(&m, &relative, &extra()).is_err());

    let mut extra_arg = good.clone();
    extra_arg.args.push("--dangerously-skip-permissions".into());
    assert!(check_launch(&m, &extra_arg, &extra()).is_err());

    let mut replaced = good.clone();
    replaced.args[0] = "exec".into();
    assert!(check_launch(&m, &replaced, &extra()).is_err());

    let mut leaky = good.clone();
    leaky.env.push(("AWS_SECRET_ACCESS_KEY".into(), "x".into()));
    assert!(check_launch(&m, &leaky, &extra()).is_err());

    let mut path = good.clone();
    path.env.retain(|(k, _)| k != "PATH");
    path.env.push(("PATH".into(), "/tmp/evil".into()));
    assert!(check_launch(&m, &path, &extra()).is_err());

    let mut cwd = good;
    cwd.cwd = PathBuf::from("relative/dir");
    assert!(check_launch(&m, &cwd, &extra()).is_err());
}

/// PATH shadowing: a relative search directory and an executable others can write are passed over.
#[tokio::test]
async fn a_relative_search_directory_and_a_writable_executable_are_not_used() {
    let b = Bench::new(SCRIPT);
    let mut g = b.grant();
    g.search_dirs = vec![PathBuf::from("."), PathBuf::from("bin")];
    assert!(matches!(
        prepare(&manifest(), &g, &[]).await,
        Err(SandboxError::NoExecutable { .. })
    ));

    fs::set_permissions(b.bin.join("examplecli"), fs::Permissions::from_mode(0o777)).unwrap();
    let err = prepare(&manifest(), &b.grant(), &[]).await.unwrap_err();
    match err {
        SandboxError::NoExecutable { why, .. } => {
            assert!(why.iter().any(|w| w.contains("writable")), "{why:?}")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_working_directory_is_the_hosts_grant_resolved_and_has_to_exist() {
    let b = Bench::new(SCRIPT);
    let mut g = b.grant();
    g.workdir = b.t.path().join("missing");
    assert!(matches!(
        prepare(&manifest(), &g, &[]).await,
        Err(SandboxError::Workdir(_))
    ));
    g.workdir = PathBuf::from("work");
    assert!(matches!(
        prepare(&manifest(), &g, &[]).await,
        Err(SandboxError::Workdir(_))
    ));
    // A link to a directory is followed once, here, and the resolved path is what runs.
    let alias = b.t.path().join("alias");
    symlink(&b.work, &alias).unwrap();
    g.workdir = alias;
    let p = prepare(&manifest(), &g, &[]).await.unwrap();
    assert_eq!(p.launch.cwd, fs::canonicalize(&b.work).unwrap());
}

#[tokio::test]
async fn the_limits_are_the_smaller_of_the_request_and_the_hosts_ceiling() {
    let b = Bench::new(SCRIPT);
    let greedy = edit(
        &template_manifest(),
        "wall_time_secs = 600",
        "wall_time_secs = 999999",
    );
    let p = prepare(&Manifest::parse(&greedy).unwrap(), &b.grant(), &[])
        .await
        .unwrap();
    assert_eq!(p.launch.limits.wall_time, Duration::from_secs(30));
    let modest = edit(
        &template_manifest(),
        "wall_time_secs = 600",
        "wall_time_secs = 5",
    );
    let p = prepare(&Manifest::parse(&modest).unwrap(), &b.grant(), &[])
        .await
        .unwrap();
    assert_eq!(p.launch.limits.wall_time, Duration::from_secs(5));
    // What it did not ask for is the host's own.
    assert_eq!(
        p.launch.limits.max_stdout_bytes,
        b.grant().ceiling.max_stdout_bytes
    );
}

#[tokio::test]
async fn a_runaway_stream_and_a_hung_cli_are_stopped_by_the_hosts_limits() {
    let flood = "#!/bin/sh\ncase \"$1\" in --version) echo 1.4.0; exit 0;; --help) printf '  --json  j\\n'; exit 0;; esac\nwhile :; do echo '{\"type\":\"text\",\"text\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}'; done\n";
    let b = Bench::new(flood);
    let mut g = b.grant();
    g.ceiling.max_stdout_bytes = 4096;
    let mut p = prepare(&manifest(), &g, &[]).await.unwrap();
    p.launch.stdin = b"x\n".to_vec();
    let m = manifest();
    let dir = b.t.path().join("t1");
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let mut eng = engine(&m, attempt(&m));
    let out = supervise(p.launch, eng.as_mut(), &mut store, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::OutputLimit);

    let hang = "#!/bin/sh\ncase \"$1\" in --version) echo 1.4.0; exit 0;; --help) printf '  --json  j\\n'; exit 0;; esac\nsleep 30\n";
    let b = Bench::new(hang);
    let mut g = b.grant();
    g.ceiling.wall_time = Duration::from_secs(1);
    let mut p = prepare(&manifest(), &g, &[]).await.unwrap();
    p.launch.stdin = Vec::new();
    let dir = b.t.path().join("t2");
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let mut eng = engine(&m, attempt(&m));
    let out = supervise(p.launch, eng.as_mut(), &mut store, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::WallTime);
}

#[tokio::test]
async fn an_optional_flag_is_added_only_when_wanted_and_the_cli_lists_it() {
    let b = Bench::new(SCRIPT);
    let m = manifest();
    let off = prepare(&m, &b.grant(), &[]).await.unwrap();
    assert!(!off.launch.args.iter().any(|a| a == "--usage"));
    let on = prepare(&m, &b.grant(), &[Capability::UsageReport])
        .await
        .unwrap();
    assert_eq!(on.launch.args.last().unwrap(), "--usage");
    // A CLI that does not list the flag: the request is an error, never a silent drop.
    let bare = Bench::new(&SCRIPT.replace("  --usage    report usage\\n", ""));
    let err = prepare(&m, &bare.grant(), &[Capability::UsageReport])
        .await
        .unwrap_err();
    assert!(matches!(err, SandboxError::CapabilityMissing(_)), "{err:?}");
}

#[tokio::test]
async fn a_capability_with_no_optional_flag_cannot_be_asked_for() {
    let b = Bench::new(SCRIPT);
    let err = prepare(&manifest(), &b.grant(), &[Capability::WorkspaceControl])
        .await
        .unwrap_err();
    assert!(matches!(err, SandboxError::CapabilityMissing(_)), "{err:?}");
}

#[tokio::test]
async fn an_os_the_extension_does_not_list_is_refused_before_anything_is_found() {
    let b = Bench::new(SCRIPT);
    let mut g = b.grant();
    g.os = "windows".into();
    assert!(matches!(
        prepare(&manifest(), &g, &[]).await,
        Err(SandboxError::Incompatible(_))
    ));
}

#[tokio::test]
async fn a_cli_that_prints_no_version_is_not_launched() {
    let b = Bench::new("#!/bin/sh\necho 'hello'\n");
    let err = prepare(&manifest(), &b.grant(), &[]).await.unwrap_err();
    assert!(matches!(err, SandboxError::NoExecutable { .. }), "{err:?}");
    let _ = Path::new("/");
}

// ---- #332 review, item 1: only the host's allowlist of official CLIs runs ----

fn grant_without_extras(b: &Bench) -> Grant {
    Grant {
        extra_programs: Vec::new(),
        ..b.grant()
    }
}

#[tokio::test]
async fn a_program_that_is_not_on_the_hosts_allowlist_is_never_built_into_a_launch() {
    let b = Bench::new(SCRIPT);
    // The manifest names examplecli and the host's own list has claude, codex, gemini and kimi.
    let err = prepare(&manifest(), &grant_without_extras(&b), &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, SandboxError::NotOfficial(ref n) if n == "examplecli"),
        "{err:?}"
    );
    // The user adds it in their own config, and then it runs.
    assert!(prepare(&manifest(), &b.grant(), &[]).await.is_ok());
}

/// The review's probe: a manifest that parses, with a real system program, in a real directory.
#[tokio::test]
async fn a_system_program_named_by_a_manifest_is_refused_even_in_a_trusted_directory() {
    let b = Bench::new(SCRIPT);
    for prog in ["ls", "cat", "plutil", "gem", "python3", "curl", "sh", "env"] {
        let m = edit(
            &template_manifest(),
            "executable_candidates = [\"examplecli\"]",
            &format!("executable_candidates = [\"{prog}\"]"),
        );
        let Ok(m) = Manifest::parse(&m) else { continue }; // the denylist already refused it
        let mut g = b.grant();
        g.search_dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        let err = prepare(&m, &g, &[]).await.unwrap_err();
        assert!(
            matches!(err, SandboxError::NotOfficial(_)),
            "{prog}: {err:?}"
        );
    }
}

#[tokio::test]
async fn the_hosts_own_list_names_the_four_official_clis_and_nothing_else() {
    assert_eq!(
        aibus_extension::sandbox::OFFICIAL_CLIS,
        ["claude", "codex", "gemini", "kimi"]
    );
}

/// A name on the list that resolves to a shell or an interpreter is not the official CLI.
#[tokio::test]
async fn an_allowlisted_name_that_resolves_to_an_interpreter_is_refused() {
    let b = Bench::new(SCRIPT);
    fs::rename(b.bin.join("examplecli"), b.bin.join("python3")).unwrap();
    symlink(b.bin.join("python3"), b.bin.join("claude")).unwrap();
    let text =
        std::fs::read_to_string(common::reference_dir().join("aibus-extension.toml")).unwrap();
    let m = Manifest::parse(&text).unwrap();
    let mut g = b.grant();
    g.os = "macos".into();
    let err = prepare(&m, &g, &[]).await.unwrap_err();
    assert!(
        matches!(
            err,
            SandboxError::NotOfficial(_) | SandboxError::NoExecutable { .. }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn check_launch_refuses_a_program_that_is_not_on_the_list_whoever_built_the_launch() {
    let b = Bench::new(SCRIPT);
    let m = manifest();
    let good = prepare(&m, &b.grant(), &[]).await.unwrap().launch;
    assert!(check_launch(&m, &good, &[]).is_err());
    assert!(check_launch(&m, &good, &extra()).is_ok());
}

#[test]
fn the_policy_file_has_to_be_the_users_and_not_writable_by_others() {
    use aibus_extension::policy::Policy;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("extension-policy.toml");
    fs::write(&p, "require_signature = true\n").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Policy::load(&p).unwrap().require_signature);
    for mode in [0o664, 0o666, 0o646] {
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        let err = Policy::load(&p).unwrap_err();
        assert!(err.contains("writable"), "{mode:o}: {err}");
    }
    assert!(Policy::load(&t.path().join("absent.toml"))
        .unwrap()
        .extra_official_clis
        .is_empty());
}

#[test]
fn the_users_own_additions_to_the_allowlist_are_plain_names_never_a_shell() {
    use aibus_extension::policy::Policy;
    assert_eq!(
        Policy::parse("extra_official_clis = [\"examplecli\"]")
            .unwrap()
            .extra_official_clis,
        ["examplecli"]
    );
    for bad in [
        "sh",
        "python3",
        "curl",
        "/usr/bin/x",
        "a b",
        "..",
        "env",
        "npx",
    ] {
        assert!(
            Policy::parse(&format!("extra_official_clis = [\"{bad}\"]")).is_err(),
            "{bad}"
        );
    }
}
