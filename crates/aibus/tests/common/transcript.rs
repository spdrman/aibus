//! Golden transcripts for the MCP server (#96): the request and response pairs of a scenario,
//! normalised so that two runs on two machines write the same bytes, compared against
//! `fixtures/mcp-server/<scenario>.jsonl`.
//!
//! What the normaliser takes out, and why:
//!
//! * JSON-RPC ids: the client numbers requests from 1, but the pairing is the file's line, so
//!   the number says nothing.
//! * `_meta` on a request: the client adds it to every `2026-07-28` request and not to the
//!   other protocol, and a golden file is the same for both.
//! * `resultType`, `ttlMs` and `cacheScope` on a result: protocol framing the SDK fills in by
//!   protocol version. The tests that care about them look at them directly.
//! * ULIDs, RFC 3339 timestamps (`<ulid>`, `<time>`) and the temp home (`<home>`), wherever they
//!   appear inside a string. The home is replaced in its raw and in its canonical spelling,
//!   since the temp directory is a symlink on macOS.
//!
//! Object keys are sorted when a line is written, so the files diff well, and so are the keys
//! of JSON held in a string (a tool result mirrors its `structuredContent` as a text block).
//!
//! `AIBUS_UPDATE_TRANSCRIPTS=1` rewrites the file instead of comparing. A missing file is a
//! failure that prints what would have been written, so a new scenario starts with the run that
//! shows its transcript.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Map, Value};

pub struct Transcript {
    /// The raw and the canonical spelling of the temp home.
    homes: Vec<String>,
    pairs: Vec<Value>,
}

impl Transcript {
    pub fn new(home: &Path) -> Self {
        let mut homes = vec![home.display().to_string()];
        if let Ok(real) = home.canonicalize() {
            homes.push(real.display().to_string());
        }
        // Longest first, so `/private/var/x` is not cut at `/var/x`.
        homes.sort_by_key(|h| std::cmp::Reverse(h.len()));
        homes.dedup();
        Self {
            homes,
            pairs: Vec::new(),
        }
    }

    /// Records one exchange: the request as it was sent and the whole response message.
    pub fn record(&mut self, request: &Value, response: &Value) {
        let mut req = Map::new();
        req.insert("method".into(), request["method"].clone());
        if let Some(Value::Object(params)) = request.get("params") {
            let mut params = params.clone();
            params.remove("_meta");
            if !params.is_empty() {
                req.insert("params".into(), Value::Object(params));
            }
        }
        let mut res = Map::new();
        for key in ["result", "error"] {
            if let Some(v) = response.get(key) {
                let mut v = v.clone();
                if let Some(o) = v.as_object_mut() {
                    for framing in ["resultType", "ttlMs", "cacheScope"] {
                        o.remove(framing);
                    }
                }
                res.insert(key.into(), v);
            }
        }
        let mut pair = json!({ "request": req, "response": res });
        self.normalise(&mut pair);
        self.pairs.push(pair);
    }

    fn normalise(&self, v: &mut Value) {
        let ulid = Regex::new(r"\b[0-9A-HJKMNP-TV-Z]{26}\b").unwrap();
        let time =
            Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})").unwrap();
        match v {
            Value::String(s) => {
                // A text block that holds JSON (`structuredContent` mirrored as text) is written
                // with sorted keys, whatever order the server built it in.
                if s.starts_with(['{', '[']) {
                    if let Ok(inner) = serde_json::from_str::<Value>(s) {
                        let mut inner = sorted(&inner);
                        self.normalise(&mut inner);
                        *s = inner.to_string();
                        return;
                    }
                }
                let mut out = s.clone();
                for h in &self.homes {
                    out = out.replace(h.as_str(), "<home>");
                }
                out = time.replace_all(&out, "<time>").into_owned();
                *s = ulid.replace_all(&out, "<ulid>").into_owned();
            }
            Value::Array(items) => items.iter_mut().for_each(|i| self.normalise(i)),
            Value::Object(map) => map.values_mut().for_each(|i| self.normalise(i)),
            _ => {}
        }
    }

    /// One line per exchange, keys in sorted order.
    pub fn lines(&self) -> Vec<String> {
        self.pairs.iter().map(|p| sorted(p).to_string()).collect()
    }

    /// Compares with `fixtures/mcp-server/<scenario>.jsonl`; prints both sides on a difference.
    pub fn assert_matches(&self, scenario: &str) {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/mcp-server")
            .join(format!("{scenario}.jsonl"));
        let got = self.lines().join("\n") + "\n";
        if std::env::var("AIBUS_UPDATE_TRANSCRIPTS").is_ok_and(|v| v == "1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &got).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{}: {e}\nthe transcript this run produced:\n{got}",
                path.display()
            )
        });
        // Compared as values, so a golden file written by hand may order its keys as it likes.
        let parse = |t: &str| -> Vec<Value> {
            t.lines()
                .map(|l| serde_json::from_str(l).expect("a transcript line is JSON"))
                .collect()
        };
        assert!(
            parse(&want) == parse(&got),
            "{} differs (AIBUS_UPDATE_TRANSCRIPTS=1 rewrites it)\n--- golden\n{want}--- this run\n{got}",
            path.display()
        );
    }
}

fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<_> = m.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), sorted(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}
