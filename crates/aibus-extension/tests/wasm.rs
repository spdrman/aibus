//! The WASM guest host (#333): what a guest may do, and what the host does when it does more. The
//! hostile guests are written as WAT so each one is a few lines; the real one is in
//! `wasm_conformance.rs`. Every test here is fast: the fuel, deadline and output limits are small.
#![cfg(feature = "wasm")]

use std::time::{Duration, Instant, SystemTime};

use aibus_extension::manifest::UsageQuality;
use aibus_extension::wasm::{Limits, Module, Wasm, WasmError, MAX_EVENTS_PER_CALL};
use aibus_supervisor::{Ending, Engine};
use aibus_trace::{AttemptRef, EventKind, Origin, TraceEvent, Visibility};
use uuid::Uuid;

fn attempt() -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(0x333_0001),
        attempt_id: Uuid::from_u128(0x333_0002),
        parent_attempt_id: None,
        provider_extension: "org.example.aibus.examplecli".into(),
    }
}

/// `s` as a WAT string literal with every byte escaped.
fn lit(s: &str) -> String {
    s.bytes().map(|b| format!("\\{b:02x}")).collect()
}

const IMPORTS: &str = r#"
  (import "aibus" "read_line" (func $read (param i32 i32) (result i32)))
  (import "aibus" "emit" (func $emit (param i32 i32) (result i32)))
  (import "aibus" "next_seq" (func $seq (result i64)))
  (memory (export "memory") 1)"#;

/// A guest module: `line` and `finish` are the bodies of `on_line` and `on_finish` (each leaves an i32).
fn guest(extra: &str, line: &str, finish: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module {IMPORTS} {extra}
  (func (export "on_line") (result i32) {line})
  (func (export "on_finish") (result i32) {finish}))"#
    ))
    .expect("the test WAT is valid")
}

/// A guest whose `on_line` emits each of `line` and whose `on_finish` emits each of `finish`.
fn emitting(line: &[&str], finish: &[&str]) -> Vec<u8> {
    let mut data = String::new();
    let mut at = 1024usize;
    let mut call = |jsons: &[&str]| {
        let mut body = String::new();
        for j in jsons {
            data += &format!("(data (i32.const {at}) \"{}\")\n", lit(j));
            body += &format!(
                "(drop (call $emit (i32.const {at}) (i32.const {})))\n",
                j.len()
            );
            at += j.len() + 8;
        }
        body + "(i32.const 0)"
    };
    let (l, f) = (call(line), call(finish));
    guest(&data, &l, &f)
}

fn drive(wasm: &[u8], limits: Limits, lines: &[&str]) -> Vec<TraceEvent> {
    drive_as(wasm, limits, UsageQuality::Reported, lines)
}

fn drive_as(wasm: &[u8], limits: Limits, q: UsageQuality, lines: &[&str]) -> Vec<TraceEvent> {
    let module = Module::load(wasm).expect("the module loads");
    let mut e = Wasm::new(attempt(), &module, limits, q).expect("the guest starts");
    let mut at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut out = e.start(at, serde_json::json!({})).events;
    for l in lines {
        at += Duration::from_millis(10);
        out.extend(e.stream_line(l.as_bytes(), at).events);
    }
    at += Duration::from_millis(10);
    out.extend(
        e.finish(Ending::Exited(0), Duration::from_secs(1), at)
            .events,
    );
    out
}

fn gaps(ev: &[TraceEvent]) -> Vec<String> {
    ev.iter()
        .filter(|e| e.kind == EventKind::GapDetected)
        .filter_map(|e| e.data["reason"].as_str().map(str::to_owned))
        .collect()
}

fn kinds(ev: &[TraceEvent]) -> Vec<EventKind> {
    ev.iter().map(|e| e.kind).collect()
}

const LINE: &str = r#"{"type":"text","text":"hi"}"#;

// ---- the well-behaved guest -------------------------------------------------------------

#[test]
fn a_guest_that_emits_one_event_per_line_produces_host_stamped_events() {
    let w = emitting(&[r#"{"kind":"message_chunk","data":{"bytes":3}}"#], &[]);
    let ev = drive(&w, Limits::default(), &[LINE, LINE]);
    assert_eq!(
        kinds(&ev),
        [
            EventKind::AttemptStarted,
            EventKind::MessageChunk,
            EventKind::MessageChunk,
            EventKind::AttemptCompleted
        ]
    );
    let chunk = &ev[1];
    assert_eq!(chunk.origin, Origin::CliStream);
    assert_eq!(chunk.visibility, Visibility::Observed);
    assert_eq!(chunk.evidence_refs, ["raw:stdout:L1"]);
    assert!(chunk.source.raw_sha256.is_some());
    assert_eq!(ev[2].evidence_refs, ["raw:stdout:L2"]);
    assert!(ev.windows(2).all(|w| w[0].event_id < w[1].event_id));
    assert!(ev.iter().all(|e| e.event_id.get_version_num() == 7));
}

#[test]
fn the_guest_reads_exactly_the_line_and_a_too_small_buffer_is_minus_one() {
    // 27 bytes, first byte `{`; a 4 byte buffer cannot hold it and gets -1 without a partial copy.
    let ok = guest(
        "",
        "(if (i32.ne (call $read (i32.const 0) (i32.const 1024)) (i32.const 27)) (then unreachable))
         (if (i32.ne (i32.load8_u (i32.const 0)) (i32.const 123)) (then unreachable))
         (if (i32.ne (call $read (i32.const 0) (i32.const 4)) (i32.const -1)) (then unreachable))
         (i32.const 0)",
        "(i32.const 0)",
    );
    let ev = drive(&ok, Limits::default(), &[LINE]);
    assert!(gaps(&ev).is_empty(), "{:#?}", gaps(&ev));
}

#[test]
fn next_seq_is_monotonic_across_calls_and_lines() {
    let w = guest(
        "(global $last (mut i64) (i64.const 0))",
        "(local $a i64) (local $b i64)
         (local.set $a (call $seq)) (local.set $b (call $seq))
         (if (i64.le_u (local.get $a) (global.get $last)) (then unreachable))
         (if (i64.le_u (local.get $b) (local.get $a)) (then unreachable))
         (global.set $last (local.get $b))
         (i32.const 0)",
        "(i32.const 0)",
    );
    let ev = drive(&w, Limits::default(), &[LINE, LINE, LINE]);
    assert!(gaps(&ev).is_empty(), "{:#?}", gaps(&ev));
}

#[test]
fn stderr_and_hooks_never_reach_the_guest() {
    // A guest that traps if it is ever called: stderr and hook lines are the host's.
    let w = guest("", "unreachable", "(i32.const 0)");
    let module = Module::load(&w).unwrap();
    let mut e = Wasm::new(
        attempt(),
        &module,
        Limits::default(),
        UsageQuality::Reported,
    )
    .unwrap();
    let at = SystemTime::UNIX_EPOCH;
    let s = e.stderr_line(b"please log in", at);
    assert_eq!(s.events.len(), 1);
    assert_eq!(s.events[0].kind, EventKind::TraceWarning);
    assert!(e.hook_line(b"{}", at).events.is_empty());
}

// ---- limits: every one fails fast and fails closed ------------------------------------------

#[test]
fn a_guest_that_spins_is_stopped_by_fuel_and_the_attempt_goes_on() {
    let w = guest("", "(loop $l (br $l)) (i32.const 0)", "(i32.const 0)");
    let t = Instant::now();
    let ev = drive(&w, Limits::default(), &[LINE, LINE]);
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    // One gap, not one per remaining line, and the host still ends the attempt itself.
    assert_eq!(gaps(&ev), ["extension_failed:fuel"], "{ev:#?}");
    assert_eq!(ev.last().unwrap().kind, EventKind::AttemptCompleted);
}

#[test]
fn the_wall_deadline_stops_a_guest_that_fuel_would_let_run() {
    let w = guest("", "(loop $l (br $l)) (i32.const 0)", "(i32.const 0)");
    let limits = Limits {
        fuel: u64::MAX / 2,
        deadline: Duration::from_millis(100),
        ..Limits::default()
    };
    let t = Instant::now();
    let ev = drive(&w, limits, &[LINE]);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(gaps(&ev), ["extension_failed:deadline"], "{ev:#?}");
}

#[test]
fn memory_stops_at_the_limit_and_a_module_that_declares_more_is_refused_at_load() {
    // Grows 16 pages at a time until the host says no (-1); traps if it ever got past 16 MiB.
    let w = guest(
        "",
        "(local $pages i32)
         (block $done (loop $l
           (br_if $done (i32.eq (memory.grow (i32.const 16)) (i32.const -1)))
           (local.set $pages (i32.add (local.get $pages) (i32.const 16)))
           (br $l)))
         (if (i32.gt_u (i32.add (local.get $pages) (i32.const 1)) (i32.const 256)) (then unreachable))
         (i32.const 0)",
        "(i32.const 0)",
    );
    let ev = drive(&w, Limits::default(), &[LINE]);
    assert!(gaps(&ev).is_empty(), "{:#?}", gaps(&ev));

    let big = wat::parse_str(
        r#"(module (memory (export "memory") 2048)
             (func (export "on_line") (result i32) (i32.const 0))
             (func (export "on_finish") (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    assert!(matches!(Module::load(&big), Err(WasmError::Memory(_))));
}

#[test]
fn a_guest_that_traps_is_a_gap_and_not_a_panic() {
    let w = guest("", "unreachable", "(i32.const 0)");
    let ev = drive(&w, Limits::default(), &[LINE, LINE]);
    assert_eq!(gaps(&ev), ["extension_failed:trap"], "{ev:#?}");
}

#[test]
fn a_nonzero_return_is_a_failure_and_so_is_a_guest_that_runs_out_of_stack() {
    let w = guest("", "(i32.const 7)", "(i32.const 0)");
    assert_eq!(
        gaps(&drive(&w, Limits::default(), &[LINE])),
        ["extension_failed:bad_return"]
    );
    let r = guest(
        "(func $f (result i32) (call $f))",
        "(call $f)",
        "(i32.const 0)",
    );
    assert_eq!(
        gaps(&drive(&r, Limits::default(), &[LINE])),
        ["extension_failed:trap"]
    );
}

#[test]
fn a_pointer_outside_the_guests_memory_is_refused_not_followed() {
    let w = guest(
        "",
        "(drop (call $read (i32.const 65530) (i32.const 1000))) (i32.const 0)",
        "(i32.const 0)",
    );
    assert_eq!(
        gaps(&drive(&w, Limits::default(), &[LINE])),
        ["extension_failed:bad_pointer"]
    );
    let e = guest(
        "",
        "(drop (call $emit (i32.const 65500) (i32.const 1000))) (i32.const 0)",
        "(i32.const 0)",
    );
    assert_eq!(
        gaps(&drive(&e, Limits::default(), &[LINE])),
        ["extension_failed:bad_pointer"]
    );
}

#[test]
fn a_guest_that_returns_garbage_gets_warnings_and_no_events() {
    let w = emitting(
        &[
            "this is not json",
            r#"{"kind":"made_up_kind"}"#,
            r#"{"kind":"attempt_completed"}"#,
            r#"{"kind":"attempt_failed"}"#,
            r#"[1,2,3]"#,
            r#"{"kind":"message_chunk","data":"not an object"}"#,
        ],
        &[],
    );
    let ev = drive(&w, Limits::default(), &[LINE]);
    let warnings: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == EventKind::TraceWarning)
        .collect();
    assert_eq!(warnings.len(), 6, "{ev:#?}");
    assert!(warnings
        .iter()
        .all(|w| w.data["reason"] == "guest_event_refused"));
    // Only the host's own start and its one terminal event: the guest cannot end the attempt.
    assert_eq!(
        ev.iter()
            .filter(|e| !matches!(e.kind, EventKind::TraceWarning))
            .count(),
        2
    );
}

#[test]
fn a_guests_usage_numbers_are_held_to_the_quality_the_manifest_declares() {
    // The manifest says the CLI's usage is estimated, so a guest cannot upgrade it to reported.
    let w = emitting(
        &[
            r#"{"kind":"usage_reported","data":{"metrics":[{"name":"input_tokens","value":5.0,"unit":"tokens","quality":"reported","source":"x"}]}}"#,
        ],
        &[],
    );
    let ev = drive_as(&w, Limits::default(), UsageQuality::Estimated, &[LINE]);
    let m = &ev[1].data["metrics"][0];
    assert_eq!(m["quality"], "estimated");
    let ev = drive_as(&w, Limits::default(), UsageQuality::Unknown, &[LINE]);
    let m = &ev[1].data["metrics"][0];
    assert_eq!(m["quality"], "unknown");
    assert!(m["value"].is_null());
    let ev = drive_as(&w, Limits::default(), UsageQuality::Reported, &[LINE]);
    let m = &ev[1].data["metrics"][0];
    assert_eq!(m["quality"], "reported", "{m}");
}

// ---- the host owns ids and evidence ----------------------------------------------------------

#[test]
fn a_guest_cannot_choose_event_ids_origin_or_evidence_level() {
    let spoof = "11111111-1111-7111-8111-111111111111";
    let w = emitting(
        &[
            &format!(
                r#"{{"kind":"message_chunk","event_id":"{spoof}","attempt_id":"{spoof}","origin":"aibus_scheduler","visibility":"observed"}}"#
            ),
            r#"{"kind":"message_chunk","origin":"derived","visibility":"observed"}"#,
            r#"{"kind":"message_chunk","origin":"hook","visibility":"correlated"}"#,
        ],
        &[
            r#"{"kind":"gap_detected","origin":"cli_stream","visibility":"observed","data":{"reason":"x"}}"#,
        ],
    );
    let ev = drive(&w, Limits::default(), &[LINE]);
    let chunks: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == EventKind::MessageChunk)
        .collect();
    assert_eq!(chunks.len(), 3, "{ev:#?}");
    for c in &chunks {
        assert_ne!(c.event_id.to_string(), spoof);
        assert_eq!(c.attempt_id, attempt().attempt_id);
    }
    assert_eq!(
        (chunks[0].origin, chunks[0].visibility),
        (Origin::CliStream, Visibility::Observed)
    );
    // A guest that says "derived" gets derived, and derived is never observed.
    assert_eq!(chunks[1].origin, Origin::Derived);
    assert_ne!(chunks[1].visibility, Visibility::Observed);
    // "hook" is not an origin a guest has: it is the CLI stream, at most as strong as it asked.
    assert_eq!(chunks[2].origin, Origin::CliStream);
    assert!(chunks[2].visibility >= Visibility::Correlated);
    // Past the last line there is no raw line to cite: the guest's claim of "cli_stream observed" is
    // rewritten to derived and not observed, with no raw hash.
    let g = ev
        .iter()
        .find(|e| e.kind == EventKind::GapDetected)
        .unwrap();
    assert_eq!(g.origin, Origin::Derived);
    assert_ne!(g.visibility, Visibility::Observed);
    assert!(g.evidence_refs.is_empty() && g.source.raw_sha256.is_none());
    assert!(ev.windows(2).all(|w| w[0].event_id < w[1].event_id));
}

// ---- output limits ---------------------------------------------------------------------------

#[test]
fn a_guest_that_writes_too_much_is_cut_off_at_the_per_call_cap_and_then_failed() {
    let w = guest(
        &format!(
            "(data (i32.const 1024) \"{}\")",
            lit(r#"{"kind":"message_chunk"}"#)
        ),
        "(local $i i32)
         (loop $l
           (drop (call $emit (i32.const 1024) (i32.const 24)))
           (local.set $i (i32.add (local.get $i) (i32.const 1)))
           (br_if $l (i32.lt_u (local.get $i) (i32.const 5000))))
         (i32.const 0)",
        "(i32.const 0)",
    );
    let ev = drive(&w, Limits::default(), &[LINE]);
    let chunks = ev
        .iter()
        .filter(|e| e.kind == EventKind::MessageChunk)
        .count();
    assert_eq!(chunks, MAX_EVENTS_PER_CALL, "{ev:#?}");
    assert_eq!(gaps(&ev), ["extension_failed:output_limit"]);
}

#[test]
fn one_huge_event_is_refused_and_the_attempts_total_output_is_capped() {
    let huge = format!(
        r#"{{"kind":"message_chunk","data":{{"pad":"{}"}}}}"#,
        "a".repeat(40_000)
    );
    let w = emitting(&[&huge], &[]);
    // 40 KB inside a 64 KiB memory page, over the 16 KiB event cap.
    let ev = drive(&w, Limits::default(), &[LINE]);
    assert_eq!(
        kinds(&ev)
            .iter()
            .filter(|k| **k == EventKind::MessageChunk)
            .count(),
        0
    );
    assert!(ev
        .iter()
        .any(|e| e.kind == EventKind::TraceWarning && e.data["why"] == "event_too_big"));

    let small = emitting(&[r#"{"kind":"message_chunk","data":{"n":1}}"#], &[]);
    let limits = Limits {
        max_output_bytes: 100,
        ..Limits::default()
    };
    let ev = drive(&small, limits, &[LINE, LINE, LINE, LINE]);
    assert!(
        gaps(&ev).contains(&"extension_failed:output_limit".to_owned()),
        "{ev:#?}"
    );
    assert!(
        ev.iter()
            .filter(|e| e.kind == EventKind::MessageChunk)
            .count()
            < 4
    );
}

// ---- load: what a module may import and export ---------------------------------------------

fn module_with(imports: &str, exports_ok: bool) -> Vec<u8> {
    let tail = if exports_ok {
        r#"(func (export "on_line") (result i32) (i32.const 0))
           (func (export "on_finish") (result i32) (i32.const 0))"#
    } else {
        ""
    };
    wat::parse_str(format!(
        r#"(module {imports} (memory (export "memory") 1) {tail})"#
    ))
    .unwrap()
}

#[test]
fn a_module_that_imports_anything_outside_the_host_api_is_refused_at_load() {
    for (what, import) in [
        (
            "wasi write",
            r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#,
        ),
        (
            "wasi clock",
            r#"(import "wasi_snapshot_preview1" "clock_time_get" (func (param i32 i64 i32) (result i32)))"#,
        ),
        (
            "wasi environ",
            r#"(import "wasi_snapshot_preview1" "environ_get" (func (param i32 i32) (result i32)))"#,
        ),
        (
            "wasi path_open",
            r#"(import "wasi_snapshot_preview1" "path_open" (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))"#,
        ),
        (
            "an aibus function that does not exist",
            r#"(import "aibus" "open" (func (param i32) (result i32)))"#,
        ),
        (
            "emit with the wrong signature",
            r#"(import "aibus" "emit" (func (param i32) (result i32)))"#,
        ),
        (
            "read_line as a global",
            r#"(import "aibus" "read_line" (global i32))"#,
        ),
        (
            "an imported memory",
            r#"(import "env" "memory" (memory 1))"#,
        ),
        (
            "an imported table",
            r#"(import "env" "table" (table 1 funcref))"#,
        ),
        ("a host function in env", r#"(import "env" "abort" (func))"#),
    ] {
        let m = module_with(import, true);
        let r = Module::load(&m);
        // An imported memory next to a defined one is two memories: refused either way.
        assert!(
            matches!(r, Err(WasmError::Import(_)) | Err(WasmError::Compile(_))),
            "{what}: {r:?}"
        );
    }
    // The three functions of the host API, with the right signatures, load.
    assert!(Module::load(&guest("", "(i32.const 0)", "(i32.const 0)")).is_ok());
}

#[test]
fn a_module_without_the_entry_points_or_with_the_wrong_ones_is_refused_at_load() {
    let no_exports = module_with("", false);
    assert!(matches!(
        Module::load(&no_exports),
        Err(WasmError::Export(_))
    ));
    let wrong = wat::parse_str(
        r#"(module (memory (export "memory") 1)
             (func (export "on_line") (param i32) (result i32) (i32.const 0))
             (func (export "on_finish") (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    assert!(matches!(Module::load(&wrong), Err(WasmError::Export(_))));
    let no_memory = wat::parse_str(
        r#"(module (func (export "on_line") (result i32) (i32.const 0))
                   (func (export "on_finish") (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    assert!(matches!(
        Module::load(&no_memory),
        Err(WasmError::Export(_))
    ));
}

#[test]
fn bytes_that_are_not_a_module_and_shared_memory_are_refused_at_load() {
    assert!(matches!(
        Module::load(b"not wasm at all"),
        Err(WasmError::Compile(_))
    ));
    assert!(matches!(Module::load(b""), Err(WasmError::Compile(_))));
    let shared = wat::parse_str(
        r#"(module (memory (export "memory") 1 1 shared)
             (func (export "on_line") (result i32) (i32.const 0))
             (func (export "on_finish") (result i32) (i32.const 0)))"#,
    );
    // Threads are off: the module does not compile (or the WAT front end refuses it first).
    if let Ok(bytes) = shared {
        assert!(Module::load(&bytes).is_err());
    }
}

#[test]
fn a_start_function_that_spins_is_stopped_before_the_guest_is_used() {
    let w = guest(
        "(func $s (loop $l (br $l))) (start $s)",
        "(i32.const 0)",
        "(i32.const 0)",
    );
    let module = Module::load(&w).unwrap();
    let t = Instant::now();
    let r = Wasm::new(
        attempt(),
        &module,
        Limits::default(),
        UsageQuality::Reported,
    );
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(10));
}
