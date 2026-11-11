//! A small JSON Schema checker for the files in `docs/schemas/cli/` (draft-07) and for the tool
//! schemas the MCP server hands out (2020-12). It knows the keywords those files use (`type`,
//! `enum`, `const`, `required`, `properties`, `items`, `minItems`, `minimum`, `maximum`,
//! `pattern`, `anyOf`, `allOf`, `if`/`then`/`else`, local `$ref`) and fails loudly on a keyword
//! it does not know, so a schema cannot quietly say more than the checker enforces. `$id` was
//! added to the known list for `workflow/v1.json`; it is an identifier and checks nothing.
//!
//! Added for the MCP conformance harness (#96), each because the schemas an MCP server sends
//! use it: `$defs` (the 2020-12 name for `definitions`, where a generator puts shared types, and
//! `$ref` already resolves into it), `oneOf` (exactly one branch passes), `minLength`,
//! `maxLength`, `maxItems` and `uniqueItems` (checked), `additionalProperties` as a schema
//! (checked on the keys `properties` does not name), and the annotations `format`, `default`,
//! `examples` and `$comment`, which check nothing. `lint_2020_12` walks a whole schema, not
//! only the branches a value happens to reach, and says what is wrong with the schema itself.

use std::path::Path;

use serde_json::Value;

const KNOWN: [&str; 32] = [
    "$schema", "$id", "title", "description", "definitions", "type", "enum", "const", "required",
    "properties", "items", "minItems", "minimum", "maximum", "pattern", "anyOf", "allOf", "if",
    "then", "else", "$ref", "additionalProperties", "$defs", "oneOf", "minLength", "maxLength",
    "maxItems", "uniqueItems", "format", "default", "examples", "$comment",
];

pub const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

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
    if let Some(text) = v.as_str() {
        let len = text.chars().count() as u64;
        if obj.get("minLength").and_then(Value::as_u64).is_some_and(|m| len < m) {
            errors.push(format!("{at}: shorter than minLength"));
        }
        if obj.get("maxLength").and_then(Value::as_u64).is_some_and(|m| len > m) {
            errors.push(format!("{at}: longer than maxLength"));
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
        let known = obj.get("properties").and_then(Value::as_object);
        match obj.get("additionalProperties") {
            Some(Value::Bool(false)) => {
                for k in map.keys() {
                    if !known.is_some_and(|p| p.contains_key(k)) {
                        errors.push(format!("{at}: unexpected {k}"));
                    }
                }
            }
            Some(sub @ Value::Object(_)) => {
                for (k, child) in map {
                    if !known.is_some_and(|p| p.contains_key(k)) {
                        walk(root, sub, child, &format!("{at}.{k}"), errors);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(items) = v.as_array() {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                errors.push(format!("{at}: fewer than {min} items"));
            }
        }
        if obj.get("maxItems").and_then(Value::as_u64).is_some_and(|m| items.len() as u64 > m) {
            errors.push(format!("{at}: more than maxItems"));
        }
        if obj.get("uniqueItems") == Some(&Value::Bool(true))
            && items.iter().enumerate().any(|(i, a)| items[..i].contains(a))
        {
            errors.push(format!("{at}: items are not unique"));
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
    if let Some(all) = obj.get("oneOf").and_then(Value::as_array) {
        let n = all.iter().filter(|b| passes(b)).count();
        if n != 1 {
            errors.push(format!("{at}: matches {n} branches of oneOf, want exactly one"));
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

/// What is wrong with the schema itself (not with a value): a keyword the checker does not know,
/// a `$schema` that is not 2020-12, a `type` that is not a JSON type, a `required` that is not a
/// list of strings or names a property the schema does not declare, a `pattern` that does not
/// compile, a `$ref` that does not resolve. Every subschema is visited.
pub fn lint_2020_12(root: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(s) = root.get("$schema") {
        if s != DRAFT_2020_12 {
            problems.push(format!("$schema is {s}, want {DRAFT_2020_12}"));
        }
    }
    lint(root, root, "$", &mut problems);
    problems
}

fn lint(root: &Value, schema: &Value, at: &str, problems: &mut Vec<String>) {
    let Some(obj) = schema.as_object() else {
        if !schema.is_boolean() {
            problems.push(format!("{at}: a schema is an object or a boolean, got {schema}"));
        }
        return;
    };
    for key in obj.keys() {
        if !KNOWN.contains(&key.as_str()) {
            problems.push(format!("{at}: unknown keyword {key}"));
        }
    }
    let types = ["object", "array", "string", "boolean", "null", "number", "integer"];
    match obj.get("type") {
        None => {}
        Some(Value::String(t)) if types.contains(&t.as_str()) => {}
        Some(Value::Array(all))
            if all.iter().all(|t| t.as_str().is_some_and(|t| types.contains(&t))) => {}
        Some(other) => problems.push(format!("{at}: bad type {other}")),
    }
    if let Some(req) = obj.get("required") {
        match req.as_array().map(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>()) {
            Some(Some(names)) => {
                // A `$ref` or a branch may declare the property elsewhere.
                let declared = obj.get("properties").and_then(Value::as_object);
                let elsewhere = ["$ref", "allOf", "anyOf", "oneOf", "if"]
                    .iter()
                    .any(|k| obj.contains_key(*k));
                for n in names {
                    if !elsewhere && !declared.is_some_and(|p| p.contains_key(n)) {
                        problems.push(format!("{at}: required {n} is not a declared property"));
                    }
                }
            }
            _ => problems.push(format!("{at}: required is not a list of strings")),
        }
    }
    if let Some(p) = obj.get("pattern") {
        if p.as_str().map(regex::Regex::new).is_none_or(|r| r.is_err()) {
            problems.push(format!("{at}: pattern {p} does not compile"));
        }
    }
    if let Some(r) = obj.get("$ref") {
        let target = r.as_str().and_then(|r| r.strip_prefix("#/")).and_then(|path| {
            path.split('/').try_fold(root, |s, k| s.get(k))
        });
        if target.is_none() {
            problems.push(format!("{at}: $ref {r} does not resolve inside the schema"));
        }
    }
    if let Some(Value::Object(props)) = obj.get("properties") {
        for (k, sub) in props {
            lint(root, sub, &format!("{at}.{k}"), problems);
        }
    }
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(defs)) = obj.get(key) {
            for (k, sub) in defs {
                lint(root, sub, &format!("{at}/{key}/{k}"), problems);
            }
        }
    }
    for key in ["items", "additionalProperties", "if", "then", "else"] {
        if let Some(sub) = obj.get(key) {
            lint(root, sub, &format!("{at}/{key}"), problems);
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        for (i, sub) in obj.get(key).and_then(Value::as_array).into_iter().flatten().enumerate() {
            lint(root, sub, &format!("{at}/{key}/{i}"), problems);
        }
    }
}
