//! #292: no outbound traffic unless a destination was configured, and an exporter outage never
//! reaches the caller.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use aibus_core::redact::Redactor;
use aibus_otel::{export, Destination, Outcome};
use aibus_trace::otel::{to_otlp, Revisions};
use serde_json::json;

fn dest(endpoint: String, timeout_ms: u64) -> Destination {
    Destination {
        endpoint,
        headers: vec![("authorization".into(), "Basic dGVzdDp0ZXN0".into())],
        timeout: Duration::from_millis(timeout_ms),
    }
}

#[test]
fn no_destination_means_no_connection_even_with_otel_env_set() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    // The standard OTel variables must not switch export on by themselves.
    std::env::set_var(
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        format!("http://127.0.0.1:{port}"),
    );
    std::env::set_var(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        format!("http://127.0.0.1:{port}/v1/traces"),
    );
    let (body, _) = to_otlp(&[], &Revisions::default(), &Redactor::new());
    assert_eq!(export(None, &body), Outcome::Disabled);
    assert_eq!(
        std::thread::spawn(move || export(None, &body))
            .join()
            .unwrap(),
        Outcome::Disabled
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        l.accept().is_err(),
        "something connected although no destination was configured"
    );
}

#[test]
fn a_dead_collector_is_a_failed_outcome_not_an_error_in_the_run() {
    // Bind then drop: the port is closed, connections are refused.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let t = Instant::now();
    let d = dest(format!("http://127.0.0.1:{port}/v1/traces"), 500);
    let h = std::thread::spawn(move || export(Some(&d), &json!({})));
    assert!(
        t.elapsed() < Duration::from_millis(100),
        "starting the thread must not wait for the collector"
    );
    assert!(matches!(h.join().unwrap(), Outcome::Failed(_)));
}

#[test]
fn a_hung_collector_is_cut_off_by_the_timeout() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let keep = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        std::thread::sleep(Duration::from_secs(2));
        drop(s);
    });
    let t = Instant::now();
    let out = export(
        Some(&dest(format!("http://127.0.0.1:{port}/v1/traces"), 300)),
        &json!({}),
    );
    assert!(matches!(out, Outcome::Failed(_)), "{out:?}");
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
    keep.join().unwrap();
}

#[test]
fn a_collector_error_status_is_a_failed_outcome() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let srv = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        s.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .unwrap();
    });
    let out = export(
        Some(&dest(format!("http://127.0.0.1:{port}/v1/traces"), 20_000)),
        &json!({}),
    );
    assert!(matches!(out, Outcome::Failed(m) if m.contains("503")));
    srv.join().unwrap();
}

#[test]
fn a_configured_destination_receives_the_otlp_body_with_its_headers() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let srv = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&got).contains("resourceSpans") {
            let n = s.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&got).to_lowercase()
    });
    let (body, _) = to_otlp(&[], &Revisions::default(), &Redactor::new());
    let out = export(
        Some(&dest(format!("http://127.0.0.1:{port}/v1/traces"), 20_000)),
        &body,
    );
    assert_eq!(out, Outcome::Sent);
    let req = srv.join().unwrap();
    assert!(req.starts_with("post /v1/traces"), "{req}");
    assert!(req.contains("authorization: basic"));
}

/// Needs a collector: `AIBUS_OTLP_TEST_ENDPOINT=http://host:4318/v1/traces cargo test -- --ignored`.
/// Not part of a gate; run by hand against a local collector (docs/OTEL.md).
#[test]
#[ignore = "needs a running OTLP collector"]
fn a_local_collector_accepts_the_export() {
    let ep = std::env::var("AIBUS_OTLP_TEST_ENDPOINT").expect("AIBUS_OTLP_TEST_ENDPOINT");
    use aibus_trace::{AttemptRef, EventIds, EventKind, Origin, TraceEvent, Visibility};
    let a = AttemptRef {
        workflow_run_id: uuid::Uuid::from_u128(0xa1b2),
        attempt_id: uuid::Uuid::from_u128(0xc3d4),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    };
    let at = std::time::SystemTime::now();
    let mut ids = EventIds::default();
    let ev = [EventKind::AttemptStarted, EventKind::AttemptCompleted].map(|k| {
        let mut e = TraceEvent::new(
            &a,
            ids.next(at, &[&[k as u8]]),
            k,
            Origin::AibusScheduler,
            Visibility::Observed,
            at,
            "t/1",
        );
        e.data = json!({ "status": "completed" });
        e
    });
    let rev = Revisions {
        prompt: Some("prompt@1".into()),
        workflow: Some("wf@1".into()),
    };
    let (body, c) = to_otlp(&ev, &rev, &Redactor::new());
    assert_eq!(c.spans, 2);
    assert_eq!(export(Some(&dest(ep, 5000)), &body), Outcome::Sent);
}
