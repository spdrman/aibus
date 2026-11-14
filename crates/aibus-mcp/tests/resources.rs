mod common;

use std::fs;

use aibus_core::{AccountName, Redactor};
use aibus_mcp::Caps;

const KEY: &str = "live-credential-4d5e6f7a8b";

fn read(server: &aibus_mcp::Server, uri: &str) -> Result<(&'static str, String), i32> {
    server.read_resource_text(uri).map_err(|e| e.code.0)
}

#[test]
fn the_three_static_resources_and_one_template_are_listed() {
    let uris = || {
        aibus_mcp::resources::list()
            .into_iter()
            .map(|r| r.uri.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        uris(),
        [
            "aibus://workflows",
            "aibus://schema/workflow",
            "aibus://agents"
        ]
    );
    let templates: Vec<_> = aibus_mcp::resources::templates()
        .into_iter()
        .map(|t| t.uri_template.clone())
        .collect();
    assert_eq!(templates, ["aibus://workflows/{scope}/{name}"]);
    assert_eq!(uris(), uris(), "the listing is deterministic");
}

#[test]
fn reading_a_workflow_returns_toml_bytes_with_the_toml_mime() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    let toml = "description = \"d\"\n[[step]]\nagent = \"plan\"\nprompt = \"{{input}}\"\n";
    fs::write(dir.join("ship.toml"), toml).unwrap();
    assert_eq!(
        read(&server, "aibus://workflows/project/ship"),
        Ok(("application/toml", toml.to_owned()))
    );
    let (mime, list) = read(&server, "aibus://workflows").unwrap();
    assert_eq!(mime, "application/json");
    assert!(list.contains("\"ship\""), "{list}");
    let (mime, agents) = read(&server, "aibus://agents").unwrap();
    assert_eq!(mime, "application/json");
    assert!(agents.contains("\"explore\""), "{agents}");
    // A workflow that is not there is the same error as any other bad uri.
    assert_eq!(read(&server, "aibus://workflows/user/ship"), Err(-32602));
}

#[test]
fn the_schema_resource_is_the_file_in_docs() {
    let (server, _home) = common::server(Caps::default(), Redactor::new());
    let (mime, body) = read(&server, "aibus://schema/workflow").unwrap();
    assert_eq!(mime, "application/schema+json");
    let file = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/schemas/workflow/v1.json"
    ))
    .unwrap();
    assert_eq!(body.as_bytes(), file.as_bytes());
}

#[test]
fn a_path_shaped_name_is_invalid_params() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    fs::write(home.path().join("secret.toml"), "x").unwrap();
    for uri in [
        "aibus://workflows/project/../x",
        "aibus://workflows/project/%2e%2e",
        "aibus://workflows/project/..%2fsecret",
        "aibus://workflows/project/a/b",
        "aibus://workflows/cwd/x",
        "aibus://config",
        "aibus://runs/anything",
        "aibus://",
        "file:///etc/passwd",
    ] {
        assert_eq!(read(&server, uri), Err(-32602), "{uri}");
    }
}

#[test]
fn a_registered_key_in_an_agent_description_is_redacted_in_prompts_and_resources() {
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), KEY);
    let (server, mut home) = common::server(Caps::default(), redactor);
    home.forbid(KEY);
    let agents = home.path().join("proj/.aibus/agents");
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&agents).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        agents.join("leaky.md"),
        format!("---\nname: leaky\ndescription: key is {KEY}\n---\nBody.\n"),
    )
    .unwrap();
    fs::write(
        dir.join("w.toml"),
        format!("description = \"{KEY}\"\n[[step]]\nagent = \"plan\"\nprompt = \"{KEY}\"\n"),
    )
    .unwrap();
    for uri in [
        "aibus://agents",
        "aibus://workflows",
        "aibus://workflows/project/w",
    ] {
        assert!(!read(&server, uri).unwrap().1.contains(KEY), "{uri}");
    }
    for (name, args) in [
        ("author_workflow", serde_json::json!({ "goal": KEY })),
        ("review_workflow", serde_json::json!({ "name": "w" })),
    ] {
        let got = server
            .get_prompt_text(name, args.as_object().unwrap())
            .unwrap();
        assert!(!format!("{got:?}").contains(KEY), "{name}");
    }
    // The fixtures hold the key on purpose; the scan of the temp tree at the end is for what the
    // server wrote.
    fs::remove_file(agents.join("leaky.md")).unwrap();
    fs::remove_file(dir.join("w.toml")).unwrap();
}
