mod common;

use std::time::Instant;

use aibus_core::{AccountName, Redactor};
use aibus_mcp::Caps;
use rmcp::model::Implementation;
use serde_json::json;

const KEY: &str = "live-credential-4d5e6f7a8b";

fn args(v: serde_json::Value) -> Option<serde_json::Map<String, serde_json::Value>> {
    v.as_object().cloned()
}

#[test]
fn every_call_leaves_one_redacted_line_with_the_client_name() {
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), KEY);
    let (server, mut home) = common::server(Caps::default(), redactor);
    home.forbid(KEY);
    let client = Implementation::new("conformance-client", "9.9.9");

    // One call that works, one with the key in its arguments (the tool takes none, so it is
    // refused), one for a tool that is not there with the key in its name.
    server.call(Some(&client), "workflow_list", None, Instant::now()).unwrap();
    let refused = server
        .call(Some(&client), "workflow_list", args(json!({ "note": KEY })), Instant::now())
        .unwrap_err();
    let unknown = server.call(Some(&client), &format!("tool-{KEY}"), None, Instant::now()).unwrap_err();
    for e in [&refused, &unknown] {
        assert!(!e.message.contains(KEY), "{e:?}");
    }

    let lines = common::audit_lines(&home);
    // The start line, then one per call.
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[0]["method"], "server/start");
    assert_eq!(lines[0]["args"]["write"], false);
    for (line, ok) in lines[1..].iter().zip([true, false, false]) {
        assert_eq!(line["client"], json!({ "name": "conformance-client", "version": "9.9.9" }));
        assert_eq!(line["method"], "tools/call");
        assert_eq!(line["ok"], ok, "{line}");
        assert!(line["ms"].is_u64() && line["t"].as_str().is_some_and(|t| t.ends_with('Z')), "{line}");
    }
    assert_eq!(lines[1]["tool"], "workflow_list");
    let text = lines.iter().map(ToString::to_string).collect::<String>();
    assert!(text.contains("[redacted:work]"), "{text}");
    assert!(!text.contains(KEY), "{text}");
}

#[test]
fn the_audit_file_is_private_and_arguments_are_capped_at_4_kib() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let big = args(json!({ "blob": "x".repeat(10_000) }));
    server.call(None, "workflow_list", big, Instant::now()).unwrap_err();
    let lines = common::audit_lines(&home);
    let kept = lines[1]["args"].as_str().expect("a capped argument is kept as text");
    assert!(kept.len() <= 4096 + 3, "{}", kept.len());
    assert!(kept.ends_with("..."));
    assert!(lines[1]["client"].is_null());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = std::fs::read_dir(home.paths.state.join("logs/mcp")).unwrap().next().unwrap().unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}
