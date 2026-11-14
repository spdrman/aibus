//! `workflow_create` and `workflow_update` (#103): the content rules that make a remote write
//! safe enough, called directly on the server.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use aibus_core::workflow::{self, Workflow};
use aibus_core::{AccountName, Redactor};
use aibus_mcp::{Caps, WRITES_PER_MINUTE};
use aibus_testkit::Xdg;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const FLOW: &str = "description = \"Plan, then build\"\n\n\
    [[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n\n\
    [[step]]\nagent = \"general\"\nprompt = \"Build it: {{prev.summary}}\"\n";

const GATED: &str = "description = \"Plan, then build\"\n\n\
    [[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n\n\
    [[step]]\nagent = \"general\"\nprompt = \"Build it: {{prev.summary}}\"\ngate = \"cargo test\"\n";

const PROVENANCE: &str = "# written through aibus mcp, ";

fn caps(gates: bool) -> Caps {
    Caps::new(true, gates, false, vec![]).unwrap()
}

/// A server whose project already has `.aibus/`.
fn setup(caps: Caps) -> (aibus_mcp::Server, Xdg) {
    let (server, home) = common::server(caps, Redactor::new());
    fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
    (server, home)
}

fn call(server: &aibus_mcp::Server, tool: &str, args: Value, now: Instant) -> Value {
    let r = server
        .call(None, tool, args.as_object().cloned(), now)
        .unwrap();
    serde_json::to_value(r).unwrap()
}

fn create(server: &aibus_mcp::Server, name: &str, scope: &str, toml: &str) -> Value {
    call(
        server,
        "workflow_create",
        json!({ "name": name, "scope": scope, "toml": toml }),
        Instant::now(),
    )
}

fn update(server: &aibus_mcp::Server, name: &str, toml: &str, revision: &str) -> Value {
    let args =
        json!({ "name": name, "scope": "project", "toml": toml, "expected_revision": revision });
    call(server, "workflow_update", args, Instant::now())
}

fn is_error(r: &Value) -> bool {
    r["isError"] == true
}

fn message(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Every path below `dir`, sorted, so a before and an after can be compared.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        for e in read.flatten() {
            out.push(e.path().display().to_string());
            walk(&e.path(), out);
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn project_file(home: &Xdg, name: &str) -> std::path::PathBuf {
    home.path()
        .join("proj/.aibus/workflows")
        .join(format!("{name}.toml"))
}

#[test]
fn create_refuses_bad_names_before_touching_the_disk() {
    let (server, home) = setup(caps(false));
    let long = "a".repeat(65);
    let bad = [
        "../x",
        "/etc/x",
        "~x",
        "X",
        long.as_str(),
        "a/b",
        "a\\b",
        ".hidden",
        "",
        "a.b",
        "-rf",
        "_x",
        "a b",
    ];
    let before = (tree(home.path()), home.paths.config.exists());
    for scope in ["project", "user"] {
        for name in bad {
            let r = create(&server, name, scope, FLOW);
            assert!(is_error(&r), "{name:?} in {scope}: {r}");
            assert!(message(&r).contains("name"), "{name:?}: {}", message(&r));
        }
    }
    // The audit file is the one thing the server itself writes; everything else is as it was.
    assert_eq!((tree(home.path()), home.paths.config.exists()), before);
    // 64 characters is the longest name that works.
    let ok = create(&server, &"a".repeat(64), "project", FLOW);
    assert!(!is_error(&ok), "{ok}");
}

#[test]
fn create_refuses_a_gate_without_the_gates_flag() {
    let (server, home) = setup(caps(false));
    let r = create(&server, "gated", "project", GATED);
    assert!(is_error(&r), "{r}");
    assert!(message(&r).contains("step 2"), "{}", message(&r));
    assert!(message(&r).contains("--allow gates"), "{}", message(&r));
    assert!(!project_file(&home, "gated").exists());
    assert!(
        !home.path().join("proj/.aibus/workflows").exists(),
        "not even the directory"
    );

    let (server, home) = setup(caps(true));
    let r = create(&server, "gated", "project", GATED);
    assert!(!is_error(&r), "{r}");
    // The warning shows the command text.
    assert!(
        r["structuredContent"]["warnings"]
            .to_string()
            .contains("cargo test"),
        "{r}"
    );
    assert!(fs::read_to_string(project_file(&home, "gated"))
        .unwrap()
        .contains("gate = \"cargo test\""));
}

#[test]
fn create_writes_atomically_and_the_file_parses() {
    let (server, home) = setup(caps(false));
    let r = create(&server, "ship", "project", FLOW);
    assert!(!is_error(&r), "{r}");
    let body = &r["structuredContent"];
    assert_eq!(
        (body["name"].as_str(), body["scope"].as_str()),
        (Some("ship"), Some("project"))
    );
    assert_eq!(body["path"], ".aibus/workflows/ship.toml");

    let file = project_file(&home, "ship");
    let bytes = fs::read(&file).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    // The input, byte for byte, under one provenance line.
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.starts_with(PROVENANCE), "{first}");
    assert_eq!(rest, FLOW);
    assert_eq!(body["revision"], sha(&bytes));
    // No temp file is left next to it, and it is private.
    assert_eq!(tree(file.parent().unwrap()), [file.display().to_string()]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // The workflow parser and the listing both accept it, and so does workflow_list.
    assert!(Workflow::parse("ship", &text).is_ok());
    let dirs = workflow::dirs(&home.path().join("proj"), Some(&home.paths.config));
    assert!(workflow::list(&dirs)
        .iter()
        .any(|f| f.name == "ship" && f.parsed.is_ok()));
    let listed = call(&server, "workflow_list", json!({}), Instant::now());
    assert_eq!(
        listed["structuredContent"]["workflows"][0]["ok"], true,
        "{listed}"
    );

    // A second create of the same name is an error that points at workflow_update.
    let again = create(&server, "ship", "project", FLOW);
    assert!(
        is_error(&again) && message(&again).contains("workflow_update"),
        "{again}"
    );
    assert_eq!(fs::read(&file).unwrap(), bytes);
}

#[test]
fn the_user_scope_makes_its_directory_and_the_project_scope_needs_aibus() {
    let (server, home) = common::server(caps(false), Redactor::new());
    // No .aibus in the project: nothing is created there.
    let r = create(&server, "ship", "project", FLOW);
    assert!(is_error(&r) && message(&r).contains(".aibus"), "{r}");
    assert!(!home.path().join("proj/.aibus").exists());

    let r = create(&server, "ship", "user", FLOW);
    assert!(!is_error(&r), "{r}");
    let file = home.paths.config.join("workflows/ship.toml");
    assert!(file.is_file());
    assert_eq!(r["structuredContent"]["scope"], "user");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn a_workflow_that_does_not_validate_is_an_error_and_writes_nothing() {
    let (server, home) = setup(caps(true));
    let before = tree(home.path());
    for (why, toml) in [
        ("not toml", "this is = = not toml"),
        ("no steps", "description = \"x\"\n"),
        (
            "unknown agent",
            "[[step]]\nagent = \"nobody-by-this-name\"\nprompt = \"x\"\n",
        ),
        (
            "bad template",
            "[[step]]\nagent = \"plan\"\nprompt = \"{{nope}}\"\n",
        ),
    ] {
        let r = create(&server, "bad", "project", toml);
        assert!(is_error(&r), "{why}: {r}");
    }
    assert_eq!(tree(home.path()), before);
    assert!(!project_file(&home, "bad").exists());
}

#[test]
fn update_needs_the_current_revision() {
    let (server, home) = setup(caps(false));
    // Updating what is not there is an error that points at workflow_create.
    let missing = update(&server, "ship", FLOW, &"0".repeat(64));
    assert!(
        is_error(&missing) && message(&missing).contains("workflow_create"),
        "{missing}"
    );

    let first = create(&server, "ship", "project", FLOW);
    let rev1 = first["structuredContent"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let changed = FLOW.replace("Plan, then build", "Plan, build, check");

    // A wrong revision writes nothing and names the current one.
    let stale = update(&server, "ship", &changed, &"f".repeat(64));
    assert!(
        is_error(&stale) && message(&stale).contains(&rev1),
        "{stale}"
    );
    assert!(!fs::read_to_string(project_file(&home, "ship"))
        .unwrap()
        .contains("check"));

    // The right one writes, and answers with the new revision.
    let second = update(&server, "ship", &changed, &rev1);
    assert!(!is_error(&second), "{second}");
    let rev2 = second["structuredContent"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(rev1, rev2);
    assert_eq!(rev2, sha(&fs::read(project_file(&home, "ship")).unwrap()));

    // Two updates with the same expected revision: the second fails with what the first made.
    let late = update(
        &server,
        "ship",
        &FLOW.replace("Plan, then build", "other"),
        &rev1,
    );
    assert!(is_error(&late) && message(&late).contains(&rev2), "{late}");
    assert!(fs::read_to_string(project_file(&home, "ship"))
        .unwrap()
        .contains("check"));
}

#[test]
fn the_provenance_line_is_added_once() {
    let (server, home) = setup(caps(false));
    let count = |home: &Xdg| {
        fs::read_to_string(project_file(home, "ship"))
            .unwrap()
            .lines()
            .filter(|l| l.starts_with(PROVENANCE))
            .count()
    };
    // A copy of the line in the input is not doubled.
    let r = create(
        &server,
        "ship",
        "project",
        &format!("{PROVENANCE}2020-01-01T00:00:00Z\n{FLOW}"),
    );
    assert_eq!(count(&home), 1);
    let text = fs::read_to_string(project_file(&home, "ship")).unwrap();
    assert!(
        !text.contains("2020-01-01"),
        "the line is the server's own: {text}"
    );

    // An update keeps the line the file had and does not add another.
    let first_line = text.lines().next().unwrap().to_owned();
    let rev = r["structuredContent"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    update(
        &server,
        "ship",
        &FLOW.replace("Plan, then build", "v2"),
        &rev,
    );
    assert_eq!(count(&home), 1);
    let after = fs::read_to_string(project_file(&home, "ship")).unwrap();
    assert_eq!(after.lines().next().unwrap(), first_line);
    assert!(after.contains("v2"));

    // With the option off the file is the input.
    let mut c = caps(false);
    c.no_provenance = true;
    let (server, home) = setup(c);
    create(&server, "ship", "project", FLOW);
    assert_eq!(
        fs::read_to_string(project_file(&home, "ship")).unwrap(),
        FLOW
    );
}

#[test]
fn size_and_step_caps_hold() {
    let (server, home) = setup(caps(false));
    // 64 KiB exactly is fine, one byte more is not.
    let pad = |n: usize| {
        let mut t = String::from(FLOW);
        while t.len() < n {
            t.push_str(
                "# padding padding padding padding padding padding padding padding padding\n",
            );
        }
        t.truncate(n);
        t
    };
    // Truncating inside a comment keeps the text valid TOML.
    let r = create(&server, "big", "project", &pad(64 * 1024 + 1));
    assert!(is_error(&r) && message(&r).contains("bytes"), "{r}");
    assert!(!project_file(&home, "big").exists());
    let r = create(&server, "fits", "project", &pad(64 * 1024));
    assert!(!is_error(&r), "{r}");

    let steps = |n: usize| {
        (0..n)
            .map(|_| "[[step]]\nagent = \"general\"\nprompt = \"{{input}}\"\n\n")
            .collect::<String>()
    };
    let r = create(&server, "many", "project", &steps(33));
    assert!(is_error(&r) && message(&r).contains("33"), "{r}");
    assert!(!project_file(&home, "many").exists());
    let r = create(&server, "thirty-two", "project", &steps(32));
    assert!(!is_error(&r), "{r}");
}

#[test]
fn the_eleventh_write_in_a_minute_is_an_error() {
    let (server, home) = setup(caps(false));
    let t0 = Instant::now();
    let make = |i: u32, now: Instant| {
        call(
            &server,
            "workflow_create",
            json!({ "name": format!("w{i}"), "scope": "project", "toml": FLOW }),
            now,
        )
    };
    for i in 0..WRITES_PER_MINUTE {
        assert!(!is_error(&make(i, t0)), "write {i}");
    }
    let limited = make(WRITES_PER_MINUTE, t0);
    assert!(
        is_error(&limited) && message(&limited).contains("rate limit"),
        "{limited}"
    );
    assert!(!project_file(&home, &format!("w{WRITES_PER_MINUTE}")).exists());
    // A token comes back every six seconds, so seven is enough.
    assert!(!is_error(&make(
        WRITES_PER_MINUTE,
        t0 + Duration::from_secs(7)
    )));
}

#[test]
fn a_credential_in_the_text_is_refused_and_never_echoed() {
    const KEY: &str = "live-credential-4d5e6f7a8b";
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), KEY);
    let (server, mut home) =
        common::server(Caps::new(true, false, false, vec![]).unwrap(), redactor);
    home.forbid(KEY);
    fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
    let toml = FLOW.replace("Plan this", &format!("Use {KEY} to plan"));
    let r = create(&server, "leaky", "project", &toml);
    assert!(is_error(&r), "{r}");
    assert!(!r.to_string().contains(KEY), "{r}");
    assert!(!project_file(&home, "leaky").exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_workflows_dir_is_refused_and_the_target_is_untouched() {
    let (server, home) = setup(caps(false));
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, home.path().join("proj/.aibus/workflows")).unwrap();
    let r = create(&server, "ship", "project", FLOW);
    assert!(is_error(&r) && message(&r).contains("outside"), "{r}");
    assert!(tree(&outside).is_empty(), "{:?}", tree(&outside));

    // The same for the user scope, and for a workflow file that is itself a link.
    fs::create_dir_all(&home.paths.config).unwrap();
    std::os::unix::fs::symlink(&outside, home.paths.config.join("workflows")).unwrap();
    let r = create(&server, "ship", "user", FLOW);
    assert!(is_error(&r), "{r}");
    assert!(tree(&outside).is_empty());
}

#[cfg(unix)]
#[test]
fn a_workflow_file_that_is_a_symlink_is_not_written_through() {
    let (server, home) = setup(caps(false));
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    let elsewhere = home.path().join("elsewhere.toml");
    fs::write(&elsewhere, "keep me").unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("ship.toml")).unwrap();
    let r = create(&server, "ship", "project", FLOW);
    assert!(is_error(&r), "{r}");
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "keep me");
}

#[test]
fn arguments_the_schema_forbids_are_a_protocol_error() {
    let (server, _home) = setup(caps(false));
    let create = "workflow_create";
    let base = json!({ "name": "x", "scope": "project", "toml": FLOW });
    let with = |k: &str, v: Value| {
        let mut b = base.clone();
        b[k] = v;
        b
    };
    for (tool, args) in [
        (create, json!({})),
        (create, json!({ "name": "x", "scope": "project" })),
        (create, with("scope", json!("everywhere"))),
        (create, with("path", json!("/etc/passwd"))),
        (create, with("name", json!(7))),
        // An update needs the revision it read.
        ("workflow_update", base.clone()),
        (create, with("expected_revision", json!("0".repeat(64)))),
    ] {
        let e = server
            .call(None, tool, args.as_object().cloned(), Instant::now())
            .unwrap_err();
        assert_eq!(e.code.0, -32602, "{tool}: {args}");
    }
}
