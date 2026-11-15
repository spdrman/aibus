//! `aibus extensions` (#286) end to end: a third party packages an extension, publishes it to a
//! registry directory, installs it, tests it offline and manages it, with no change to core source.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::aibus;

const ID: &str = "org.thirdparty.aibus.mycli";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The template copied to `dst` under `ID` at `version`, with its expected files.
fn package(dst: &Path, version: &str) -> PathBuf {
    let src = repo().join("examples/extensions/template");
    for entry in walk(&src) {
        let rel = entry.strip_prefix(&src).unwrap();
        let to = dst.join(rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(&entry, &to).unwrap();
    }
    let m = dst.join("aibus-extension.toml");
    let text = fs::read_to_string(&m)
        .unwrap()
        .replace("org.example.aibus.examplecli", ID)
        .replacen(
            "version = \"0.1.0\"",
            &format!("version = \"{version}\""),
            1,
        );
    fs::write(&m, text).unwrap();
    dst.to_owned()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            v.extend(walk(&p));
        } else {
            v.push(p);
        }
    }
    v
}

struct Env {
    home: tempfile::TempDir,
    reg: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let reg = home.path().join("registry");
        Self { home, reg }
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        let reg = self.reg.to_str().unwrap();
        let mut all = vec!["extensions", "--registry", reg];
        all.extend_from_slice(args);
        aibus(self.home.path(), &all)
    }
    fn publish(&self, version: &str) {
        let dir = self.home.path().join(format!("src-{version}"));
        package(&dir, version);
        let out = self.run(&["publish", dir.to_str().unwrap()]);
        assert!(out.status.success(), "{}", common::stderr(&out));
    }
}

fn text(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_third_party_extension_round_trips_without_a_core_change() {
    let e = Env::new();
    // Write and test it from the directory, before it is anywhere.
    let dir = e.home.path().join("mycli");
    package(&dir, "1.0.0");
    let t = e.run(&["test", dir.to_str().unwrap(), "--fixtures"]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t));
    assert!(
        text(&t).contains("pass  basic: event ids ascend"),
        "{}",
        text(&t)
    );

    // Package: publish into the registry.
    let p = e.run(&["publish", dir.to_str().unwrap()]);
    assert!(
        text(&p).contains(&format!("published {ID}@1.0.0  sha256-tree-v1:")),
        "{}",
        text(&p)
    );
    assert!(text(&e.run(&["search", "MYCLI"])).contains(&format!("{ID}@1.0.0")));
    assert!(text(&e.run(&["search", "nothing"])).contains("no extension matches"));

    // Install, list, verify, test by id.
    assert!(text(&e.run(&["install", ID])).contains("installed"));
    let l = e.run(&["list"]);
    assert!(
        text(&l).contains(&format!("{ID}  active 1.0.0")),
        "{}",
        text(&l)
    );
    assert!(text(&e.run(&["verify", ID])).starts_with(&format!("ok {ID} 1.0.0")));
    assert_eq!(e.run(&["test", ID, "--fixtures"]).status.code(), Some(0));

    // Remove.
    assert!(e.run(&["remove", ID]).status.success());
    assert!(text(&e.run(&["list"])).contains("no extensions installed"));
}

#[test]
fn update_pin_disable_rollback_and_the_exit_codes() {
    let e = Env::new();
    e.publish("1.0.0");
    assert!(e.run(&["install", &format!("{ID}@1.0.0")]).status.success());
    e.publish("1.1.0");
    assert!(e.run(&["pin", ID]).status.success());
    assert_eq!(
        e.run(&["update", ID]).status.code(),
        Some(6),
        "a pin refuses"
    );
    assert!(e.run(&["unpin", ID]).status.success());
    assert!(text(&e.run(&["update", ID])).contains("1.1.0"));
    assert_eq!(
        e.run(&["update", ID]).status.code(),
        Some(2),
        "nothing newer is a usage error"
    );
    assert!(text(&e.run(&["rollback", ID])).contains("1.0.0"));

    assert!(e.run(&["disable", ID]).status.success());
    assert_eq!(e.run(&["test", ID, "--fixtures"]).status.code(), Some(6));
    assert!(text(&e.run(&["list"])).contains("disabled"));
    assert!(e.run(&["enable", ID]).status.success());
    assert_eq!(e.run(&["test", ID, "--fixtures"]).status.code(), Some(0));

    assert_eq!(
        e.run(&["install", &format!("{ID}@1.x")]).status.code(),
        Some(2)
    );
    assert_eq!(
        e.run(&["install", "org.thirdparty.aibus.absent"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(e.run(&["remove", "../../etc"]).status.code(), Some(2));
}

#[test]
fn a_revoked_package_is_quarantined_not_installed_and_the_list_says_so() {
    let e = Env::new();
    e.publish("1.0.0");
    let index = e.reg.join("aibus-registry.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&index).unwrap()).unwrap();
    v["revoked"] = serde_json::json!([{ "id": ID, "reason": "withdrawn by the publisher" }]);
    fs::write(&index, serde_json::to_vec(&v).unwrap()).unwrap();
    let out = e.run(&["install", ID]);
    assert_eq!(out.status.code(), Some(6));
    assert!(
        common::stderr(&out).contains("quarantined (revoked)"),
        "{}",
        common::stderr(&out)
    );
    let l = text(&e.run(&["list"]));
    assert!(
        l.contains("active none") && l.contains("quarantined:1"),
        "{l}"
    );
}

#[test]
fn a_package_changed_after_it_was_published_is_quarantined_as_provenance_invalid() {
    let e = Env::new();
    e.publish("1.0.0");
    fs::write(e.reg.join(ID).join("1.0.0/README.md"), "tampered").unwrap();
    let out = e.run(&["install", ID]);
    assert_eq!(out.status.code(), Some(6));
    assert!(
        common::stderr(&out).contains("provenance_invalid"),
        "{}",
        common::stderr(&out)
    );
}

#[test]
fn a_failing_fixture_exits_1_and_names_the_check() {
    let e = Env::new();
    let dir = e.home.path().join("bad");
    package(&dir, "1.0.0");
    fs::write(
        dir.join("fixtures/basic.expected.json"),
        r#"{"ending":"exit:0","status":"failed","kinds":[]}"#,
    )
    .unwrap();
    let out = e.run(&["test", dir.to_str().unwrap(), "--fixtures"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out).contains("FAIL  basic: matches its expected events"),
        "{}",
        text(&out)
    );
}

#[test]
fn json_output_is_one_object_per_command() {
    let e = Env::new();
    e.publish("1.0.0");
    e.run(&["install", ID]);
    let reg = e.reg.to_str().unwrap();
    let l = aibus(
        e.home.path(),
        &["--json", "extensions", "--registry", reg, "list"],
    );
    let v: serde_json::Value = serde_json::from_slice(&l.stdout).unwrap();
    assert_eq!(v["extensions"][ID]["active"], "1.0.0");
    let t = aibus(
        e.home.path(),
        &[
            "--json",
            "extensions",
            "--registry",
            reg,
            "test",
            ID,
            "--fixtures",
        ],
    );
    let v: serde_json::Value = serde_json::from_slice(&t.stdout).unwrap();
    assert_eq!(
        (v["id"].as_str(), v["passed"].as_bool()),
        (Some(ID), Some(true))
    );
}

#[test]
fn a_registry_that_is_a_link_is_refused() {
    let e = Env::new();
    e.publish("1.0.0");
    let alias = e.home.path().join("alias");
    std::os::unix::fs::symlink(&e.reg, &alias).unwrap();
    let out = aibus(
        e.home.path(),
        &[
            "extensions",
            "--registry",
            alias.to_str().unwrap(),
            "search",
            "x",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
}

/// The ratified id installs and passes its recorded streams through the same commands.
#[test]
fn the_reference_extension_publishes_installs_and_tests_like_any_other() {
    let e = Env::new();
    let dir = repo().join("examples/extensions/reference-cli");
    assert!(e.run(&["publish", dir.to_str().unwrap()]).status.success());
    assert!(e
        .run(&["install", "org.devshell.aibus.reference-cli"])
        .status
        .success());
    let t = e.run(&["test", "org.devshell.aibus.reference-cli", "--fixtures"]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t));
    assert!(text(&t).contains("pass  native-subagent: matches its expected events"));
}

/// The spec's command list and `aibus extensions --help` say the same thing.
#[test]
fn the_spec_names_every_extensions_subcommand_and_flag() {
    let home = tempfile::tempdir().unwrap();
    let help = String::from_utf8(aibus(home.path(), &["extensions", "--help"]).stdout).unwrap();
    let verbs: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter(|v| *v != "help")
        .collect();
    assert!(verbs.len() >= 13, "{verbs:?}");
    let spec = fs::read_to_string(repo().join("docs/spec/16-extensions.md")).unwrap();
    let cli = fs::read_to_string(repo().join("docs/spec/06-cli.md")).unwrap();
    for v in verbs {
        assert!(
            spec.contains(v),
            "docs/spec/16-extensions.md does not mention `{v}`"
        );
        assert!(
            cli.lines()
                .any(|l| l.starts_with("aibus extensions") && l.contains(v)),
            "docs/spec/06-cli.md does not list `{v}`"
        );
    }
    for flag in ["--registry", "--accept", "--fixtures"] {
        assert!(spec.contains(flag), "{flag}");
    }
}

// ---- #332 review ----

#[test]
fn verify_fails_and_says_so_when_the_registry_cannot_be_read() {
    let e = Env::new();
    e.publish("1.0.0");
    assert!(e.run(&["install", ID]).status.success());
    fs::remove_file(e.reg.join("aibus-registry.json")).unwrap();
    let out = e.run(&["verify", ID]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(!text(&out).starts_with("ok"), "{}", text(&out));
    assert!(
        common::stderr(&out).contains("revocations"),
        "{}",
        common::stderr(&out)
    );
}

/// What gets installed is shown with the program it would run and whether the host would run it.
#[test]
fn install_shows_the_program_the_argv_and_whether_it_is_on_the_allowlist() {
    let e = Env::new();
    e.publish("1.0.0");
    let out = text(&e.run(&["install", ID]));
    assert!(
        out.contains("program examplecli: not on the host's allowlist"),
        "{out}"
    );
    assert!(out.contains("argv: run --json"), "{out}");
    let dir = repo().join("examples/extensions/reference-cli");
    e.run(&["publish", dir.to_str().unwrap()]);
    let out = text(&e.run(&["install", "org.devshell.aibus.reference-cli"]));
    assert!(
        out.contains("program claude: on the host's allowlist"),
        "{out}"
    );
}

#[test]
fn the_docs_say_what_the_sandbox_is_and_is_not() {
    let spec = fs::read_to_string(repo().join("docs/spec/16-extensions.md")).unwrap();
    let sec = fs::read_to_string(repo().join("docs/SECURITY.md")).unwrap();
    let dec = fs::read_to_string(repo().join("docs/DECISIONS.md")).unwrap();
    for (name, doc) in [("spec 16", &spec), ("SECURITY", &sec)] {
        assert!(doc.contains("not an operating system sandbox"), "{name}");
        assert!(
            doc.contains("`HOME`"),
            "{name}: files under HOME are reachable"
        );
    }
    assert!(spec.contains("claude, codex, gemini, kimi") && spec.contains("extra_official_clis"));
    assert!(dec.contains("claude, codex, gemini, kimi") && dec.contains("default"));
    assert!(
        !spec.contains("least privilege"),
        "the sandbox is not least privilege"
    );
    assert!(!sec.contains("no ambient credential"));
}
