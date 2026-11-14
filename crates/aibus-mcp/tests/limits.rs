mod common;

use std::time::{Duration, Instant};

use aibus_core::Redactor;
use aibus_mcp::{Caps, CALLS_PER_MINUTE, MAX_ARGS_BYTES};
use serde_json::json;

fn text(result: &rmcp::model::CallToolResult) -> String {
    serde_json::to_value(result).unwrap()["content"][0]["text"].as_str().unwrap().to_owned()
}

#[test]
fn the_sixty_first_call_in_a_minute_is_an_error_with_a_retry_hint() {
    let (server, _home) = common::server(Caps::default(), Redactor::new());
    let t0 = Instant::now();
    for i in 0..CALLS_PER_MINUTE {
        let r = server.call(None, "workflow_list", None, t0).unwrap();
        assert_ne!(r.is_error, Some(true), "call {i}");
    }
    let limited = server.call(None, "workflow_list", None, t0).unwrap();
    assert_eq!(limited.is_error, Some(true));
    assert!(text(&limited).contains("try again in 1 s"), "{}", text(&limited));
    // Half a second later there is still no token; a second later there is one.
    let still = server.call(None, "workflow_list", None, t0 + Duration::from_millis(500)).unwrap();
    assert_eq!(still.is_error, Some(true));
    let again = server.call(None, "workflow_list", None, t0 + Duration::from_secs(1)).unwrap();
    assert_ne!(again.is_error, Some(true));
}

#[test]
fn an_argument_over_256_kib_is_a_protocol_error() {
    let (server, _home) = common::server(Caps::default(), Redactor::new());
    let big = json!({ "blob": "a".repeat(MAX_ARGS_BYTES + 1) }).as_object().cloned();
    let e = server.call(None, "workflow_list", big, Instant::now()).unwrap_err();
    assert_eq!(e.code.0, -32602);
    assert!(e.message.contains("256 KiB"), "{}", e.message);
}

#[test]
fn a_refused_call_does_not_spend_a_token() {
    let (server, _home) = common::server(Caps::default(), Redactor::new());
    let t0 = Instant::now();
    for _ in 0..(CALLS_PER_MINUTE * 2) {
        server.call(None, "no_such_tool", None, t0).unwrap_err();
    }
    assert_ne!(server.call(None, "workflow_list", None, t0).unwrap().is_error, Some(true));
}
