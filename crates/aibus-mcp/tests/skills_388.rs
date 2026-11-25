//! H1.1 (#388): `agent_list` over MCP carries the skills of the project and the user.

mod common;

use std::fs;
use std::time::Instant;

use aibus_core::Redactor;
use aibus_mcp::Caps;
use serde_json::{json, Value};

fn skill(root: &std::path::Path, name: &str, description: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nbody\n"),
    )
    .unwrap();
}

#[test]
fn agent_list_has_the_skills_and_what_each_agent_preloads() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    skill(
        &home.path().join("proj/.aibus/skills"),
        "deploy",
        "Ship a release",
    );
    skill(&home.paths.config.join("skills"), "notes", "Take notes");
    let agents = home.path().join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("a.md"),
        "---\nname: a\ndescription: d\nskills: [deploy]\n---\nDo.\n",
    )
    .unwrap();

    let r = server
        .call(
            None,
            "agent_list",
            json!({}).as_object().cloned(),
            Instant::now(),
        )
        .unwrap();
    let list: Value = r.structured_content.expect("structured content");
    let skills = list["skills"].as_array().unwrap();
    let names: Vec<_> = skills.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["deploy", "notes"]);
    assert_eq!(skills[0]["scope"], "project");
    assert_eq!(skills[0]["description"], "Ship a release");
    assert_eq!(skills[1]["scope"], "user");
    let a = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "a")
        .unwrap();
    assert_eq!(a["skills"], json!(["deploy"]));
}
