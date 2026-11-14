//! #271 from the command line: `workflow pack` and `workflow install`, `workflow list` saying
//! where each workflow came from, `workflow validate` on a bundle, and a bundled gate that does not
//! run until the bundle is installed with `--allow gates`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use aibus_testkit::xdg_temp;
use common::{aibus, command, ndjson, stderr};
use serde_json::Value;

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> Output {
    let out = aibus(home, args);
    assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
    out
}

fn project_flows(home: &Path) -> PathBuf {
    home.join("proj/.aibus/workflows")
}

/// The user's workflows directory under the throwaway home.
fn user_flows(home: &Path) -> PathBuf {
    home.join(".config/aibus/workflows")
}

const CHAIN: &str = r#"[
    Text("the cause is in main.rs"), Stop(EndTurn),
    Text("a small change.\nRISK: LOW"), Stop(EndTurn),
    Text("patched"), Stop(EndTurn),
    Text("tests pass"), Stop(EndTurn),
    Text("the fix is in"), Stop(EndTurn),
]"#;

#[test]
fn pack_then_install_gives_byte_identical_files_and_list_says_where_each_came_from() {
    let home = xdg_temp();
    let h = home.path();
    ok(h, &["init"]);
    // A hand-edited file: install has to give back exactly these bytes.
    let review = format!(
        "# mine, edited by hand \n{}",
        fs::read_to_string(project_flows(h).join("review.toml")).unwrap()
    );
    fs::write(project_flows(h).join("review.toml"), &review).unwrap();
    let out = ok(
        h,
        &[
            "workflow",
            "pack",
            "review",
            "explore",
            "-o",
            "../kit.aibus",
            "--description",
            "two of mine",
        ],
    );
    assert!(stdout(&out).contains("kit.aibus"), "{}", stdout(&out));
    let bytes = fs::read(h.join("kit.aibus")).unwrap();
    assert_eq!(&bytes[257..263], b"ustar\0", "a plain tar");

    // Into the user's directory: byte for byte, 0600.
    ok(h, &["workflow", "install", "../kit.aibus", "--user"]);
    for name in ["review", "explore"] {
        let installed = user_flows(h).join(format!("{name}.toml"));
        assert_eq!(
            fs::read(&installed).unwrap(),
            fs::read(project_flows(h).join(format!("{name}.toml"))).unwrap(),
            "{name}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    // A bundle in the user's directory is read where it lies; the project wins a name.
    fs::remove_file(user_flows(h).join("review.toml")).unwrap();
    fs::remove_file(user_flows(h).join("explore.toml")).unwrap();
    fs::copy(h.join("kit.aibus"), user_flows(h).join("kit.aibus")).unwrap();
    fs::remove_file(project_flows(h).join("explore.toml")).unwrap();
    let out = ok(h, &["workflow", "list", "--json"]);
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let by = |n: &str| {
        rows.iter()
            .find(|r| r["name"] == n)
            .unwrap_or_else(|| panic!("{n} in {rows:?}"))
            .clone()
    };
    assert_eq!(by("explore")["scope"], "user");
    assert_eq!(by("explore")["bundle"], "kit.aibus");
    assert_eq!(by("explore")["source"], "user bundle kit.aibus");
    assert_eq!(by("review")["scope"], "project");
    assert_eq!(by("review")["bundle"], Value::Null);
    assert_eq!(by("review")["source"], "project");
    let plain = stdout(&ok(h, &["workflow", "list"]));
    assert!(
        plain
            .lines()
            .any(|l| l.starts_with("explore\t") && l.ends_with("\tuser bundle kit.aibus")),
        "{plain}"
    );
    assert!(
        plain
            .lines()
            .any(|l| l.starts_with("review\t") && l.ends_with("\tproject")),
        "{plain}"
    );
    // `show` reads it out of the bundle and says so.
    let shown = stdout(&ok(h, &["workflow", "show", "explore"]));
    assert!(shown.contains("user bundle kit.aibus"), "{shown}");
}

#[test]
fn install_never_overwrites_without_force() {
    let home = xdg_temp();
    let h = home.path();
    ok(h, &["init"]);
    ok(h, &["workflow", "pack", "review", "-o", "../kit.aibus"]);
    fs::write(project_flows(h).join("review.toml"), "# mine\n").unwrap();
    let out = aibus(h, &["workflow", "install", "../kit.aibus"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("--force"), "{}", stderr(&out));
    assert_eq!(
        fs::read_to_string(project_flows(h).join("review.toml")).unwrap(),
        "# mine\n"
    );
    ok(h, &["workflow", "install", "../kit.aibus", "--force"]);
    assert_ne!(
        fs::read_to_string(project_flows(h).join("review.toml")).unwrap(),
        "# mine\n"
    );
}

#[test]
fn pack_writes_only_a_named_aibus_file_and_never_over_one_without_force() {
    let home = xdg_temp();
    let h = home.path();
    ok(h, &["init"]);
    for bad in [".aibus", "kit.tar", "../Kit Two.aibus", "kit"] {
        let out = aibus(h, &["workflow", "pack", "review", "-o", bad]);
        assert_eq!(out.status.code(), Some(2), "{bad}: {}", stderr(&out));
    }
    let out = aibus(h, &["workflow", "pack", "nope", "-o", "../kit.aibus"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!h.join("kit.aibus").exists());
    ok(h, &["workflow", "pack", "review", "-o", "../kit.aibus"]);
    let out = aibus(h, &["workflow", "pack", "explore", "-o", "../kit.aibus"]);
    assert_ne!(out.status.code(), Some(0));
    ok(
        h,
        &[
            "workflow",
            "pack",
            "explore",
            "-o",
            "../kit.aibus",
            "--force",
        ],
    );
}

#[test]
fn pack_with_agents_carries_the_project_agent_and_install_puts_it_back() {
    let home = xdg_temp();
    let h = home.path();
    let agents = h.join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    let def = "---\nname: checker\ndescription: Checks things\n---\nCheck what you are given.\n";
    fs::write(agents.join("checker.md"), def).unwrap();
    fs::create_dir_all(project_flows(h)).unwrap();
    fs::write(
        project_flows(h).join("check.toml"),
        "[[step]]\nagent = \"checker\"\nprompt = \"Check {{input}}\"\n",
    )
    .unwrap();
    ok(
        h,
        &[
            "workflow",
            "pack",
            "check",
            "--with-agents",
            "-o",
            "../kit.aibus",
        ],
    );
    ok(h, &["workflow", "install", "../kit.aibus", "--user"]);
    assert_eq!(
        fs::read_to_string(h.join(".config/aibus/agents/checker.md")).unwrap(),
        def
    );
}

#[test]
fn a_bundled_gate_is_listed_but_does_not_run_until_installed_with_allow_gates() {
    let home = xdg_temp();
    let h = home.path();
    ok(h, &["init"]);
    // `fix` has a gate. Pack it, then make the bundle the only place it is.
    ok(h, &["workflow", "pack", "fix", "-o", "../ci.aibus"]);
    fs::remove_file(project_flows(h).join("fix.toml")).unwrap();
    fs::copy(h.join("ci.aibus"), project_flows(h).join("ci.aibus")).unwrap();

    let out = ok(h, &["workflow", "list", "--json"]);
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let fix = rows.iter().find(|r| r["name"] == "fix").expect("listed");
    assert_eq!(fix["runnable"], false, "{fix}");
    assert!(
        fix["not_runnable"]
            .as_str()
            .unwrap()
            .contains("--allow gates"),
        "{fix}"
    );

    let out = aibus(h, &["workflow", "validate", "--json"]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report.to_string().contains("--allow gates"), "{report}");

    // Neither `workflow run`, `run --workflow` nor `/fix` starts it.
    for args in [
        &["workflow", "run", "fix", "x"][..],
        &["run", "--workflow", "fix", "x"],
    ] {
        let out = aibus(h, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("--allow gates"), "{}", stderr(&out));
    }
    let file = h.join("s.ron");
    fs::write(&file, r#"[Text("no workflow ran"), Stop(EndTurn)]"#).unwrap();
    let out = command(h)
        .args(["run", "--json", "--script"])
        .arg(&file)
        .arg("/fix login")
        .output()
        .unwrap();
    let lines = ndjson(&out);
    assert!(
        !lines
            .iter()
            .any(|l| l["type"] == "RunStarted" && l["agent"] == "explore"),
        "a step ran: {lines:?}"
    );

    // Installing without the flag refuses and writes nothing.
    let out = aibus(h, &["workflow", "install", "../ci.aibus"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("--allow gates"), "{}", stderr(&out));
    assert!(!project_flows(h).join("fix.toml").exists());

    // Trusted: the loose copy wins and runs, gate and all.
    ok(
        h,
        &["workflow", "install", "../ci.aibus", "--allow", "gates"],
    );
    fs::write(&file, CHAIN).unwrap();
    let out = command(h)
        .args([
            "--json",
            "workflow",
            "run",
            "fix",
            "login fails",
            "--script",
        ])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn validate_reads_a_bundle_file_and_a_hostile_one_is_refused_and_writes_nothing() {
    let home = xdg_temp();
    let h = home.path();
    ok(h, &["init"]);
    ok(h, &["workflow", "pack", "review", "-o", "../kit.aibus"]);
    let out = ok(h, &["workflow", "validate", "../kit.aibus"]);
    assert!(stdout(&out).contains("ok"), "{}", stdout(&out));

    // A tar with one entry that climbs out of the directory.
    let mut tar = vec![0u8; 512];
    let name = b"../../escaped.toml";
    tar[..name.len()].copy_from_slice(name);
    tar[100..108].copy_from_slice(b"0000644\0");
    tar[124..136].copy_from_slice(b"00000000000\0");
    tar[156] = b'0';
    tar[257..263].copy_from_slice(b"ustar\0");
    tar[263..265].copy_from_slice(b"00");
    tar[148..156].copy_from_slice(b"        ");
    let sum: u32 = tar.iter().map(|b| u32::from(*b)).sum();
    tar[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    tar.extend_from_slice(&[0u8; 1024]);
    fs::write(h.join("evil.aibus"), &tar).unwrap();
    let out = aibus(
        h,
        &[
            "workflow",
            "install",
            "../evil.aibus",
            "--force",
            "--allow",
            "gates",
        ],
    );
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr(&out).contains(".."), "{}", stderr(&out));
    assert!(!h.join("escaped.toml").exists() && !h.join("proj/escaped.toml").exists());
    let out = aibus(h, &["workflow", "validate", "../evil.aibus"]);
    assert_eq!(out.status.code(), Some(12), "{}", stdout(&out));
}
