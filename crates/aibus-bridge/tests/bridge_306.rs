//! #306: the bridge answers read-only requests from the trace projections, a subscriber recovers a
//! dropped batch by cursor without losing order, and a schema mismatch says what to do.

use std::fs;
use std::path::Path;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, SystemTime};

use aibus_bridge::{
    handle, handle_json, stream, Command, ErrorCode, EventBatch, Reply, Request, Response,
    Subscriber, Visibility, BRIDGE_SCHEMA, MAX_BATCH,
};
use aibus_trace::{AttemptRef, EventIds, EventKind, Origin, TraceEvent};
use serde_json::json;
use uuid::Uuid;

/// A run of `chunks` message events after a lead agent, a delegation, a child and one tool, written
/// as `<root>/run-a/events.jsonl` the way the store writes it.
fn write_run(root: &Path, chunks: u64) {
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    };
    let mut ids = EventIds::default();
    let mut text = String::new();
    let mut push = |kind: EventKind, vis, f: &dyn Fn(&mut TraceEvent)| {
        let n = text.lines().count() as u64 + 1;
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + n);
        let mut e = TraceEvent::new(
            &attempt,
            ids.next(at, &[&n.to_le_bytes()]),
            kind,
            Origin::CliStream,
            vis,
            at,
            "test/1",
        );
        f(&mut e);
        text.push_str(&format!("{}\n", serde_json::to_string(&e).unwrap()));
    };
    use aibus_trace::Visibility::{Inferred, Observed};
    push(EventKind::AttemptStarted, Observed, &|_| {});
    push(EventKind::AgentStarted, Observed, &|e| {
        e.agent_id = Some("lead".into());
        e.data = json!({ "role": "lead" });
    });
    push(EventKind::TaskDelegated, Inferred, &|e| {
        e.agent_id = Some("lead".into());
        e.native.tool_use_id = Some("toolu_A".into());
    });
    push(EventKind::AgentStarted, Inferred, &|e| {
        e.agent_id = Some("child".into());
        e.parent_agent_id = Some("lead".into());
        e.parent_tool_use_id = Some("toolu_A".into());
    });
    push(EventKind::ToolStarted, Observed, &|e| {
        e.agent_id = Some("child".into());
        e.native.tool_use_id = Some("toolu_R".into());
        e.data = json!({ "tool": "Read" });
    });
    for _ in 0..chunks {
        push(EventKind::MessageChunk, Observed, &|_| {});
    }
    let dir = root.join("run-a");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn batch(root: &Path, after: Option<&str>, limit: u32) -> EventBatch {
    let req = Request {
        schema_version: BRIDGE_SCHEMA,
        command: Command::Events {
            run: "run-a".into(),
            after: after.map(str::to_owned),
            limit: Some(limit),
        },
    };
    match handle(root, &req).unwrap() {
        Reply::Events { batch } => batch,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_dropped_batch_is_a_gap_and_resubscribe_recovers_the_events_in_order_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 7); // 12 events
    let mut sub = Subscriber {
        run: "run-a".into(),
        cursor: None,
    };
    let b1 = batch(tmp.path(), None, 4);
    let b2 = batch(tmp.path(), b1.next.as_deref(), 4);
    let b3 = batch(tmp.path(), b2.next.as_deref(), 4);
    let mut seen: Vec<String> = Vec::new();
    sub.apply(&b1).unwrap();
    seen.extend(b1.events.iter().map(|e| e.event_id.clone()));
    // The connection drops and b2 never arrives: b3 does not continue from the cursor.
    let e = sub.apply(&b3).unwrap_err();
    assert_eq!(e.code, ErrorCode::Gap);
    assert_eq!(
        sub.cursor, b1.next,
        "a refused batch leaves the cursor alone"
    );
    // Resubscribe from the cursor and apply what comes back until caught up.
    loop {
        let Reply::Events { batch } = handle(tmp.path(), &sub.request(Some(4))).unwrap() else {
            panic!()
        };
        if sub.apply(&batch).unwrap() == 0 {
            break;
        }
        seen.extend(batch.events.iter().map(|e| e.event_id.clone()));
    }
    assert_eq!(seen.len(), 12);
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "ordered, no repeats");
    let all = batch(tmp.path(), None, MAX_BATCH);
    let all: Vec<_> = all.events.iter().map(|e| e.event_id.clone()).collect();
    assert_eq!(seen, all);
}

#[test]
fn a_repeated_or_reordered_batch_is_refused_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 3);
    let mut sub = Subscriber {
        run: "run-a".into(),
        cursor: None,
    };
    let b1 = batch(tmp.path(), None, 3);
    sub.apply(&b1).unwrap();
    assert_eq!(
        sub.apply(&b1).unwrap_err().code,
        ErrorCode::Gap,
        "replayed batch"
    );
    let mut b2 = batch(tmp.path(), b1.next.as_deref(), 3);
    b2.events.reverse();
    assert_eq!(
        sub.apply(&b2).unwrap_err().code,
        ErrorCode::Gap,
        "reordered batch"
    );
}

#[test]
fn a_bounded_channel_applies_back_pressure_and_loses_nothing_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 7);
    let (tx, rx) = sync_channel::<EventBatch>(1);
    let sent = stream(tmp.path(), "run-a", None, Some(4), &tx).unwrap();
    assert_eq!(
        sent, 1,
        "the second batch found the channel full and stopped"
    );
    let mut sub = Subscriber {
        run: "run-a".into(),
        cursor: None,
    };
    let mut n = sub.apply(&rx.recv().unwrap()).unwrap();
    while n < 12 {
        stream(tmp.path(), "run-a", sub.cursor.as_deref(), Some(4), &tx).unwrap();
        n += sub.apply(&rx.recv().unwrap()).unwrap();
    }
    assert_eq!(n, 12);
    drop(rx);
    let e = stream(tmp.path(), "run-a", None, Some(4), &tx).unwrap_err();
    assert_eq!(
        e.code,
        ErrorCode::Io,
        "a closed channel is the UI going away"
    );
}

#[test]
fn batches_are_bounded_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 600);
    let b = batch(tmp.path(), None, 100_000);
    assert_eq!(b.events.len(), MAX_BATCH as usize);
    assert!(b.more);
}

#[test]
fn an_unknown_cursor_asks_for_a_reload_and_the_projection_head_resyncs_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 2);
    let req = |after: &str| Request {
        schema_version: BRIDGE_SCHEMA,
        command: Command::Events {
            run: "run-a".into(),
            after: Some(after.into()),
            limit: None,
        },
    };
    let e = handle(tmp.path(), &req(&Uuid::from_u128(9).to_string())).unwrap_err();
    assert_eq!(e.code, ErrorCode::CursorUnknown);
    assert!(e.message.contains("reload the projection"), "{}", e.message);
    assert_eq!(
        handle(tmp.path(), &req("nope")).unwrap_err().code,
        ErrorCode::BadCursor
    );

    let proj = Request {
        schema_version: BRIDGE_SCHEMA,
        command: Command::Projection {
            run: "run-a".into(),
        },
    };
    let Reply::Projection { projection } = handle(tmp.path(), &proj).unwrap() else {
        panic!()
    };
    let mut sub = Subscriber {
        run: "run-a".into(),
        cursor: Some(Uuid::from_u128(9).to_string()),
    };
    sub.resync(&projection);
    let Reply::Events { batch } = handle(tmp.path(), &sub.request(None)).unwrap() else {
        panic!()
    };
    assert!(
        batch.events.is_empty() && !batch.more,
        "caught up at the head"
    );
}

#[test]
fn the_projection_keeps_the_trace_visibility_on_edges_and_tools_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 0);
    let proj = Request {
        schema_version: BRIDGE_SCHEMA,
        command: Command::Projection {
            run: "run-a".into(),
        },
    };
    let Reply::Projection { projection: p } = handle(tmp.path(), &proj).unwrap() else {
        panic!()
    };
    let ids: Vec<_> = p.agents.iter().map(|a| a.agent_id.as_str()).collect();
    assert_eq!(ids, ["lead", "child"]);
    assert_eq!(p.tools.len(), 1);
    assert_eq!(p.tools[0].name.as_deref(), Some("Read"));
    assert_eq!(p.edges.len(), 1);
    assert_eq!(
        p.edges[0].level,
        Visibility::Inferred,
        "never presented as observed"
    );
    assert!(!p.edges[0].sources.is_empty());
    assert!(p.head.is_some());

    let Reply::Runs { runs } = handle(
        tmp.path(),
        &Request {
            schema_version: BRIDGE_SCHEMA,
            command: Command::ListRuns,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].id.as_str(), runs[0].events, runs[0].edges[2]),
        ("run-a", 5, 1)
    );
}

#[test]
fn the_ui_has_no_command_that_runs_or_writes_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 1);
    let before = fs::read(tmp.path().join("run-a/events.jsonl")).unwrap();
    for cmd in [
        json!({"cmd": "exec", "argv": ["sh", "-c", "id"]}),
        json!({"cmd": "append", "run": "run-a", "events": []}),
        json!({"cmd": "events", "run": "../../etc", "after": null, "limit": null, "path": "/etc/passwd"}),
    ] {
        let body = json!({"schema_version": BRIDGE_SCHEMA, "command": cmd}).to_string();
        let out: Response = serde_json::from_str(&handle_json(tmp.path(), &body)).unwrap();
        let Response::Error { error } = out else {
            panic!("{cmd} was accepted")
        };
        assert!(
            matches!(error.code, ErrorCode::BadRequest | ErrorCode::UnknownRun),
            "{cmd}: {error}"
        );
    }
    assert_eq!(
        fs::read(tmp.path().join("run-a/events.jsonl")).unwrap(),
        before
    );
}

#[test]
fn a_schema_mismatch_names_the_fix_306() {
    let tmp = tempfile::tempdir().unwrap();
    write_run(tmp.path(), 0);
    for (v, fix) in [
        (0, "update the desktop app"),
        (BRIDGE_SCHEMA + 1, "update aibus"),
    ] {
        let req = Request {
            schema_version: v,
            command: Command::ListRuns,
        };
        let e = handle(tmp.path(), &req).unwrap_err();
        assert_eq!(e.code, ErrorCode::SchemaMismatch);
        assert!(e.message.contains(fix), "{}", e.message);
    }
    // A trace written under another trace schema is refused with the same code.
    let dir = tmp.path().join("run-old");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        "{\"schema_version\":1,\"sequence\":1}\n",
    )
    .unwrap();
    let req = Request {
        schema_version: BRIDGE_SCHEMA,
        command: Command::Projection {
            run: "run-old".into(),
        },
    };
    let e = handle(tmp.path(), &req).unwrap_err();
    assert_eq!(e.code, ErrorCode::SchemaMismatch);
    assert!(e.message.contains("update aibus"), "{}", e.message);
}
