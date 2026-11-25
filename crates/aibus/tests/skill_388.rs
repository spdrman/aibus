//! H1.1 (#388): `aibus skill list|show|validate|import` and what `agent validate` says about skills.

mod common;

use std::fs;
use std::path::Path;

use aibus_testkit::xdg_temp;
use common::{aibus, stderr};
use serde_json::Value;

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn skill(root: &Path, name: &str, description: &str, body: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
    )
    .unwrap();
}

/// A project at `home/proj` and the user skills directory under the home.
fn setup(home: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let project = home.join("proj/.aibus/skills");
    let user = home.join(".config/aibus/skills");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&user).unwrap();
    (project, user)
}

#[test]
fn list_shows_the_winning_path_and_what_it_shadows() {
    let home = xdg_temp();
    let (project, user) = setup(home.path());
    skill(&project, "lint", "Project lint", "p");
    skill(&user, "lint", "User lint", "u");
    skill(&user, "notes", "Take notes", "n");
    let out = aibus(home.path(), &["skill", "list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rows: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "lint");
    assert_eq!(rows[0]["scope"], "project");
    assert!(rows[0]["path"]
        .as_str()
        .unwrap()
        .ends_with("proj/.aibus/skills/lint/SKILL.md"));
    assert_eq!(rows[0]["shadowed"].as_array().unwrap().len(), 1);
    assert_eq!(rows[1]["scope"], "user");
    let text = stdout(&aibus(home.path(), &["skill", "list"]));
    assert!(
        text.contains("lint\tproject\t") && text.contains("shadows"),
        "{text}"
    );
}

#[test]
fn show_prints_the_body_and_an_unknown_name_is_a_usage_error() {
    let home = xdg_temp();
    let (project, _) = setup(home.path());
    skill(&project, "deploy", "Ship it", "Run the deploy script.");
    let out = aibus(home.path(), &["skill", "show", "deploy"]);
    assert_eq!(stdout(&out).trim(), "Run the deploy script.");
    let missing = aibus(home.path(), &["skill", "show", "nope"]);
    assert_ne!(missing.status.code(), Some(0));
    assert!(
        stderr(&missing).contains("no skill named nope"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn validate_fails_on_a_duplicate_across_scopes_and_passes_on_a_clean_set() {
    let home = xdg_temp();
    let (project, user) = setup(home.path());
    skill(&project, "lint", "Project lint", "p");
    let clean = aibus(home.path(), &["skill", "validate"]);
    assert_eq!(
        clean.status.code(),
        Some(0),
        "{}{}",
        stdout(&clean),
        stderr(&clean)
    );
    skill(&user, "lint", "User lint", "u");
    let out = aibus(home.path(), &["skill", "validate", "--json"]);
    assert_eq!(out.status.code(), Some(12));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(report["ok"], false);
    assert!(report["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("duplicate skill name"));
}

#[test]
fn agent_validate_fails_on_an_unresolved_skill_and_on_a_duplicate() {
    let home = xdg_temp();
    let (project, user) = setup(home.path());
    skill(&project, "present", "Here", "b");
    let agents = home.path().join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("a.md"),
        "---\nname: a\ndescription: d\nmodel: inherit\nskills: [present, mispelled]\n---\nDo it.\n",
    )
    .unwrap();
    let out = aibus(home.path(), &["agent", "validate", "--json"]);
    assert_eq!(out.status.code(), Some(12), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(
        text.contains("mispelled") && text.contains("not found"),
        "{text}"
    );
    assert!(
        !text.contains("\"present\""),
        "a skill that resolves is not reported: {text}"
    );

    fs::write(
        agents.join("a.md"),
        "---\nname: a\ndescription: d\nmodel: inherit\nskills: [present]\n---\nDo it.\n",
    )
    .unwrap();
    assert_eq!(
        aibus(home.path(), &["agent", "validate"]).status.code(),
        Some(0)
    );
    skill(&user, "present", "Also here", "b");
    let dup = aibus(home.path(), &["agent", "validate", "--json"]);
    assert_eq!(dup.status.code(), Some(12));
    assert!(stdout(&dup).contains("duplicate skill name"));
}

#[test]
fn import_copies_claude_skills_into_the_project() {
    let home = xdg_temp();
    let proj = home.path().join("proj");
    fs::create_dir_all(proj.join(".aibus")).unwrap();
    skill(
        &proj.join(".claude/skills"),
        "imported",
        "From elsewhere",
        "body",
    );
    let out = aibus(home.path(), &["skill", "import", ".claude/skills"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(proj.join(".aibus/skills/imported/SKILL.md").is_file());
    let listed = stdout(&aibus(home.path(), &["skill", "list", "--names"]));
    assert_eq!(listed.trim(), "imported");
}
