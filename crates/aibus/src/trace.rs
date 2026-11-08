//! `--trace <file>`: Chrome trace events for commits, effects, provider calls, tool calls and
//! hooks (08 section 6). The engine opens `tracing` spans with target `aibus_trace`; this layer
//! turns each closed span into one "complete" event. Without `--trace` the layer does not exist,
//! the log layer filters the target out, and the spans are disabled callsites (an atomic load).
//!
//! The file is the JSON object form (`{"traceEvents":[...]}`), which `chrome://tracing` and
//! Perfetto open. Every event is flushed as it is written, so a run that is killed still leaves a
//! file the viewers can load (they accept a missing closing bracket).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// The `tracing` target the engine's spans use.
pub const TARGET: &str = "aibus_trace";

/// The tracks of the viewer, one per kind of span, so that spans of different kinds never have
/// to nest.
const TRACKS: [&str; 5] = ["commit", "effect", "provider_call", "tool_call", "hook"];

struct Out {
    file: BufWriter<File>,
    first: bool,
}

struct Shared {
    out: Mutex<Out>,
    epoch: Instant,
}

impl Shared {
    fn emit(&self, event: &Value) {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let comma = if std::mem::replace(&mut out.first, false) { "" } else { ",\n" };
        let _ = write!(out.file, "{comma}{event}");
        let _ = out.file.flush();
    }
}

/// Closes the JSON when dropped.
pub struct TraceGuard(Arc<Shared>);

impl Drop for TraceGuard {
    fn drop(&mut self) {
        let mut out = self.0.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out.file, "\n]}}");
        let _ = out.file.flush();
    }
}

pub struct TraceLayer(Arc<Shared>);

/// Creates `path` (0600) with the track names in it. The guard goes at the end of `main`.
pub fn create(path: &Path) -> std::io::Result<(TraceLayer, TraceGuard)> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = BufWriter::new(opts.open(path)?);
    let shared = Arc::new(Shared {
        out: Mutex::new(Out { file, first: true }),
        epoch: Instant::now(),
    });
    {
        let mut out = shared.out.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(out.file, "{{\"displayTimeUnit\":\"ms\",\"traceEvents\":[")?;
    }
    shared.emit(&json!({"ph": "M", "name": "process_name", "pid": 1, "args": {"name": "aibus"}}));
    for (i, name) in TRACKS.iter().enumerate() {
        shared.emit(&json!({
            "ph": "M", "name": "thread_name", "pid": 1, "tid": i + 1, "args": {"name": name}
        }));
    }
    Ok((TraceLayer(shared.clone()), TraceGuard(shared)))
}

/// The fields of a span as JSON arguments.
struct Args<'a>(&'a mut Map<String, Value>);

impl Visit for Args<'_> {
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.0.insert(f.name().into(), format!("{v:?}").into());
    }
}

struct Open {
    began: Instant,
    args: Map<String, Value>,
}

impl<S> Layer<S> for TraceLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut args = Map::new();
        attrs.record(&mut Args(&mut args));
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Open {
                began: Instant::now(),
                args,
            });
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(open) = span.extensions_mut().get_mut::<Open>() {
                values.record(&mut Args(&mut open.args));
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(open) = span.extensions_mut().remove::<Open>() else {
            return;
        };
        let name = span.name();
        let tid = TRACKS.iter().position(|t| *t == name).unwrap_or(TRACKS.len()) + 1;
        self.0.emit(&json!({
            "name": name,
            "cat": "aibus",
            "ph": "X",
            "ts": open.began.duration_since(self.0.epoch).as_micros() as u64,
            "dur": open.began.elapsed().as_micros() as u64,
            "pid": 1,
            "tid": tid,
            "args": open.args,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

/// What a span's name and fields look like in the file, for the tests: `(name, args)` of every
/// complete event, in order.
pub fn complete_events(text: &str) -> Result<Vec<(String, Value)>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let events = doc["traceEvents"].as_array().ok_or("no traceEvents")?;
    let mut out = Vec::new();
    for e in events.iter().filter(|e| e["ph"] == "X") {
        for key in ["name", "ts", "dur", "pid", "tid"] {
            if e.get(key).is_none() {
                return Err(format!("event without {key}: {e}"));
            }
        }
        out.push((e["name"].as_str().unwrap_or_default().to_owned(), e["args"].clone()));
    }
    Ok(out)
}

    #[test]
    fn spans_become_complete_events_with_their_fields_and_the_file_is_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        let (layer, guard) = create(&path).unwrap();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::trace_span!(
                target: "aibus_trace", "commit",
                intent = "Tick", changed = tracing::field::Empty, recompute_us = tracing::field::Empty
            );
            span.record("changed", 3_u64);
            span.record("recompute_us", 12_u64);
            drop(span);
            let _tool = tracing::trace_span!(target: "aibus_trace", "tool_call", tool = "ls");
        });
        drop(guard);
        let text = std::fs::read_to_string(&path).unwrap();
        let events = complete_events(&text).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "commit");
        assert_eq!(events[0].1["intent"], "Tick");
        assert_eq!(events[0].1["changed"], 3);
        assert_eq!(events[0].1["recompute_us"], 12);
        assert_eq!(events[1].0, "tool_call");
        // Every event of a track shares one tid.
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert!(doc["traceEvents"].as_array().unwrap().iter().any(|e| e["ph"] == "M"));
    }
}
