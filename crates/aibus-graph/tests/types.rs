//! Wire and equality rules of the value types.

use aibus_graph::{Bytes, StoreHandle, Value};

#[test]
fn bytes_are_base64_on_the_wire() {
    let v = Value::Bytes(Bytes(vec![0, 1, 2, 250, 255].into()));
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, r#"{"Bytes":"AAEC+v8="}"#);
    assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), v);
    assert!(serde_json::from_str::<Value>(r#"{"Bytes":"not base64!"}"#).is_err());
}

#[test]
fn handle_equality_is_store_id_version() {
    let h = |store: &str, id, version| {
        Value::Handle(StoreHandle {
            store: store.into(),
            id,
            version,
        })
    };
    assert_eq!(h("transcript", 1, 2), h("transcript", 1, 2));
    assert_ne!(h("transcript", 1, 2), h("transcript", 1, 3));
    assert_ne!(h("transcript", 1, 2), h("transcript", 2, 2));
    assert_ne!(h("transcript", 1, 2), h("log", 1, 2));
}

#[test]
fn every_value_shape_round_trips() {
    let map: indexmap::IndexMap<_, _> = [("a".into(), Value::I64(1)), ("b".into(), Value::Null)]
        .into_iter()
        .collect();
    let all = Value::List(
        vec![
            Value::Null,
            Value::Bool(true),
            Value::I64(-7),
            Value::F64(1.5),
            Value::Str("hi".into()),
            Value::Bytes(Bytes(vec![9].into())),
            Value::Map(map.into()),
            Value::Handle(StoreHandle {
                store: "s".into(),
                id: 3,
                version: 4,
            }),
        ]
        .into(),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&serde_json::to_string(&all).unwrap()).unwrap(),
        all
    );
}
