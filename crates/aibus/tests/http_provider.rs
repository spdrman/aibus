//! #203: the runtimes the binary builds can do network IO. Every other test of the account and
//! run paths uses the fake provider, which never opens a socket, so a runtime without an IO
//! driver went unnoticed. Here a loopback server stands in for an OpenAI-compatible provider.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use aibus_testkit::xdg_temp;
use common::{command, stderr};

/// Answers `GET .../models` with one model and everything else with a 401, and keeps the request
/// lines it saw. It lives for the rest of the test process.
fn server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = vec![0u8; 16 * 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let line = head.lines().next().unwrap_or_default().to_owned();
                let (status, body) = if line.starts_with("GET ") {
                    ("200 OK", r#"{"object":"list","data":[{"id":"llama3","object":"model"}]}"#)
                } else {
                    ("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#)
                };
                log.lock().unwrap().push(line);
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            });
        }
    });
    (base, seen)
}

#[test]
fn account_add_verifies_the_key_over_http() {
    let home = xdg_temp();
    let (base, seen) = server();
    let out = command(home.path())
        .env("LOCAL_KEY", "sk-http-0123456789abcdef")
        .args(["account", "add", "local", "--provider", "openai-compatible", "--base-url", &base, "--env", "LOCAL_KEY"])
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(!err.contains("IO is disabled") && !err.contains("panicked"), "{err}");
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(seen.lock().unwrap().iter().any(|l| l.starts_with("GET /v1/models")), "{:?}", seen.lock().unwrap());
}

#[test]
fn a_run_reaches_the_provider_over_http() {
    let home = xdg_temp();
    let (base, seen) = server();
    let added = command(home.path())
        .env("LOCAL_KEY", "sk-http-0123456789abcdef")
        .args(["account", "add", "local", "--provider", "openai-compatible", "--base-url", &base, "--env", "LOCAL_KEY"])
        .output()
        .unwrap();
    assert_eq!(added.status.code(), Some(0), "{}", stderr(&added));
    let out = command(home.path())
        .env("LOCAL_KEY", "sk-http-0123456789abcdef")
        .args(["run", "--json", "--model", "llama3", "say hi"])
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(!err.contains("IO is disabled") && !err.contains("panicked"), "{err}");
    assert!(seen.lock().unwrap().iter().any(|l| l.starts_with("POST ")), "the run never sent a request: {:?}", seen.lock().unwrap());
}
