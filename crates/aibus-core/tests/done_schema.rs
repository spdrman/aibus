//! The `output_schema` subset (#389): what `check_output` accepts and rejects.

use aibus_core::run::done::{check_output, check_schema};
use serde_json::json;

#[test]
fn a_fenced_or_bare_object_that_fits_passes() {
    let schema = json!({"type": "object", "required": ["n"], "properties": {"n": {"type": "integer", "minimum": 1}}, "additionalProperties": false});
    assert_eq!(check_output(&schema, r#"{"n": 3}"#), Ok(()));
    assert_eq!(check_output(&schema, "```json\n{\"n\": 3}\n```"), Ok(()));
}

#[test]
fn each_kind_of_mismatch_says_where() {
    let schema = json!({"type": "object", "required": ["n"], "properties": {
        "n": {"type": "integer", "minimum": 1, "maximum": 5},
        "tag": {"enum": ["a", "b"]},
        "xs": {"type": "array", "items": {"type": "string"}},
    }, "additionalProperties": false});
    for (text, want) in [
        ("nope", "not valid JSON"),
        ("[]", "$: expected object"),
        ("{}", "missing required `n`"),
        (r#"{"n": 1.5}"#, "$.n: expected integer"),
        (r#"{"n": 0}"#, "below the minimum"),
        (r#"{"n": 9}"#, "above the maximum"),
        (r#"{"n": 2, "tag": "z"}"#, "$.tag: not one of"),
        (r#"{"n": 2, "xs": ["a", 3]}"#, "$.xs[1]: expected string"),
        (r#"{"n": 2, "extra": 1}"#, "unexpected property `extra`"),
    ] {
        let e = check_output(&schema, text).unwrap_err();
        assert!(e.contains(want), "{text}: {e}");
    }
}

#[test]
fn check_schema_flags_what_would_never_be_checked() {
    assert_eq!(
        check_schema(&json!({"type": "object", "properties": {"a": {"type": "string"}}})),
        None
    );
    assert!(check_schema(&json!({"type": "banana"})).is_some());
    assert!(check_schema(&json!({"oneOf": []}))
        .unwrap()
        .contains("oneOf"));
    assert!(check_schema(&json!({"required": "a"})).is_some());
    assert!(check_schema(&json!({"properties": {"a": {"type": "x"}}})).is_some());
}
