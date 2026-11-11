//! `aibus mcp --http` (E11, #106): loopback only, a bearer token on every request, the `Origin`
//! check and the protocol-version check, driven with a hand-written HTTP/1.1 client so the test
//! crate needs no HTTP dependency.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Stdio};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use aibus_testkit::xdg_temp;
use common::transcript::Transcript;
use serde_json::{json, Value};

const TOKEN: &str = "test-token-0123456789abcdef";

/// `aibus mcp --http 127.0.0.1:0` in the throwaway home, and the address it printed.
struct Http {
    child: Child,
    addr: String,
    stderr: Arc<Mutex<String>>,
}

impl Http {
    fn start(home: &std::path::Path, env_token: Option<&str>, extra: &[&str]) -> Self {
        let mut cmd = common::command(home);
        cmd.args(["mcp", "--http", "127.0.0.1:0"]).args(extra);
        match env_token {
            Some(t) => cmd.env("AIBUS_MCP_TOKEN", t),
            None => cmd.env_remove("AIBUS_MCP_TOKEN"),
        };
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("start aibus mcp");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let (err, sink) = (child.stderr.take().unwrap(), stderr.clone());
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut s = sink.lock().unwrap();
                s.push_str(&line);
                s.push('\n');
            }
        });
        let first = rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|e| {
            panic!("no listening line ({e}); stderr:\n{}", stderr.lock().unwrap());
        });
        let addr = first
            .strip_prefix("aibus mcp listening on http://")
            .and_then(|r| r.strip_suffix("/mcp"))
            .unwrap_or_else(|| panic!("unexpected first stdout line {first:?}"))
            .to_owned();
        Self { child, addr, stderr }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A POST to `/mcp` with the given extra headers; `Connection: close` so reading to the end
    /// is reading the whole answer.
    fn post(&self, headers: &[(&str, &str)], body: &Value) -> Reply {
        let body = body.to_string();
        let host = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("host")).map_or(self.addr.as_str(), |(_, v)| v);
        let mut head = format!(
            "POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (k, v) in headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("host")) {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        let mut s = TcpStream::connect(&self.addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        s.write_all(format!("{head}\r\n{body}").as_bytes()).unwrap();
        let mut raw = Vec::new();
        let _ = s.read_to_end(&mut raw);
        Reply::parse(&raw)
    }

    /// The headers of a request the server accepts, for `2026-07-28`.
    fn authed(&self) -> Vec<(&'static str, String)> {
        vec![("Authorization", format!("Bearer {TOKEN}")), ("MCP-Protocol-Version", "2026-07-28".into())]
    }

    /// A good 2026-07-28 JSON-RPC call; `extra` adds or overrides headers by name.
    fn rpc(&self, extra: &[(&str, &str)], method: &str, params: Value) -> (Value, Reply) {
        let mut params = params;
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": { "name": "http-test", "version": "1" },
        });
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let mut headers: Vec<(String, String)> =
            self.authed().into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        for (k, v) in extra {
            headers.retain(|(h, _)| !h.eq_ignore_ascii_case(k));
            headers.push(((*k).to_owned(), (*v).to_owned()));
        }
        let borrowed: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let reply = self.post(&borrowed, &request);
        (request, reply)
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Reply {
    fn parse(raw: &[u8]) -> Self {
        let text = String::from_utf8_lossy(raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let headers = head.to_ascii_lowercase();
        let body = if headers.contains("transfer-encoding: chunked") { dechunk(body) } else { body.to_owned() };
        Self { status, headers, body }
    }

    /// The JSON-RPC message in the body, whether it came as JSON or as one SSE event.
    fn json(&self) -> Value {
        if let Ok(v) = serde_json::from_str::<Value>(&self.body) {
            return v;
        }
        self.body
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .find(|v| v.get("result").is_some() || v.get("error").is_some())
            .unwrap_or_else(|| panic!("no JSON-RPC message in status {} body {:?}", self.status, self.body))
    }
}

fn dechunk(body: &str) -> String {
    let (mut rest, mut out) = (body, String::new());
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || tail.len() < n {
            break;
        }
        out.push_str(&tail[..n]);
        rest = tail[n..].trim_start_matches("\r\n");
    }
    out
}

#[test]
fn non_loopback_binds_are_refused_before_listening() {
    for addr in ["0.0.0.0:7777", "192.168.1.2:7777", "[::]:7777", "localhost:7777", "7777"] {
        let home = xdg_temp();
        let out = common::command(home.path()).args(["mcp", "--http", addr]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{addr}: {}", common::stderr(&out));
        assert!(out.stdout.is_empty(), "{addr}: something reached stdout");
        // Refused before the server (and with it the audit file) exists.
        assert!(!home.path().join(".local/state/aibus/logs/mcp").exists(), "{addr}: a server was created");
    }
    let home = xdg_temp();
    let out = common::command(home.path()).args(["mcp", "--http", "0.0.0.0:7777"]).output().unwrap();
    assert!(common::stderr(&out).contains("loopback"), "{}", common::stderr(&out));
    // The loopback range and ::1 are fine, and port 0 reports the port it got.
    let server = Http::start(home.path(), Some(TOKEN), &[]);
    assert!(server.addr.starts_with("127.0.0.1:") && !server.addr.ends_with(":0"), "{}", server.addr);
}

#[test]
fn a_missing_or_wrong_token_is_401() {
    let home = xdg_temp();
    let server = Http::start(home.path(), Some(TOKEN), &[]);
    for auth in [None, Some("Bearer wrong-token-0123456789"), Some(TOKEN), Some("Basic dXNlcjpwYXNz")] {
        let extra: Vec<(&str, &str)> = auth.map(|a| ("Authorization", a)).into_iter().collect();
        // No default Authorization either, so build the request by hand.
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
        let reply = server.post(&extra, &body);
        assert_eq!(reply.status, 401, "{auth:?}: {}", reply.body);
        assert!(reply.headers.contains("www-authenticate: bearer"), "{auth:?}: {}", reply.headers);
        assert!(!reply.body.contains(TOKEN));
    }
    // The right token on the same endpoint works.
    let (_, ok) = server.rpc(&[], "tools/list", json!({}));
    assert_eq!(ok.status, 200, "{}", ok.body);
}

#[test]
fn a_foreign_origin_is_403_and_no_origin_is_fine() {
    let home = xdg_temp();
    let server = Http::start(home.path(), Some(TOKEN), &["--origin", "https://tools.example"]);
    for origin in ["http://evil.example", "http://127.0.0.1:1", "http://localhost", "null"] {
        let (_, r) = server.rpc(&[("Origin", origin)], "tools/list", json!({}));
        assert_eq!(r.status, 403, "{origin}: {}", r.body);
    }
    for origin in [server.origin(), format!("http://localhost:{}", server.addr.rsplit(':').next().unwrap()), "https://tools.example".to_owned()] {
        let (_, r) = server.rpc(&[("Origin", &origin)], "tools/list", json!({}));
        assert_eq!(r.status, 200, "{origin}: {}", r.body);
    }
    let (_, none) = server.rpc(&[], "tools/list", json!({}));
    assert_eq!(none.status, 200, "{}", none.body);
    // A rebinding page keeps its own Host; it is refused whatever it knows.
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let rebound = server.post(&[("Authorization", &format!("Bearer {TOKEN}")), ("Host", "evil.example")], &body);
    assert_eq!(rebound.status, 403, "{}", rebound.body);
}

#[test]
fn an_unsupported_protocol_version_header_is_400() {
    let home = xdg_temp();
    let server = Http::start(home.path(), Some(TOKEN), &[]);
    let (_, r) = server.rpc(&[("MCP-Protocol-Version", "1999-01-01")], "tools/list", json!({}));
    assert_eq!(r.status, 400, "{}", r.body);
}

#[test]
fn the_legacy_handshake_works_over_http_too() {
    let home = xdg_temp();
    let server = Http::start(home.path(), Some(TOKEN), &[]);
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": { "name": "http-test", "version": "1" } },
    });
    let r = server.post(&[("Authorization", &format!("Bearer {TOKEN}"))], &init);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["result"]["serverInfo"]["name"], "aibus");
}

#[test]
fn the_list_scenario_matches_the_stdio_golden_transcript_over_http() {
    let home = xdg_temp();
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("ship.toml"),
        "name = \"ship\"\ndescription = \"Plan, build, check\"\n\n\
         [[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n\n\
         [[step]]\nagent = \"build\"\nprompt = \"Build it: {{prev.summary}}\"\n",
    )
    .unwrap();
    let server = Http::start(home.path(), Some(TOKEN), &[]);
    let mut transcript = Transcript::new(home.path());
    for (method, params) in [
        ("tools/list", json!({})),
        ("tools/call", json!({ "name": "workflow_list", "arguments": {} })),
    ] {
        let (request, reply) = server.rpc(&[], method, params);
        assert_eq!(reply.status, 200, "{}", reply.body);
        transcript.record(&request, &reply.json());
    }
    // The same file the stdio server is held to; the normaliser hides the transport.
    transcript.assert_matches("list");
}

#[test]
fn the_http_token_is_never_on_disk_or_in_any_output() {
    let mut home = xdg_temp();
    // A generated token: the only place it may appear is the one stderr line that shows it.
    let mut server = Http::start(home.path(), None, &[]);
    thread::sleep(Duration::from_millis(200));
    let err = server.stderr.lock().unwrap().clone();
    let token = err
        .lines()
        .find_map(|l| l.strip_prefix("Authorization: Bearer "))
        .unwrap_or_else(|| panic!("no Authorization line on stderr:\n{err}"))
        .to_owned();
    assert_eq!(token.len(), 64, "32 random bytes as hex");
    home.forbid(&token);
    let auth = format!("Bearer {token}");
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let _ = server.post(&[("Authorization", &auth)], &body);
    // A call that fails with the token in an argument, to feed the audit log and the log.
    let bad = server.post(&[("Authorization", &auth), ("MCP-Protocol-Version", "2026-07-28")], &json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": format!("tool-{token}"), "arguments": { "t": token },
                    "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                               "io.modelcontextprotocol/clientCapabilities": {},
                               "io.modelcontextprotocol/clientInfo": { "name": "http-test", "version": "1" } } },
    }));
    assert!(!bad.body.contains(&token), "the token came back in a response: {}", bad.body);
    let _ = server.child.kill();
    let _ = server.child.wait();
    thread::sleep(Duration::from_millis(200));
    let err = server.stderr.lock().unwrap().clone();
    assert_eq!(err.matches(&token).count(), 1, "the token is shown once and only once:\n{err}");
    // The audit file records the call, with the peer address and without the token.
    let dir = home.path().join(".local/state/aibus/logs/mcp");
    let log: String = fs::read_dir(&dir).unwrap().flatten().map(|f| fs::read_to_string(f.path()).unwrap()).collect();
    assert!(!log.contains(&token));
    assert!(log.contains("\"addr\":\"127.0.0.1:"), "the audit line lacks the client address:\n{log}");
    // `aibus doctor` with the token in the environment does not print it either.
    let doctor = common::command(home.path()).env("AIBUS_MCP_TOKEN", &token).arg("doctor").output().unwrap();
    assert!(!String::from_utf8_lossy(&doctor.stdout).contains(&token));
    assert!(!common::stderr(&doctor).contains(&token));
}

#[cfg(unix)]
#[test]
fn sigterm_stops_the_server_cleanly() {
    let home = xdg_temp();
    let mut server = Http::start(home.path(), Some(TOKEN), &[]);
    let killed = std::process::Command::new("kill").args(["-TERM", &server.child.id().to_string()]).status().unwrap();
    assert!(killed.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(s) = server.child.try_wait().unwrap() {
            break s;
        }
        assert!(std::time::Instant::now() < deadline, "the server ignored SIGTERM");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0), "{}", server.stderr.lock().unwrap());
}
