//! A small JSON Schema (draft-07) checker for the files in `docs/schemas/cli/`. It knows the
//! keywords those files use (`type`, `enum`, `const`, `required`, `properties`, `items`,
//! `minItems`, `minimum`, `maximum`, `pattern`, `anyOf`, `allOf`, `if`/`then`/`else`, local
//! `$ref`) and fails loudly on a keyword it does not know, so a schema cannot quietly say more
//! than the checker enforces. `$id` was added to the known list for `workflow/v1.json`; it is
//! an identifier and checks nothing.

use std::path::Path;

use serde_json::Value;

const KNOWN: [&str; 22] = [
    "$schema", "$id", "title", "description", "definitions", "type", "enum", "const", "required",
    "properties", "items", "minItems", "minimum", "maximum", "pattern", "anyOf", "allOf", "if",
    "then", "else", "$ref", "additionalProperties",
];

pub fn load(name: &str) -> Value {
    load_from("cli", name)
}

/// `docs/schemas/<dir>/<name>.json`.
pub fn load_from(dir: &str, name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schemas")
        .join(dir)
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every problem `value` has against the schema `name`; empty when it validates.
pub fn check(name: &str, value: &Value) -> Vec<String> {
    check_against(&load(name), value)
}

pub fn check_against(root: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    walk(root, root, value, "$", &mut errors);
    errors
}

pub fn assert_valid(name: &str, value: &Value) {
    let errors = check(name, value);
    assert!(errors.is_empty(), "{name}: {errors:?}\n{value}");
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        other => panic!("unknown type {other}"),
    }
}

fn walk(root: &Value, schema: &Value, v: &Value, at: &str, errors: &mut Vec<String>) {
    let Some(obj) = schema.as_object() else {
        return;
    };
    for key in obj.keys() {
        assert!(KNOWN.contains(&key.as_str()), "the checker does not know the keyword {key}");
    }
    if let Some(r) = obj.get("$ref") {
        let path = r.as_str().unwrap().strip_prefix("#/").expect("only local refs");
        let target = path.split('/').fold(root, |s, k| &s[k]);
        return walk(root, target, v, at, errors);
    }
    if let Some(t) = obj.get("type") {
        let ok = match t {
            Value::String(s) => type_ok(s, v),
            Value::Array(all) => all.iter().any(|s| type_ok(s.as_str().unwrap(), v)),
            _ => panic!("bad type"),
        };
        if !ok {
            errors.push(format!("{at}: want type {t}, got {v}"));
            return;
        }
    }
    if let Some(e) = obj.get("enum") {
        if !e.as_array().unwrap().contains(v) {
            errors.push(format!("{at}: {v} is not one of {e}"));
        }
    }
    if let Some(c) = obj.get("const") {
        if c != v {
            errors.push(format!("{at}: want {c}, got {v}"));
        }
    }
    if let (Some(p), Some(s)) = (obj.get("pattern"), v.as_str()) {
        if !regex::Regex::new(p.as_str().unwrap()).unwrap().is_match(s) {
            errors.push(format!("{at}: {s:?} does not match {p}"));
        }
    }
    if let Some(n) = v.as_f64() {
        if obj.get("minimum").and_then(Value::as_f64).is_some_and(|m| n < m) {
            errors.push(format!("{at}: {n} is below the minimum"));
        }
        if obj.get("maximum").and_then(Value::as_f64).is_some_and(|m| n > m) {
            errors.push(format!("{at}: {n} is above the maximum"));
        }
    }
    if let Some(map) = v.as_object() {
        for r in obj.get("required").and_then(Value::as_array).into_iter().flatten() {
            if !map.contains_key(r.as_str().unwrap()) {
                errors.push(format!("{at}: missing {r}"));
            }
        }
        for (k, sub) in obj.get("properties").and_then(Value::as_object).into_iter().flatten() {
            if let Some(child) = map.get(k) {
                walk(root, sub, child, &format!("{at}.{k}"), errors);
            }
        }
        if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
            let known = obj.get("properties").and_then(Value::as_object);
            for k in map.keys() {
                if !known.is_some_and(|p| p.contains_key(k)) {
                    errors.push(format!("{at}: unexpected {k}"));
                }
            }
        }
    }
    if let Some(items) = v.as_array() {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                errors.push(format!("{at}: fewer than {min} items"));
            }
        }
        if let Some(sub) = obj.get("items") {
            for (i, item) in items.iter().enumerate() {
                walk(root, sub, item, &format!("{at}[{i}]"), errors);
            }
        }
    }
    let passes = |sub: &Value| {
        let mut e = Vec::new();
        walk(root, sub, v, at, &mut e);
        e.is_empty()
    };
    if let Some(any) = obj.get("anyOf").and_then(Value::as_array) {
        if !any.iter().any(passes) {
            errors.push(format!("{at}: matches none of anyOf"));
        }
    }
    for sub in obj.get("allOf").and_then(Value::as_array).into_iter().flatten() {
        walk(root, sub, v, at, errors);
    }
    if let Some(cond) = obj.get("if") {
        let branch = if passes(cond) { "then" } else { "else" };
        if let Some(sub) = obj.get(branch) {
            walk(root, sub, v, at, errors);
        }
    }
}
