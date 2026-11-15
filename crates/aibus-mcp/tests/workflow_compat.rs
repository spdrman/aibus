//! #283: the read tools an older client already calls keep their names, their schemas and their
//! answers for every workflow that existed before the compiler. The expected file was written by
//! the server before #283 (`AIBUS_BLESS=1` rewrites it, which is only right when a change to one
//! of these tools is meant).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aibus_core::Redactor;
use aibus_mcp::Caps;
use serde_json::{json, Value};

const OLD_TOOLS: [&str; 6] = [
    "workflow_list",
    "workflow_get",
    "workflow_explain",
    "workflow_validate",
    "workflow_dry_run",
    "agent_list",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn examples() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(root().join("examples/workflows"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            (stem, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    out.sort();
    out
}

fn answer(server: &aibus_mcp::Server, tool: &str, args: Value) -> Value {
    let r = server
        .call(None, tool, args.as_object().cloned(), Instant::now())
        .unwrap();
    json!({
        "is_error": r.is_error,
        "structured": r.structured_content,
        "text": r.content.iter().map(|c| serde_json::to_value(c).unwrap()).collect::<Vec<_>>(),
    })
}

#[test]
fn the_tools_older_clients_call_answer_exactly_as_before() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let dir = home.path().join("proj/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in examples() {
        std::fs::write(dir.join(format!("{name}.toml")), text).unwrap();
    }
    let mut got: BTreeMap<String, Value> = BTreeMap::new();
    let tools: BTreeMap<String, Value> = server
        .tools()
        .into_iter()
        .filter(|t| OLD_TOOLS.contains(&t.name.as_ref()))
        .map(|t| (t.name.to_string(), serde_json::to_value(&t).unwrap()))
        .collect();
    assert_eq!(tools.len(), OLD_TOOLS.len());
    got.insert("tools".into(), json!(tools));
    for (name, text) in examples() {
        let by_name = json!({ "name": name });
        got.insert(
            format!("{name}/get"),
            answer(&server, "workflow_get", by_name.clone()),
        );
        got.insert(
            format!("{name}/explain"),
            answer(&server, "workflow_explain", by_name.clone()),
        );
        got.insert(
            format!("{name}/validate"),
            answer(&server, "workflow_validate", by_name),
        );
        got.insert(
            format!("{name}/validate-draft"),
            answer(&server, "workflow_validate", json!({ "toml": text })),
        );
        got.insert(
            format!("{name}/dry_run"),
            answer(
                &server,
                "workflow_dry_run",
                json!({ "name": name, "input": "INPUT" }),
            ),
        );
    }
    let expected_path = root().join("fixtures/mcp-server/compat-before-283.json");
    if std::env::var_os("AIBUS_BLESS").is_some() {
        let text = serde_json::to_string_pretty(&got).unwrap() + "\n";
        std::fs::write(&expected_path, text).unwrap();
    }
    let expected: BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(&expected_path).unwrap()).unwrap();
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (key, value) in &got {
        assert_eq!(value, &expected[key], "{key}");
    }
}
