//! `aibus agent list|show|new|validate|where` (06 sections 1 and 4).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{aibus, command, stderr};
use serde_json::Value;
use tempfile::TempDir;

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/agents")
}

fn proj(home: &Path) -> PathBuf {
    let p = home.join("proj");
    fs::create_dir_all(p.join(".aibus/agents")).unwrap();
    p
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn list_shows_the_nine_built_ins_with_the_json_shape() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["agent", "list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rows = json(&out);
    let rows = rows.as_array().unwrap();
    let names: Vec<_> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["lead", "explore", "plan", "general", "advisor", "router", "search", "grep", "audit"]
    );
    let explore = &rows[1];
    assert_eq!(
        explore,
        &serde_json::json!({
            "name": "explore",
            "scope": "builtin",
            "path": null,
            "role": "worker",
            "model": "inherit",
            "tools": ["read", "grep", "glob", "ls"],
            "description": "Read-only codebase search. Say how thorough to be (quick, medium, very_thorough).",
        })
    );
    assert_eq!(rows[0]["tools"], Value::Null, "all tools is null");
    assert_eq!(rows[8]["tools"][3], "bash(git *)");
}

#[test]
fn list_follows_the_precedence_and_names_is_one_per_line() {
    let home = TempDir::new().unwrap();
    let p = proj(home.path());
    write(
        &p.join(".aibus/agents/explore.md"),
        "---\nname: explore\ndescription: Mine.\nmodel: sonnet\n---\nBody",
    );
    write(
        &home.path().join(".config/aibus/agents/helper.md"),
        "---\nname: helper\ndescription: Helps.\n---\nBody",
    );
    let out = aibus(home.path(), &["agent", "list", "--json"]);
    let rows = json(&out);
    let explore = rows.as_array().unwrap().iter().find(|r| r["name"] == "explore").unwrap();
    assert_eq!(explore["scope"], "project");
    assert_eq!(explore["model"], "sonnet");
    assert_eq!(explore["path"], p.join(".aibus/agents/explore.md").to_str().unwrap());
    let helper = rows.as_array().unwrap().iter().find(|r| r["name"] == "helper").unwrap();
    assert_eq!(helper["scope"], "user");

    let out = aibus(home.path(), &["agent", "list", "--names"]);
    let names = stdout(&out);
    assert_eq!(names.lines().count(), 10);
    assert!(names.lines().any(|l| l == "helper") && names.lines().any(|l| l == "audit"));

    let out = aibus(home.path(), &["agent", "list"]);
    assert!(stdout(&out).contains("explore\tproject\tworker\tsonnet\tMine.\n"), "{}", stdout(&out));
}

#[test]
fn list_includes_cli_definitions_first() {
    let home = TempDir::new().unwrap();
    let out = aibus(
        home.path(),
        &["--agents", r#"{"reviewer":{"description":"d","prompt":"p"}}"#, "agent", "list", "--names"],
    );
    assert_eq!(stdout(&out).lines().next(), Some("reviewer"));
}

#[test]
fn show_prints_a_built_in_as_a_file_you_can_copy_and_as_json() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["agent", "show", "explore"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.starts_with("---\nname: explore\n"), "{text}");
    assert!(text.contains("\ntools: [\"read\",\"grep\",\"glob\",\"ls\"]\n"), "{text}");
    assert!(text.contains("\nomit_project_rules: true\n"));
    assert!(text.contains("\n---\n\nYou search a codebase"));

    // Copying it into a project gives the same definition back.
    let p = proj(home.path());
    write(&p.join(".aibus/agents/explore.md"), &text);
    let again = aibus(home.path(), &["agent", "show", "explore"]);
    assert_eq!(stdout(&again), text);
    let v = aibus(home.path(), &["agent", "validate", ".aibus/agents/explore.md"]);
    assert_eq!(v.status.code(), Some(0), "{}", stdout(&v));

    let out = aibus(home.path(), &["agent", "show", "audit", "--json"]);
    let j = json(&out);
    assert_eq!(j["isolation"], "worktree");
    assert_eq!(j["model"], "swarm_model");
    assert!(j["prompt"].as_str().unwrap().starts_with("You run the project's checks"));
    assert!(j.as_object().unwrap().keys().all(|k| k.chars().all(|c| !c.is_ascii_uppercase())));
}

#[test]
fn show_of_an_unknown_or_malformed_name_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    for name in ["nobody", "Bad:Name"] {
        let out = aibus(home.path(), &["agent", "show", name]);
        assert_eq!(out.status.code(), Some(2), "{name}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn new_writes_a_template_in_the_project_and_refuses_to_overwrite() {
    let home = TempDir::new().unwrap();
    let p = proj(home.path());
    // No terminal here, so no editor is started even with one set.
    let out = command(home.path())
        .env("EDITOR", "false")
        .args(["agent", "new", "reviewer"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let file = p.join(".aibus/agents/reviewer.md");
    assert_eq!(stdout(&out), format!("wrote {}\n", file.display()));
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("---\nname: reviewer\n"));

    let v = aibus(home.path(), &["agent", "validate", ".aibus/agents/reviewer.md"]);
    assert_eq!(v.status.code(), Some(0), "{}", stdout(&v));
    let show = aibus(home.path(), &["agent", "show", "reviewer", "--json"]);
    assert_eq!(json(&show)["role"], "worker");

    let again = aibus(home.path(), &["agent", "new", "reviewer"]);
    assert_eq!(again.status.code(), Some(2));
    assert_eq!(fs::read_to_string(&file).unwrap(), text);
    assert_eq!(aibus(home.path(), &["agent", "new", "Not Valid"]).status.code(), Some(2));
}

#[test]
fn new_without_a_project_goes_to_the_user_scope() {
    let home = TempDir::new().unwrap();
    // `proj` exists (the working directory) but has neither `.aibus` nor `.git`, and the
    // temp dir above it has none either.
    let out = aibus(home.path(), &["agent", "new", "solo"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(home.path().join(".config/aibus/agents/solo.md").is_file());
}

#[test]
fn where_lists_the_directories_in_precedence_order() {
    let home = TempDir::new().unwrap();
    let p = proj(home.path());
    write(
        &p.join(".aibus/settings.toml"),
        "[agents]\nextra_dirs = [\".claude/agents\"]\n",
    );
    let out = aibus(home.path(), &["agent", "where"]);
    let h = home.path().display();
    assert_eq!(
        stdout(&out),
        format!(
            "project\t{h}/proj/.aibus/agents\nuser\t{h}/.config/aibus/agents\nextra\t{h}/proj/.claude/agents\nbuiltin\t(compiled in)\n"
        )
    );
    let out = aibus(home.path(), &["agent", "where", "--json"]);
    let rows = json(&out);
    assert_eq!(rows[0]["scope"], "project");
    assert_eq!(rows[0]["exists"], true);
    assert_eq!(rows[2]["exists"], false);
    assert_eq!(rows[3], serde_json::json!({"scope": "builtin", "path": null, "exists": null}));
}

#[test]
fn extra_dirs_load_without_hooks_and_the_warning_goes_to_stderr() {
    let home = TempDir::new().unwrap();
    let p = proj(home.path());
    write(&p.join(".aibus/settings.toml"), "[agents]\nextra_dirs = [\".claude/agents\"]\n");
    write(
        &p.join(".claude/agents/wild.md"),
        "---\nname: wild\ndescription: d\npermissionMode: bypass\nhooks:\n  stop:\n    - command: ./x.sh\n---\n",
    );
    let out = aibus(home.path(), &["agent", "show", "wild", "--json"]);
    let j = json(&out);
    assert_eq!(j["permission_mode"], "default");
    assert!(j.get("hooks").is_none());
    let err = stderr(&out);
    assert!(err.contains("wild: hooks ignored"), "{err}");
    assert!(err.contains("wild: permission_mode reset to default"), "{err}");
}

#[cfg(unix)]
fn corpus_home(accounts: &str) -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().unwrap();
    let p = proj(home.path());
    let corpus = p.join("corpus");
    for dir in ["valid", "invalid", "warnings", "semantic"] {
        fs::create_dir_all(corpus.join(dir)).unwrap();
        for f in fs::read_dir(fixtures().join(dir)).unwrap() {
            let f = f.unwrap().path();
            fs::copy(&f, corpus.join(dir).join(f.file_name().unwrap())).unwrap();
        }
    }
    for (name, mode) in [("readonly-guard.sh", 0o755), ("plain.sh", 0o644)] {
        let s = p.join("scripts").join(name);
        write(&s, "#!/bin/sh\n");
        fs::set_permissions(&s, fs::Permissions::from_mode(mode)).unwrap();
    }
    write(&home.path().join(".config/aibus/config.toml"), accounts);
    home
}

/// `AIBUS_BLESS=1` rewrites the expected files from what the binary prints.
fn expect(name: &str, actual: &str) {
    let path = fixtures().join("expected").join(name);
    if std::env::var_os("AIBUS_BLESS").is_some() {
        fs::write(&path, actual).unwrap();
    }
    let want = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(actual, want, "{name}");
}

/// The home is a temp dir, so its path is swapped for `$HOME` to keep the files stable.
fn scrub(text: &str, home: &Path) -> String {
    text.replace(&home.display().to_string(), "$HOME")
}

/// An `anthropic` account is for a provider this build does not have, so every model gets the
/// benefit of the doubt; a `fake` account only knows its own two models.
#[cfg(unix)]
#[test]
fn validate_on_the_fixture_corpus_matches_the_expected_output() {
    let unverifiable = "[[account]]\nname = \"anthropic-work\"\nprovider = \"anthropic\"\n";
    let fake_only = "[[account]]\nname = \"work\"\nprovider = \"fake\"\n";
    let cases = [
        ("valid", unverifiable, 0),
        ("warnings", unverifiable, 0),
        ("invalid", unverifiable, 12),
        ("semantic", fake_only, 12),
    ];
    for (dir, config, code) in cases {
        let home = corpus_home(config);
        let path = format!("corpus/{dir}");
        let text = aibus(home.path(), &["agent", "validate", &path]);
        let js = aibus(home.path(), &["agent", "validate", &path, "--json"]);
        assert_eq!(text.status.code(), Some(code), "{dir}: {}{}", stdout(&text), stderr(&text));
        assert_eq!(js.status.code(), Some(code), "{dir}");
        assert!(stderr(&text).is_empty() && stderr(&js).is_empty(), "{dir}: {}", stderr(&text));
        expect(&format!("{dir}.txt"), &scrub(&stdout(&text), home.path()));
        expect(&format!("{dir}.json"), &scrub(&stdout(&js), home.path()));
        assert_eq!(json(&js)["ok"], code == 0, "{dir}");
    }
}

#[test]
fn validate_with_no_path_checks_every_scope_and_exits_12_on_errors() {
    let home = TempDir::new().unwrap();
    let p = proj(home.path());
    let out = aibus(home.path(), &["agent", "validate", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "{\"ok\":true,\"errors\":[],\"warnings\":[]}\n");

    write(&p.join(".aibus/agents/bad.md"), "---\nname: bad\n---\n");
    write(
        &home.path().join(".config/aibus/agents/odd.md"),
        "---\nname: odd\ndescription: d\nflavour: x\n---\n",
    );
    let out = aibus(home.path(), &["agent", "validate", "--json"]);
    assert_eq!(out.status.code(), Some(12));
    let j = json(&out);
    assert_eq!(j["ok"], false);
    assert_eq!(j["errors"][0]["field"], "description");
    assert_eq!(j["errors"][0]["path"], p.join(".aibus/agents/bad.md").to_str().unwrap());
    assert_eq!(j["warnings"][0]["field"], "flavour");
}
