mod common;

use std::fs;
use std::time::Instant;

use aibus_core::Redactor;
use aibus_mcp::Caps;
use serde_json::json;

#[test]
fn paths_are_relative_to_the_project_or_the_home_and_broken_files_say_why() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let project = home.path().join("proj/.aibus/workflows");
    let user = home.paths.config.join("workflows");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::write(
        project.join("ship.toml"),
        "description = \"Plan, build\"\n[[step]]\nagent = \"plan\"\nprompt = \"{{input}}\"\n",
    )
    .unwrap();
    fs::write(project.join("broken.toml"), "this is = = not toml\n").unwrap();
    fs::write(user.join("mine.toml"), "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n").unwrap();

    let result = server.call(None, "workflow_list", None, Instant::now()).unwrap();
    let rows = result.structured_content.unwrap()["workflows"].clone();
    let rows = rows.as_array().unwrap();
    let by = |n: &str| rows.iter().find(|r| r["name"] == n).unwrap_or_else(|| panic!("{n} in {rows:?}"));

    assert_eq!(by("ship")["scope"], "project");
    assert_eq!(by("ship")["path"], ".aibus/workflows/ship.toml");
    assert_eq!(by("ship")["ok"], true);
    assert_eq!(by("ship")["description"], "Plan, build");
    assert_eq!(by("broken")["ok"], false);
    assert!(by("broken")["error"].as_str().is_some_and(|e| !e.is_empty()), "{rows:?}");
    assert_eq!(by("mine")["scope"], "user");
    assert!(by("mine")["path"].as_str().unwrap().starts_with("~/"), "{rows:?}");
    // No absolute path of the machine anywhere in the answer.
    let all = json!(rows).to_string();
    assert!(!all.contains(&home.path().display().to_string()), "{all}");
}
