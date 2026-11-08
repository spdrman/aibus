//! Small constructors and readers for graph `Value`s.

use std::sync::Arc;

use aibus_graph::Value;
use indexmap::IndexMap;
use smol_str::SmolStr;

pub fn s(text: &str) -> Value {
    Value::Str(text.into())
}

pub fn n(x: impl TryInto<i64>) -> Value {
    Value::I64(x.try_into().unwrap_or(i64::MAX))
}

pub fn list(items: Vec<Value>) -> Value {
    Value::List(items.into())
}

pub fn map<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Map(Arc::new(
        pairs
            .into_iter()
            .map(|(k, v)| (SmolStr::new(k), v))
            .collect(),
    ))
}

pub fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Map(m) => m.get(key),
        _ => None,
    }
}

pub fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

pub fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::I64(n) => Some(*n),
        _ => None,
    }
}

pub fn as_list(v: &Value) -> &[Value] {
    match v {
        Value::List(l) => l,
        _ => &[],
    }
}

/// `map` with one key replaced or added.
pub fn with(v: &Value, key: &str, value: Value) -> Value {
    let mut m: IndexMap<SmolStr, Value> = match v {
        Value::Map(m) => (**m).clone(),
        _ => IndexMap::new(),
    };
    m.insert(key.into(), value);
    Value::Map(Arc::new(m))
}

/// `list` with one element appended.
pub fn pushed(v: &Value, item: Value) -> Value {
    let mut items = as_list(v).to_vec();
    items.push(item);
    list(items)
}

pub fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(x) => x
            .as_i64()
            .map(Value::I64)
            .unwrap_or_else(|| Value::F64(x.as_f64().unwrap_or(0.0))),
        serde_json::Value::String(t) => Value::Str(t.as_str().into()),
        serde_json::Value::Array(a) => list(a.iter().map(json_to_value).collect()),
        serde_json::Value::Object(o) => Value::Map(Arc::new(
            o.iter()
                .map(|(k, v)| (SmolStr::new(k), json_to_value(v)))
                .collect(),
        )),
    }
}

/// The plain JSON shape of a graph value, for NDJSON output. Bytes and store handles have no
/// plain shape and come out as `null`.
pub fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null | Value::Bytes(_) | Value::Handle(_) => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::I64(n) => (*n).into(),
        Value::F64(x) => (*x).into(),
        Value::Str(t) => t.as_str().into(),
        Value::List(items) => items.iter().map(value_to_json).collect(),
        Value::Map(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect(),
        ),
    }
}
