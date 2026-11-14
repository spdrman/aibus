//! The brain of `catalog-sync`: a pure function from a models.dev snapshot and the current
//! `catalog/*.json` texts to the new texts. It does no I/O and no network, so the tool can only
//! write what a maintainer asked for and then reads in `git diff`. See `docs/PROVIDERS.md`.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};

/// Where each catalog file's rows come from in the models.dev data set. A file not listed here
/// is looked up under its own name.
pub const SOURCES: [(&str, &str); 17] = [
    ("anthropic", "anthropic"),
    ("bedrock", "amazon-bedrock"),
    ("vertex", "google-vertex-anthropic"),
    ("openai", "openai"),
    ("kimi", "moonshotai"),
    ("groq", "groq"),
    ("mistral", "mistral"),
    ("deepseek", "deepseek"),
    ("xai", "xai"),
    ("together", "togetherai"),
    ("fireworks", "fireworks-ai"),
    ("cerebras", "cerebras"),
    ("deepinfra", "deepinfra"),
    ("perplexity", "perplexity"),
    ("openrouter", "openrouter"),
    ("gemini-openai", "google"),
    ("gemini", "google"),
];

const BUCKETS: [&str; 4] = [
    "input_per_mtok",
    "output_per_mtok",
    "cache_read_per_mtok",
    "cache_write_per_mtok",
];

/// What `snapshot` (the models.dev `api.json`) says about the models already in each catalog file
/// (`catalog` maps a file stem to its text). Returns the new text of every file that changed and
/// a note per change. It refuses (`Err`, nothing returned) when a model would lose a price
/// bucket it has today, unless `allow_price_removal`. Rows with a `price_from` are never touched,
/// and a model only the snapshot knows is added only with `add_new`.
pub fn plan(
    snapshot: &Value,
    catalog: &BTreeMap<String, String>,
    add_new: bool,
    allow_price_removal: bool,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let (mut files, mut notes, mut losses) = (BTreeMap::new(), Vec::new(), Vec::new());
    for (name, text) in catalog {
        let source = SOURCES
            .iter()
            .find(|(n, _)| n == name)
            .map_or(name.as_str(), |(_, s)| s);
        let Some(theirs) = snapshot[source]["models"].as_object() else {
            notes.push(format!(
                "{name}: no \"{source}\" provider in the snapshot, left alone"
            ));
            continue;
        };
        let mut file: Value =
            serde_json::from_str(text).map_err(|e| format!("catalog/{name}.json: {e}"))?;
        let rows = file["models"]
            .as_array_mut()
            .ok_or_else(|| format!("catalog/{name}.json: no models"))?;
        for row in rows.iter_mut().filter(|r| r.get("price_from").is_none()) {
            let id = row["id"].as_str().unwrap_or_default().to_owned();
            let Some(m) = theirs.get(&id) else { continue };
            let before = row.clone();
            update_row(row, m, &mut losses, name, &id, allow_price_removal);
            if *row != before {
                notes.push(format!("{name}: {id} changed: {}", describe(&before, row)));
            }
        }
        let known: Vec<String> = rows
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_owned))
            .collect();
        for (id, m) in theirs.iter().filter(|(id, _)| !known.contains(id)) {
            if !add_new {
                notes.push(format!(
                    "{name}: {id} is in the snapshot but not the catalog (--add-new adds it)"
                ));
                continue;
            }
            let mut row = serde_json::json!({
                "id": id, "context_window": 0, "max_output": 0,
                "family": m["family"].as_str().unwrap_or(id), "deprecated": false,
            });
            update_row(&mut row, m, &mut losses, name, id, true);
            notes.push(format!("{name}: {id} added"));
            rows.push(row);
        }
        let new = render(&file);
        if new != *text {
            files.insert(name.clone(), new);
        }
    }
    if !losses.is_empty() && !allow_price_removal {
        return Err(format!(
            "refusing to write: these would lose a price they have today (--allow-price-removal to accept):\n  {}",
            losses.join("\n  ")
        ));
    }
    Ok((files, notes))
}

/// Copies the sizes, the capabilities and the price from the snapshot's model `m` onto `row`.
fn update_row(
    row: &mut Value,
    m: &Value,
    losses: &mut Vec<String>,
    file: &str,
    id: &str,
    allow_loss: bool,
) {
    if let Some(n) = m["limit"]["context"].as_u64().filter(|n| *n > 0) {
        row["context_window"] = n.into();
    }
    if let Some(n) = m["limit"]["output"].as_u64().filter(|n| *n > 0) {
        row["max_output"] = n.into();
    }
    // models.dev's names become aibus's (D-F3): `tool_call` is `capabilities.tools`, an `image`
    // among the input modalities is `capabilities.images`, and `reasoning` is the row's own
    // `reasoning`. Only what the snapshot says is written, and a hand-set capability the
    // snapshot has no word for (`max_tools`, `logprobs`) stays. The keys an earlier version of
    // this tool wrote under models.dev's names are dropped.
    if let Some(b) = m["reasoning"].as_bool() {
        row["reasoning"] = b.into();
    }
    let mut caps = row["capabilities"].as_object().cloned().unwrap_or_default();
    for stale in ["tool_call", "reasoning", "input_modalities"] {
        caps.remove(stale);
    }
    if let Some(b) = m["tool_call"].as_bool() {
        caps.insert("tools".into(), b.into());
    }
    if let Some(a) = m["modalities"]["input"].as_array() {
        caps.insert("images".into(), a.iter().any(|x| x == "image").into());
    }
    if caps.is_empty() {
        row.as_object_mut().map(|o| o.remove("capabilities"));
    } else {
        row["capabilities"] = caps.into();
    }

    let cost = &m["cost"];
    let theirs = |k: &str| {
        cost[k]
            .as_f64()
            .and_then(Number::from_f64)
            .map(Value::Number)
    };
    let old = row["price"].as_object().cloned();
    let (Some(input), Some(output)) = (theirs("input"), theirs("output")) else {
        if old.is_some() {
            losses.push(format!(
                "{file}: {id} has a price and the snapshot has none"
            ));
            if allow_loss {
                row.as_object_mut().map(|o| o.remove("price"));
            }
        }
        return;
    };
    let new = [
        ("input_per_mtok", Some(input)),
        ("output_per_mtok", Some(output)),
        ("cache_read_per_mtok", theirs("cache_read")),
        ("cache_write_per_mtok", theirs("cache_write")),
    ];
    for (k, v) in &new {
        if v.is_none()
            && old
                .as_ref()
                .is_some_and(|o| !o.get(*k).is_none_or(Value::is_null))
        {
            losses.push(format!("{file}: {id} loses {k}"));
        }
    }
    if allow_loss
        || !losses
            .iter()
            .any(|l| l.starts_with(&format!("{file}: {id} ")))
    {
        let price: Map<String, Value> = new
            .into_iter()
            .map(|(k, v)| (k.into(), v.unwrap_or(Value::Null)))
            .collect();
        row["price"] = price.into();
    }
}

fn describe(a: &Value, b: &Value) -> String {
    let (a, b) = (a.as_object().unwrap(), b.as_object().unwrap());
    let mut out = Vec::new();
    for (k, v) in b {
        if k == "price" {
            for p in BUCKETS {
                let (x, y) = (&a.get("price").map_or(Value::Null, |o| o[p].clone()), &v[p]);
                if x != y {
                    out.push(format!("{p} {} -> {}", compact(x), compact(y)));
                }
            }
        } else if a.get(k) != Some(v) {
            out.push(format!(
                "{k} {} -> {}",
                a.get(k).map_or("none".into(), compact),
                compact(v)
            ));
        }
    }
    out.join(", ")
}

/// One JSON value on one line, with `, ` and `: ` separators and sorted keys.
fn compact(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .iter()
                .map(|k| format!("{}: {}", Value::from(k.as_str()), compact(&m[*k])))
                .collect();
            format!("{{{}}}", body.join(", "))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(compact).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

/// The one canonical layout of a catalog file: a row per entry, its price on a second line with
/// all four buckets (`null` for one the provider does not charge separately), `families` last.
/// Hand edits that keep this layout round-trip unchanged, so `catalog-sync` diffs stay small.
pub fn render(file: &Value) -> String {
    const ORDER: [&str; 9] = [
        "id",
        "context_window",
        "max_output",
        "family",
        "deprecated",
        "effort",
        "price_from",
        "reasoning",
        "capabilities",
    ];
    let rows: Vec<String> = file["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let obj = row.as_object().unwrap();
            let mut keys: Vec<&str> = ORDER
                .iter()
                .copied()
                .filter(|k| obj.contains_key(*k))
                .collect();
            let mut rest: Vec<&str> = obj
                .keys()
                .map(String::as_str)
                .filter(|k| !ORDER.contains(k) && *k != "price")
                .collect();
            rest.sort_unstable();
            keys.extend(rest);
            let fields: Vec<String> = keys
                .iter()
                .map(|k| format!("\"{k}\": {}", compact(&obj[*k])))
                .collect();
            let mut line = format!("    {{{}", fields.join(", "));
            if let Some(p) = obj.get("price").and_then(Value::as_object) {
                let buckets: Vec<String> = BUCKETS
                    .iter()
                    .map(|b| format!("\"{b}\": {}", compact(p.get(*b).unwrap_or(&Value::Null))))
                    .collect();
                line.push_str(&format!(",\n     \"price\": {{{}}}", buckets.join(", ")));
            }
            line + "}"
        })
        .collect();
    let families = file
        .get("families")
        .filter(|f| f.as_object().is_some_and(|o| !o.is_empty()))
        .map_or("{}".into(), compact);
    format!(
        "{{\n  \"models\": [\n{}\n  ],\n  \"families\": {families}\n}}\n",
        rows.join(",\n")
    )
}
