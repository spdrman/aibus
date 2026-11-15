//! The WASM guest host (#333): a third party's stateful parser, run as a module that has no
//! ambient authority at all.
//!
//! **What a guest is.** A WebAssembly module (built for `wasm32-unknown-unknown`, not WASI) named in
//! the manifest by `normalizer.module`, pinned by the package's content hash and signed with it like
//! every other file. The host links exactly three functions, module `aibus`, and refuses at load a
//! module that imports anything else (so no clock, no file, no socket, no environment, no random,
//! no thread, and no WASI at all):
//!
//! | import | signature | what it does |
//! | --- | --- | --- |
//! | `read_line` | `(ptr i32, cap i32) -> i32` | copies the current stdout line to `ptr`; its length, or `-1` when `cap` is too small |
//! | `emit` | `(ptr i32, len i32) -> i32` | hands the host one normalised event as JSON; `0` taken, `1` refused |
//! | `next_seq` | `() -> i64` | the host's next sequence number: monotonic, per attempt, from 1 |
//!
//! and the guest exports `memory`, `on_line() -> i32` (called once per stdout line) and
//! `on_finish() -> i32` (called once, after the last line, so it can flush what it remembers). A
//! return value other than `0` is a failure.
//!
//! **What the host keeps for itself.** The guest never sees stderr, hook lines, the environment or a
//! clock, and never chooses an event id, an attempt id, a timestamp, a parser name, the raw-line
//! hash or the evidence citation: the host stamps those. The guest proposes a kind (from a short
//! list: never `attempt_*`), a native type, a tool use id, a little data, and an origin and evidence
//! level that the host clamps: an event made from a line is `cli_stream` at best `observed`, an event
//! the guest says is `derived` is at best `inferred`, and an event made at the end of the stream has
//! no line to cite so it is `derived` and at best `inferred`. Usage numbers are held to the quality
//! the manifest declares.
//!
//! **Limits, per call unless said.** Fuel (instructions), a wall deadline (epoch interruption, ticked
//! by one thread per loaded module), memory, the size of one event, events per call, and the total
//! bytes a guest may emit in an attempt. A guest that breaks one is stopped, a `gap_detected` says why
//! (`extension_failed:<cause>`), and it is never called again: the attempt itself carries on and the
//! host still writes its terminal event. See `docs/SECURITY.md` for what this does not cover.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use aibus_supervisor::{Channel, Ending, Engine};
use aibus_trace::{
    AttemptRef, Batch, DataQuality, EventKind, Metric, Origin, TraceEvent, Visibility,
};
use serde_json::{json, Value};
use wasmtime::{
    Caller, Config, Extern, ExternType, FuncType, Linker, Store, StoreLimits, StoreLimitsBuilder,
    Trap, TypedFunc, ValType,
};

use crate::declarative::Declarative;
use crate::manifest::UsageQuality;

/// Parser name on every event the guest's engine stamps.
pub const PARSER: &str = "wasm-module/1";
/// Events one call may hand over; the rest are refused.
pub const MAX_EVENTS_PER_CALL: usize = 64;
/// Most memory a module may declare as its minimum, and the default cap at run time.
pub const MAX_MEMORY_BYTES: usize = 16 << 20;
/// The epoch tick: a deadline is counted in these.
const TICK: Duration = Duration::from_millis(10);
const PAGE: u64 = 65_536;

/// The host's limits on one guest. Fuel and the deadline are per call; the output cap is per
/// attempt. The defaults are the host's ceiling: a manifest cannot ask for more.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Instructions one call may run.
    pub fuel: u64,
    /// Linear memory the guest may hold.
    pub memory_bytes: usize,
    /// Wall time one call may take.
    pub deadline: Duration,
    /// The longest JSON one event may be.
    pub max_event_bytes: usize,
    /// All the event bytes a guest may emit in one attempt.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 10_000_000,
            memory_bytes: MAX_MEMORY_BYTES,
            deadline: Duration::from_millis(500),
            max_event_bytes: 16 << 10,
            max_output_bytes: 4 << 20,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WasmError {
    #[error("the module does not compile: {0}")]
    Compile(String),
    #[error("{0}")]
    Import(String),
    #[error("{0}")]
    Export(String),
    #[error("{0}")]
    Memory(String),
    #[error("the module could not start: {0}")]
    Instantiate(String),
}

/// Why a guest was stopped: the word after `extension_failed:` in the gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    Fuel,
    Deadline,
    Trap,
    BadPointer,
    BadReturn,
    OutputLimit,
}

impl Cause {
    fn word(self) -> &'static str {
        match self {
            Cause::Fuel => "fuel",
            Cause::Deadline => "deadline",
            Cause::Trap => "trap",
            Cause::BadPointer => "bad_pointer",
            Cause::BadReturn => "bad_return",
            Cause::OutputLimit => "output_limit",
        }
    }
}

/// A module that passed the load checks: compiled, imports only the host API, exports the entry
/// points, declares memory within the cap. Cheap to clone.
#[derive(Clone)]
pub struct Module {
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    /// Dropping the last clone ends the thread that ticks the engine's epoch.
    _ticker: Arc<mpsc::Sender<()>>,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Module")
    }
}

/// Modules this process has already compiled, by content hash, so that reading a package and
/// every attempt after it share one compilation (the compile is the slow step). A few entries,
/// oldest out first.
static COMPILED: Mutex<Vec<(String, Module)>> = Mutex::new(Vec::new());

impl Module {
    /// Compiles `bytes` and refuses a module the host will not run. No instance exists yet, so no
    /// guest code has run.
    pub fn load(bytes: &[u8]) -> Result<Self, WasmError> {
        let key = aibus_trace::sha256_hex(bytes);
        if let Some((_, m)) = COMPILED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(k, _)| *k == key)
        {
            return Ok(m.clone());
        }
        let m = Self::compile(bytes)?;
        let mut kept = COMPILED.lock().unwrap_or_else(|e| e.into_inner());
        if kept.len() >= 8 {
            kept.remove(0);
        }
        kept.push((key, m.clone()));
        Ok(m)
    }

    fn compile(bytes: &[u8]) -> Result<Self, WasmError> {
        let mut c = Config::new();
        c.consume_fuel(true)
            .epoch_interruption(true)
            // Same input, same output: no relaxed SIMD (its results vary by CPU), canonical NaNs.
            .wasm_relaxed_simd(false)
            .cranelift_nan_canonicalization(true)
            .wasm_multi_memory(false)
            .wasm_memory64(false)
            .memory_reservation(MAX_MEMORY_BYTES as u64)
            .memory_guard_size(PAGE)
            .max_wasm_stack(256 << 10);
        let engine = wasmtime::Engine::new(&c).map_err(|e| WasmError::Compile(e.to_string()))?;
        let module = wasmtime::Module::new(&engine, bytes)
            .map_err(|e| WasmError::Compile(format!("{e:#}")))?;

        // The three functions a guest may import: name, parameters, results.
        const HOST_API: [(&str, &[ValType], &[ValType]); 3] = [
            ("read_line", &[ValType::I32, ValType::I32], &[ValType::I32]),
            ("emit", &[ValType::I32, ValType::I32], &[ValType::I32]),
            ("next_seq", &[], &[ValType::I64]),
        ];
        for i in module.imports() {
            let known = HOST_API
                .iter()
                .find(|(n, _, _)| i.module() == "aibus" && i.name() == *n);
            let ok = match (known, i.ty()) {
                (Some((_, params, results)), ExternType::Func(ft)) => {
                    let want =
                        FuncType::new(&engine, params.iter().cloned(), results.iter().cloned());
                    FuncType::eq(&ft, &want)
                }
                _ => false,
            };
            if !ok {
                return Err(WasmError::Import(format!(
                    "import {}.{} is not in the host API (aibus.read_line, aibus.emit, aibus.next_seq)",
                    i.module(),
                    i.name()
                )));
            }
        }

        let entry = |name: &str, ft: FuncType| -> Result<(), WasmError> {
            match module.get_export(name) {
                Some(ExternType::Func(f)) if FuncType::eq(&f, &ft) => Ok(()),
                _ => Err(WasmError::Export(format!(
                    "the module must export `{name}` as () -> i32"
                ))),
            }
        };
        entry("on_line", FuncType::new(&engine, [], [ValType::I32]))?;
        entry("on_finish", FuncType::new(&engine, [], [ValType::I32]))?;
        match module.get_export("memory") {
            Some(ExternType::Memory(m)) => {
                if m.is_64() || m.is_shared() {
                    return Err(WasmError::Export(
                        "`memory` must be a 32-bit, unshared memory".into(),
                    ));
                }
                if m.minimum() * PAGE > MAX_MEMORY_BYTES as u64 {
                    return Err(WasmError::Memory(format!(
                        "the module needs {} bytes of memory at the start, the most hosted is {MAX_MEMORY_BYTES}",
                        m.minimum() * PAGE
                    )));
                }
            }
            _ => return Err(WasmError::Export("the module must export `memory`".into())),
        }
        if module
            .exports()
            .filter(|e| matches!(e.ty(), ExternType::Memory(_)))
            .count()
            > 1
        {
            return Err(WasmError::Export("one memory only".into()));
        }

        let (tx, rx) = mpsc::channel::<()>();
        let ticking = engine.clone();
        std::thread::Builder::new()
            .name("aibus-wasm-epoch".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(TICK) {
                    ticking.increment_epoch();
                }
            })
            .map_err(|e| WasmError::Compile(format!("no epoch thread: {e}")))?;
        Ok(Self {
            engine,
            module,
            _ticker: Arc::new(tx),
        })
    }
}

/// What the host functions share with the engine for one call.
struct Host {
    line: Vec<u8>,
    seq: u64,
    /// Events the guest handed over in this call, raw.
    emitted: Vec<Vec<u8>>,
    /// Why events were refused at `emit` in this call.
    refused: Vec<&'static str>,
    refused_total: u32,
    output_bytes: usize,
    max_event_bytes: usize,
    max_output_bytes: usize,
    fault: Option<Cause>,
    limits: StoreLimits,
}

impl Host {
    fn fault(&mut self, cause: Cause) -> wasmtime::Error {
        self.fault = Some(cause);
        wasmtime::Error::msg(format!("guest stopped: {}", cause.word()))
    }
}

fn memory(c: &mut Caller<'_, Host>) -> wasmtime::Result<wasmtime::Memory> {
    match c.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(c.data_mut().fault(Cause::Trap)),
    }
}

fn link(engine: &wasmtime::Engine) -> Result<Linker<Host>, WasmError> {
    let mut l = Linker::new(engine);
    let err = |e: wasmtime::Error| WasmError::Instantiate(e.to_string());
    l.func_wrap(
        "aibus",
        "read_line",
        |mut c: Caller<'_, Host>, ptr: i32, cap: i32| -> wasmtime::Result<i32> {
            let line = std::mem::take(&mut c.data_mut().line);
            let r = if (cap as u32 as usize) < line.len() {
                Ok(-1)
            } else {
                let m = memory(&mut c)?;
                match m.write(&mut c, ptr as u32 as usize, &line) {
                    Ok(()) => Ok(line.len() as i32),
                    Err(_) => Err(c.data_mut().fault(Cause::BadPointer)),
                }
            };
            c.data_mut().line = line;
            r
        },
    )
    .map_err(err)?;
    l.func_wrap(
        "aibus",
        "emit",
        |mut c: Caller<'_, Host>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            let m = memory(&mut c)?;
            let (start, len) = (ptr as u32 as usize, len as u32 as usize);
            let end = start.checked_add(len).filter(|e| *e <= m.data_size(&c));
            let Some(end) = end else {
                return Err(c.data_mut().fault(Cause::BadPointer));
            };
            let h = c.data();
            let why = if len > h.max_event_bytes {
                Some("event_too_big")
            } else if h.emitted.len() >= MAX_EVENTS_PER_CALL {
                Some("too_many_events")
            } else {
                None
            };
            if let Some(why) = why {
                let h = c.data_mut();
                h.refused.push(why);
                h.refused_total += 1;
                // Refused events (too big, too many) an attempt tolerates before the guest is stopped.
                if h.refused_total > 16 {
                    return Err(h.fault(Cause::OutputLimit));
                }
                return Ok(1);
            }
            if h.output_bytes + len > h.max_output_bytes {
                return Err(c.data_mut().fault(Cause::OutputLimit));
            }
            let bytes = m.data(&c)[start..end].to_vec();
            let h = c.data_mut();
            h.output_bytes += len;
            h.emitted.push(bytes);
            Ok(0)
        },
    )
    .map_err(err)?;
    l.func_wrap("aibus", "next_seq", |mut c: Caller<'_, Host>| -> i64 {
        let h = c.data_mut();
        h.seq += 1;
        h.seq as i64
    })
    .map_err(err)?;
    Ok(l)
}

/// The instantiated guest: its store and the two entry points.
struct Guest {
    store: Store<Host>,
    on_line: TypedFunc<(), i32>,
    on_finish: TypedFunc<(), i32>,
    fuel: u64,
    ticks: u64,
}

/// A guest's events as the guest wrote them. Only these fields are read: anything else a guest adds
/// (an `event_id`, an `attempt_id`, a timestamp) is ignored.
struct Proposal {
    kind: EventKind,
    native_type: Option<String>,
    tool_use_id: Option<String>,
    data: Value,
    origin: Origin,
    visibility: Option<Visibility>,
}

fn proposal(raw: &[u8]) -> Result<Proposal, &'static str> {
    let v: Value = serde_json::from_slice(raw).map_err(|_| "not_json")?;
    let o = v.as_object().ok_or("not_an_object")?;
    let kind: EventKind = o
        .get("kind")
        .and_then(|k| serde_json::from_value(k.clone()).ok())
        .ok_or("kind_not_allowed")?;
    if !matches!(
        kind,
        EventKind::MessageChunk
            | EventKind::ToolStarted
            | EventKind::ToolCompleted
            | EventKind::ToolFailed
            | EventKind::UsageReported
            | EventKind::TraceWarning
            | EventKind::GapDetected
    ) {
        return Err("kind_not_allowed");
    }
    let text = |k: &str, max: usize| match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.chars().take(max).collect::<String>())),
        Some(_) => Err("bad_field"),
    };
    let data = match o.get("data") {
        None | Some(Value::Null) => json!({}),
        Some(d) if d.is_object() => d.clone(),
        Some(_) => return Err("bad_data"),
    };
    let visibility = match o.get("visibility") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|_| "bad_visibility")?),
    };
    // A guest can say "derived" and nothing else: no `aibus_scheduler`, no `hook`.
    let origin = if o.get("origin").and_then(Value::as_str) == Some("derived") {
        Origin::Derived
    } else {
        Origin::CliStream
    };
    Ok(Proposal {
        kind,
        native_type: text("native_type", 128)?,
        tool_use_id: text("tool_use_id", 256)?,
        data,
        origin,
        visibility,
    })
}

/// Usage numbers are held to the quality the manifest declares, and to whole numbers up to 2^53,
/// as the declarative engine does.
fn clamp_metrics(data: &mut Value, quality: UsageQuality) -> Result<(), &'static str> {
    let Some(list) = data.get_mut("metrics") else {
        return Ok(());
    };
    let metrics: Vec<Metric> = serde_json::from_value(list.clone()).map_err(|_| "bad_metric")?;
    let fixed: Vec<Metric> = metrics
        .into_iter()
        .map(|mut m| {
            m.value = m
                .value
                .filter(|v| v.fract() == 0.0 && (0.0..=(1u64 << 53) as f64).contains(v))
                .filter(|_| quality != UsageQuality::Unknown);
            m.quality = match (m.value, quality, m.quality) {
                (None, _, _) => DataQuality::Unknown,
                (_, UsageQuality::Estimated, DataQuality::Reported) => DataQuality::Estimated,
                (Some(_), _, DataQuality::Unknown) => DataQuality::Estimated,
                (_, _, q) => q,
            };
            m
        })
        .collect();
    *list = serde_json::to_value(fixed).map_err(|_| "bad_metric")?;
    Ok(())
}

/// A parser hosted as a WASM guest. The events it hands over are checked and stamped by the host;
/// the events the host owns (start, stderr, gaps, the terminal event) come from the same code as the
/// declarative engine's.
pub struct Wasm {
    inner: Declarative,
    guest: Option<Guest>,
    usage_quality: UsageQuality,
    /// Why the guest stopped (or never started), once it has.
    failed: Option<String>,
}

impl Wasm {
    /// An instance of `module` for one attempt. Its start function, if any, runs here under the
    /// same fuel and deadline as a call.
    pub fn new(
        attempt: AttemptRef,
        module: &Module,
        limits: Limits,
        usage_quality: UsageQuality,
    ) -> Result<Self, WasmError> {
        let mut inner = Declarative::new(attempt, Vec::new(), usage_quality);
        inner.parser = PARSER;
        let host = Host {
            line: Vec::new(),
            seq: 0,
            emitted: Vec::new(),
            refused: Vec::new(),
            refused_total: 0,
            output_bytes: 0,
            max_event_bytes: limits.max_event_bytes,
            max_output_bytes: limits.max_output_bytes,
            fault: None,
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes)
                .memories(1)
                .tables(1)
                .table_elements(10_000)
                .instances(1)
                .trap_on_grow_failure(false)
                .build(),
        };
        let mut store = Store::new(&module.engine, host);
        store.limiter(|h| &mut h.limits);
        store.epoch_deadline_trap();
        let ticks = (limits.deadline.as_millis() / TICK.as_millis()).max(1) as u64;
        store.set_epoch_deadline(ticks);
        store
            .set_fuel(limits.fuel)
            .map_err(|e| WasmError::Instantiate(e.to_string()))?;
        let linker = link(&module.engine)?;
        let instance = linker
            .instantiate(&mut store, &module.module)
            .map_err(|e| {
                let why = match e.downcast_ref::<Trap>() {
                    Some(Trap::OutOfFuel) => "it ran out of fuel".to_owned(),
                    Some(Trap::Interrupt) => "it passed its deadline".to_owned(),
                    _ => format!("{e:#}"),
                };
                WasmError::Instantiate(why)
            })?;
        let mut entry = |name: &str| {
            instance
                .get_typed_func::<(), i32>(&mut store, name)
                .map_err(|e| WasmError::Export(e.to_string()))
        };
        let on_line = entry("on_line")?;
        let on_finish = entry("on_finish")?;
        Ok(Self {
            inner,
            guest: Some(Guest {
                store,
                on_line,
                on_finish,
                fuel: limits.fuel,
                ticks,
            }),
            usage_quality,
            failed: None,
        })
    }

    /// An engine for a package whose guest is not there to run (the module is missing, or this is
    /// the call that has only a manifest): it hosts nothing, says so once, and the attempt carries on.
    pub fn unavailable(attempt: AttemptRef, usage_quality: UsageQuality, why: &str) -> Self {
        let mut inner = Declarative::new(attempt, Vec::new(), usage_quality);
        inner.parser = PARSER;
        Self {
            inner,
            guest: None,
            usage_quality,
            failed: Some(why.to_owned()),
        }
    }

    /// One call into the guest. Events handed over before a failure are kept.
    fn call(
        &mut self,
        finish: bool,
        line: &[u8],
    ) -> (Vec<Vec<u8>>, Vec<&'static str>, Option<Cause>) {
        let Some(g) = self.guest.as_mut() else {
            return (Vec::new(), Vec::new(), None);
        };
        {
            let h = g.store.data_mut();
            h.line = line.to_vec();
            h.emitted.clear();
            h.refused.clear();
        }
        let _ = g.store.set_fuel(g.fuel);
        g.store.set_epoch_deadline(g.ticks);
        let r = if finish {
            g.on_finish.call(&mut g.store, ())
        } else {
            g.on_line.call(&mut g.store, ())
        };
        let h = g.store.data_mut();
        let cause = match r {
            Ok(0) => None,
            Ok(_) => Some(Cause::BadReturn),
            Err(e) => Some(
                h.fault
                    .take()
                    .unwrap_or_else(|| match e.downcast_ref::<Trap>() {
                        Some(Trap::OutOfFuel) => Cause::Fuel,
                        Some(Trap::Interrupt) => Cause::Deadline,
                        _ => Cause::Trap,
                    }),
            ),
        };
        h.line.clear();
        (
            std::mem::take(&mut h.emitted),
            std::mem::take(&mut h.refused),
            cause,
        )
    }

    /// The warning for one refused event.
    fn refusal(&mut self, why: &str, at: SystemTime) -> TraceEvent {
        let mut e = self.inner.host_event(
            EventKind::TraceWarning,
            Origin::AibusScheduler,
            Visibility::Observed,
            at,
        );
        e.data = json!({ "reason": "guest_event_refused", "why": why });
        e
    }

    /// Turns what one call handed over into stamped events. `line` is the stdout line the call was
    /// for (`None` at the end of the stream).
    fn stamp(
        &mut self,
        raw: Vec<Vec<u8>>,
        refused: Vec<&'static str>,
        cause: Option<Cause>,
        line: Option<(&str, &[u8])>,
        at: SystemTime,
    ) -> Vec<TraceEvent> {
        let mut out: Vec<TraceEvent> = refused.into_iter().map(|w| self.refusal(w, at)).collect();
        for bytes in raw {
            let p = match proposal(&bytes).and_then(|mut p| {
                if p.kind == EventKind::UsageReported {
                    clamp_metrics(&mut p.data, self.usage_quality)?;
                }
                Ok(p)
            }) {
                Ok(p) => p,
                Err(why) => {
                    out.push(self.refusal(why, at));
                    continue;
                }
            };
            // The clamp: from a line, `cli_stream` observed is the most a guest gets; asking for
            // `derived`, or having no line to cite, gives `derived` and at best `inferred`.
            let origin = if line.is_some() {
                p.origin
            } else {
                Origin::Derived
            };
            let ceiling = if origin == Origin::Derived {
                Visibility::Inferred
            } else {
                Visibility::Observed
            };
            let want = p.visibility.unwrap_or(if p.kind == EventKind::GapDetected {
                Visibility::Unavailable
            } else {
                ceiling
            });
            let visibility = want.max(ceiling);
            let mut e = match line {
                Some((key, raw)) => self.inner.event(key, raw, p.kind, origin, visibility, at),
                None => self.inner.host_event(p.kind, origin, visibility, at),
            };
            e.native.native_type = p.native_type;
            e.native.tool_use_id = p.tool_use_id;
            if p.kind == EventKind::GapDetected && line.is_some() {
                self.inner.malformed += 1;
            }
            if p.kind == EventKind::TraceWarning && p.data["reason"] == "unknown_variant" {
                self.inner.unknown += 1;
            }
            if p.data["requires_reconciliation"] == true {
                self.inner.reconcile = true;
            }
            e.data = p.data;
            out.push(e);
        }
        if let Some(c) = cause {
            let why = format!("extension_failed:{}", c.word());
            self.failed = Some(why.clone());
            self.guest = None;
            out.extend(self.inner.gap(Channel::Host, &why, 0, at).events);
        }
        out
    }
}

impl Engine for Wasm {
    fn observe(&mut self, id: uuid::Uuid) {
        self.inner.observe(id);
    }

    fn start(&mut self, at: SystemTime, info: Value) -> Batch {
        let mut b = self.inner.start(at, info);
        if let Some(why) = self.failed.clone().filter(|_| self.guest.is_none()) {
            b.events.extend(
                self.inner
                    .gap(Channel::Host, &format!("extension_failed:{why}"), 0, at)
                    .events,
            );
        }
        b
    }

    fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.inner.lines += 1;
        self.inner.stdout_lines += 1;
        if self.guest.is_none() {
            return Batch::default();
        }
        let key = format!("stdout:L{}", self.inner.stdout_lines);
        let (raw, refused, cause) = self.call(false, line);
        let events = self.stamp(raw, refused, cause, Some((&key, line)), at);
        Batch {
            events,
            payload: None,
        }
    }

    fn hook_line(&mut self, _line: &[u8], _at: SystemTime) -> Batch {
        Batch::default()
    }

    fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.inner.stderr_line(line, at)
    }

    fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch {
        self.inner.gap(channel, reason, bytes, at)
    }

    fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch {
        if self.inner.ended {
            return Batch::default();
        }
        self.inner.ended = true;
        let mut out = self.inner.close(ending, at);
        if self.guest.is_some() {
            let (raw, refused, cause) = self.call(true, &[]);
            out.extend(self.stamp(raw, refused, cause, None, at));
        }
        out.push(self.inner.terminal(ending, wall, at));
        Batch {
            events: out,
            payload: None,
        }
    }
}
