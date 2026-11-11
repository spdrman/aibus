//! A scripted MCP client for the conformance tests (#96): it starts `aibus mcp` (or any other
//! stdio server) and speaks newline-delimited JSON-RPC to it, in one of the two protocols the
//! server has to answer.
//!
//! * `2026-07-28` is stateless: no handshake, and every request carries `_meta` with the
//!   protocol version and the client's capabilities (and its name, which the audit log reads).
//! * `2025-11-25` opens with `initialize` and `notifications/initialized`.
//!
//! The client keeps every line the server wrote to stdout, so a test can check afterwards that
//! nothing but JSON-RPC came out (`stdout`, and `finish`). When the server cannot be started, or
//! closes its stdout before it answers (`aibus mcp` on a binary that has no such subcommand
//! does exactly that), the panic says so, with the exit status and what the server wrote to
//! stderr, rather than failing on a missing field somewhere downstream.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::transcript::Transcript;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";

/// How long a test waits for one message before it calls the server hung.
const ANSWER: Duration = Duration::from_secs(20);

pub const CLIENT_NAME: &str = "aibus-conformance-client";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    V2026_07_28,
    V2025_11_25,
}

impl Protocol {
    pub const BOTH: [Protocol; 2] = [Protocol::V2026_07_28, Protocol::V2025_11_25];

    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::V2026_07_28 => "2026-07-28",
            Protocol::V2025_11_25 => "2025-11-25",
        }
    }
}

/// A JSON-RPC error answer.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

pub type Reply = Result<Value, RpcError>;

/// What is left after the server has been closed.
pub struct Finished {
    pub status: ExitStatus,
    pub stderr: String,
}

pub struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
    pub protocol: Protocol,
    /// Every line the server wrote to stdout, in order.
    pub stdout: Vec<String>,
    /// The exchanges so far, for `assert_matches`.
    pub transcript: Transcript,
}

/// `aibus mcp <args>` in the throwaway home, the way every other integration test starts it.
pub fn mcp_command(home: &Path, args: &[&str]) -> Command {
    let mut c = super::command(home);
    c.arg("mcp").args(args);
    c
}

impl McpClient {
    /// A `2026-07-28` client: no handshake, `_meta` on every request.
    pub fn v2026_07_28(home: &Path, args: &[&str]) -> Self {
        Self::spawn(mcp_command(home, args), Protocol::V2026_07_28, home)
    }

    /// A `2025-11-25` client, past `initialize` and `notifications/initialized`.
    pub fn v2025_11_25(home: &Path, args: &[&str]) -> Self {
        let mut c = Self::spawn(mcp_command(home, args), Protocol::V2025_11_25, home);
        c.initialize_legacy().expect("the server refused initialize");
        c
    }

    /// Either one, by value, so a test can loop over `Protocol::BOTH`.
    pub fn start(home: &Path, protocol: Protocol, args: &[&str]) -> Self {
        match protocol {
            Protocol::V2026_07_28 => Self::v2026_07_28(home, args),
            Protocol::V2025_11_25 => Self::v2025_11_25(home, args),
        }
    }

    /// Starts `cmd` with all three standard streams piped and no handshake. `home` is only what
    /// the transcript normaliser replaces with `<home>`.
    pub fn spawn(mut cmd: Command, protocol: Protocol, home: &Path) -> Self {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start the server {cmd:?}: {e}"));
        let stdin = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let err = child.stderr.take().unwrap();
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut s = sink.lock().unwrap();
                s.push_str(&line);
                s.push('\n');
            }
        });
        Self {
            child,
            stdin,
            lines,
            stderr,
            next_id: 1,
            protocol,
            stdout: Vec::new(),
            transcript: Transcript::new(home),
        }
    }

    // ---- wire ------------------------------------------------------------------------------

    /// Writes one line exactly as given.
    fn write_line(&mut self, line: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            // A server that already exited makes this fail; the read that follows says why.
            let _ = writeln!(stdin, "{line}").and_then(|()| stdin.flush());
        }
    }

    /// The next message the server wrote, parsed. Anything that is not a JSON object is a
    /// failure of the test, with the line in the message.
    fn next_message(&mut self) -> Value {
        let line = match self.lines.recv_timeout(ANSWER) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => self.fail("gave no answer in 20 s"),
            Err(RecvTimeoutError::Disconnected) => self.fail("closed stdout before it answered"),
        };
        self.stdout.push(line.clone());
        match serde_json::from_str::<Value>(&line) {
            Ok(v) if v.is_object() => v,
            _ => self.fail(&format!("wrote a stdout line that is not a JSON-RPC message: {line:?}")),
        }
    }

    /// Kills the server and panics with what it had said and how it ended.
    fn fail(&mut self, what: &str) -> ! {
        self.stdin = None;
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        // The reader threads end when the pipes close; give them a moment to drain.
        thread::sleep(Duration::from_millis(100));
        let stderr = self.stderr.lock().unwrap().clone();
        panic!("aibus mcp {what} (exit status {status:?}); stderr:\n{stderr}");
    }

    /// Sends a request and waits for the answer with its id; notifications and requests the
    /// server makes in the meantime are kept in `stdout` and skipped.
    pub fn request(&mut self, method: &str, params: Value) -> Reply {
        let id = self.next_id;
        self.next_id += 1;
        let mut params = match params {
            Value::Null => json!({}),
            other => other,
        };
        if self.protocol == Protocol::V2026_07_28 {
            params["_meta"] = json!({
                META_VERSION: self.protocol.as_str(),
                META_CAPABILITIES: {},
                META_CLIENT_INFO: { "name": CLIENT_NAME, "version": "1.0.0" },
            });
        }
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_line(&request.to_string());
        let response = loop {
            let m = self.next_message();
            if m.get("id") == Some(&json!(id)) && m.get("method").is_none() {
                break m;
            }
        };
        if !matches!(method, "server/discover" | "initialize") {
            self.transcript.record(&request, &response);
        }
        reply(&response)
    }

    /// A notification: no id, no answer.
    pub fn notify(&mut self, method: &str, params: Value) {
        let n = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&n.to_string());
    }

    /// Sends `line` as it is (malformed JSON, a request with no `_meta`, ...) and returns the
    /// next message the server writes, whatever it is.
    pub fn raw(&mut self, line: &str) -> Value {
        self.write_line(line);
        self.next_message()
    }

    // ---- the protocol ----------------------------------------------------------------------

    /// `server/discover`, `2026-07-28` only. The result.
    pub fn discover(&mut self) -> Reply {
        assert_eq!(self.protocol, Protocol::V2026_07_28, "server/discover is a 2026-07-28 request");
        self.request("server/discover", json!({}))
    }

    /// `initialize` and `notifications/initialized`, `2025-11-25` only. The result of `initialize`.
    pub fn initialize_legacy(&mut self) -> Reply {
        assert_eq!(self.protocol, Protocol::V2025_11_25, "initialize is the 2025-11-25 handshake");
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": "1.0.0" },
            }),
        )?;
        self.notify("notifications/initialized", json!({}));
        Ok(result)
    }

    /// One page of `tools/list`.
    pub fn list_tools_page(&mut self, cursor: Option<&str>) -> Reply {
        let params = cursor.map_or_else(|| json!({}), |c| json!({ "cursor": c }));
        self.request("tools/list", params)
    }

    /// Every tool, following `nextCursor` to the end (and failing on a server that never ends).
    pub fn list_tools(&mut self) -> Vec<Value> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let page = self.list_tools_page(cursor.as_deref()).expect("tools/list failed");
            tools.extend(page["tools"].as_array().expect("tools is a list").iter().cloned());
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = Some(next.to_owned()),
                None => return tools,
            }
        }
        panic!("tools/list still had a nextCursor after 100 pages");
    }

    /// `tools/call`. The result is the `CallToolResult` object; a protocol error is `Err`.
    pub fn call(&mut self, name: &str, args: Value) -> Reply {
        self.request("tools/call", json!({ "name": name, "arguments": args }))
    }

    pub fn read_resource(&mut self, uri: &str) -> Reply {
        self.request("resources/read", json!({ "uri": uri }))
    }

    pub fn list_prompts(&mut self) -> Reply {
        self.request("prompts/list", json!({}))
    }

    pub fn get_prompt(&mut self, name: &str, args: Value) -> Reply {
        self.request("prompts/get", json!({ "name": name, "arguments": args }))
    }

    // ---- the end ---------------------------------------------------------------------------

    /// Closes stdin (what a client going away looks like), waits for the server to exit and
    /// collects the rest of stdout into `stdout`.
    pub fn finish(mut self) -> (Finished, Vec<String>) {
        self.stdin = None;
        let deadline = std::time::Instant::now() + ANSWER;
        let status = loop {
            if let Some(s) = self.child.try_wait().expect("wait for the server") {
                break s;
            }
            if std::time::Instant::now() > deadline {
                self.fail("did not exit 20 s after its stdin closed");
            }
            thread::sleep(Duration::from_millis(20));
        };
        thread::sleep(Duration::from_millis(100));
        while let Ok(line) = self.lines.try_recv() {
            self.stdout.push(line);
        }
        let stderr = self.stderr.lock().unwrap().clone();
        (Finished { status, stderr }, std::mem::take(&mut self.stdout))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reply(response: &Value) -> Reply {
    match response.get("error") {
        Some(e) => Err(RpcError {
            code: e["code"].as_i64().expect("an error has a code"),
            message: e["message"].as_str().unwrap_or_default().to_owned(),
        }),
        None => Ok(response.get("result").cloned().expect("a response has a result or an error")),
    }
}

/// Every line of `lines` is one JSON-RPC 2.0 message: an object with `"jsonrpc": "2.0"` and
/// either `method`, `result` or `error`. The names of the lines that are not.
pub fn lines_that_are_not_json_rpc(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| {
            serde_json::from_str::<Value>(l).ok().is_none_or(|v| {
                v["jsonrpc"] != "2.0"
                    || !["method", "result", "error"].iter().any(|k| v.get(*k).is_some())
            })
        })
        .cloned()
        .collect()
}
