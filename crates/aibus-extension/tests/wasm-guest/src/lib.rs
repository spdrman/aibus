//! A tiny stateful parser for the reference stream (`examples/extensions/template`): one JSON
//! object per line, `text`, `tool_start`, `tool_end`, `tool_error` and `usage`. What makes it
//! stateful: it remembers which tool uses are still open and, when the stream ends, says that their
//! outcome is unknown. A declarative manifest cannot express that without the host's help; here it
//! is a few lines of the guest's own.
//!
//! The host API (module `aibus`): `read_line`, `emit`, `next_seq`. The guest chooses kinds and data
//! only. The host stamps ids, citations and parser, and clamps origin and evidence level.

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde_json::{json, Value};

#[link(wasm_import_module = "aibus")]
extern "C" {
    fn read_line(ptr: *mut u8, cap: i32) -> i32;
    fn emit(ptr: *const u8, len: i32) -> i32;
    fn next_seq() -> i64;
}

const LINE_CAP: usize = 256 * 1024;
const MAX_OPEN: usize = 4_096;

thread_local! {
    static OPEN: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static LAST_SEQ: RefCell<i64> = const { RefCell::new(0) };
}

fn send(event: Value) -> bool {
    let bytes = event.to_string().into_bytes();
    unsafe { emit(bytes.as_ptr(), bytes.len() as i32) == 0 }
}

fn gap(reason: &str, bytes: usize) {
    send(json!({
        "kind": "gap_detected",
        "visibility": "unavailable",
        "data": { "reason": reason, "bytes": bytes, "channel": "stdout" },
    }));
}

fn text_at(v: &Value, pointer: &str) -> Option<String> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(|s| s.chars().take(256).collect())
}

fn metric(v: &Value, pointer: &str, name: &str) -> Value {
    let n = v
        .pointer(pointer)
        .and_then(Value::as_u64)
        .filter(|n| *n <= 1 << 53);
    json!({
        "name": name,
        "value": n,
        "unit": "tokens",
        "quality": if n.is_some() { "reported" } else { "unknown" },
        "source": "the CLI's own usage line",
    })
}

#[no_mangle]
pub extern "C" fn on_line() -> i32 {
    // The host's sequence numbers only ever go up: a guest that numbers its own work with them can
    // rely on that, and one that sees them go back knows the host is not the one it was built for.
    let seq = unsafe { next_seq() };
    let ok = LAST_SEQ.with(|l| std::mem::replace(&mut *l.borrow_mut(), seq) < seq);
    if !ok {
        return 2;
    }
    let mut buf = vec![0u8; LINE_CAP];
    let n = unsafe { read_line(buf.as_mut_ptr(), LINE_CAP as i32) };
    if n < 0 {
        gap("line_too_long", 0);
        return 0;
    }
    let line = &buf[..n as usize];
    let Ok(text) = std::str::from_utf8(line) else {
        gap("invalid_utf8", line.len());
        return 0;
    };
    let v: Value = match serde_json::from_str(text) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => return gap_and_ok("not_an_object", line.len()),
        Err(_) => return gap_and_ok("malformed_json", line.len()),
    };
    let kind = v.pointer("/type").and_then(Value::as_str).unwrap_or("");
    let native: String = kind.chars().take(128).collect();
    match kind {
        "text" => {
            send(
                json!({ "kind": "message_chunk", "native_type": native, "data": { "bytes": line.len() } }),
            );
        }
        "tool_start" | "tool_end" | "tool_error" => {
            let id = text_at(&v, "/id");
            if let Some(id) = &id {
                OPEN.with(|o| {
                    let mut o = o.borrow_mut();
                    if kind == "tool_start" {
                        if o.len() < MAX_OPEN {
                            o.insert(id.clone());
                        }
                    } else {
                        o.remove(id);
                    }
                });
            }
            let k = match kind {
                "tool_start" => "tool_started",
                "tool_end" => "tool_completed",
                _ => "tool_failed",
            };
            send(json!({ "kind": k, "native_type": native, "tool_use_id": id, "data": {} }));
        }
        "usage" => {
            send(json!({
                "kind": "usage_reported",
                "native_type": native,
                "data": {
                    "scope": "attempt",
                    "cumulative": false,
                    "metrics": [
                        metric(&v, "/usage/in", "input_tokens"),
                        metric(&v, "/usage/out", "output_tokens"),
                    ],
                },
            }));
        }
        _ => {
            let what: String = text_at(&v, "/type")
                .unwrap_or_default()
                .chars()
                .take(64)
                .collect();
            send(json!({
                "kind": "trace_warning",
                "data": { "reason": "unknown_variant", "what": what },
            }));
        }
    }
    0
}

fn gap_and_ok(reason: &str, bytes: usize) -> i32 {
    gap(reason, bytes);
    0
}

#[no_mangle]
pub extern "C" fn on_finish() -> i32 {
    // Whatever is still open never reported an outcome: say so, once each, in a stable order.
    let open = OPEN.with(|o| std::mem::take(&mut *o.borrow_mut()));
    for id in open {
        send(json!({
            "kind": "gap_detected",
            "origin": "derived",
            "visibility": "unavailable",
            "tool_use_id": id,
            "data": { "reason": "tool_outcome_unknown", "requires_reconciliation": true },
        }));
    }
    0
}
