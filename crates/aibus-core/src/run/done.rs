//! The `output_schema` check (01 section 1.1): the final message has to be JSON that satisfies
//! the agent's schema. Only the keywords below are read; any other keyword is ignored, and
//! `check_schema` reports the ones that would never be checked so `agent validate` can say so.

use serde_json::Value;

/// The first problem with the schema itself, or `None`: an unknown `type`, a `required` that is
/// not a list of names, a keyword the check does not read.
pub fn check_schema(schema: &Value) -> Option<String> {
    const TYPES: [&str; 7] = [
        "object", "array", "string", "number", "integer", "boolean", "null",
    ];
    const KEYWORDS: [&str; 10] = [
        "type",
        "enum",
        "required",
        "properties",
        "additionalProperties",
        "items",
        "minimum",
        "maximum",
        "description",
        "title",
    ];
    let Value::Object(o) = schema else {
        return Some("a schema is a map".into());
    };
    if let Some(k) = o
        .keys()
        .find(|k| !KEYWORDS.contains(&k.as_str()) && !k.starts_with('$'))
    {
        return Some(format!(
            "keyword `{k}` is not checked (supported: {})",
            KEYWORDS.join(", ")
        ));
    }
    match o.get("type") {
        None => {}
        Some(Value::String(t)) if TYPES.contains(&t.as_str()) => {}
        Some(_) => return Some(format!("`type` is one of {}", TYPES.join(", "))),
    }
    if o.get("required")
        .is_some_and(|r| !r.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
    {
        return Some("`required` is a list of names".into());
    }
    if o.get("enum").is_some_and(|e| !e.is_array()) {
        return Some("`enum` is a list".into());
    }
    for sub in o
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|p| p.values())
    {
        if let Some(p) = check_schema(sub) {
            return Some(p);
        }
    }
    o.get("items").and_then(check_schema)
}

/// Checks the final message against `schema`. The text may sit in a ```json fence. `Err` is the
/// reason that goes back to the model.
pub fn check_output(schema: &Value, text: &str) -> Result<(), String> {
    let t = text.trim();
    let body = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .and_then(|r| r.trim().strip_suffix("```"))
        .unwrap_or(t);
    let value: Value = serde_json::from_str(body.trim())
        .map_err(|e| format!("the final message is not valid JSON: {e}"))?;
    walk(schema, &value, "$")
}

fn walk(schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    let Some(o) = schema.as_object() else {
        return Ok(());
    };
    if let Some(Value::String(t)) = o.get("type") {
        let ok = match t.as_str() {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "number" => v.is_number(),
            "integer" => v.is_i64() || v.is_u64(),
            "boolean" => v.is_boolean(),
            _ => v.is_null(),
        };
        if !ok {
            return Err(format!("{at}: expected {t}"));
        }
    }
    if let Some(Value::Array(e)) = o.get("enum") {
        if !e.contains(v) {
            return Err(format!("{at}: not one of the allowed values"));
        }
    }
    if let (Some(n), Some(m)) = (v.as_f64(), o.get("minimum").and_then(Value::as_f64)) {
        if n < m {
            return Err(format!("{at}: below the minimum {m}"));
        }
    }
    if let (Some(n), Some(m)) = (v.as_f64(), o.get("maximum").and_then(Value::as_f64)) {
        if n > m {
            return Err(format!("{at}: above the maximum {m}"));
        }
    }
    if let Some(obj) = v.as_object() {
        for name in o
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !obj.contains_key(name) {
                return Err(format!("{at}: missing required `{name}`"));
            }
        }
        let props = o.get("properties").and_then(Value::as_object);
        for (k, val) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => walk(sub, val, &format!("{at}.{k}"))?,
                None if o.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{at}: unexpected property `{k}`"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(list)) = (o.get("items"), v.as_array()) {
        for (i, val) in list.iter().enumerate() {
            walk(items, val, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}
