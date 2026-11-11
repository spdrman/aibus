//! The scripted fake server and the fixtures it plays (issue #109).
//!
//! A fixture says what one answer looks like: status, headers, and either a body sent at once
//! or a list of steps (SSE frames with an optional delay before each), then how the connection
//! ends. Two file forms load into the same value:
//!
//! * `<name>.http`, the recorded form: `HTTP <status>`, header lines, a blank line, the body.
//! * `<name>.json`, the scripted form:
//!   `{"status": 200, "headers": {..}, "delay_ms": 0, "body": "raw text"}` or, instead of
//!   `body`, `"steps": [{"event": "x", "data": {..}, "delay_ms": 20}, {"raw": "data: {bro", "delay_ms": 0}]`,
//!   and `"end": "close" | "drop" | "hang"` (default `close`).
//!
//! The server speaks plain HTTP/1.1 on a loopback port, one answer per connection, chunked, so
//! `drop` is a real broken stream (no final chunk) and `hang` keeps the connection open until
//! the client goes away. Nothing here leaves the machine.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub enum Body {
    /// Sent in one chunk.
    Raw(String),
    /// Sent one chunk per step, each after its delay.
    Steps(Vec<(String, Duration)>),
    /// Sent in one chunk, and not necessarily text: Bedrock's binary event stream.
    Bytes(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// A proper final chunk, then close.
    Close,
    /// Close with no final chunk: the client sees a broken body.
    Drop,
    /// Keep the connection open and silent until the client hangs up.
    Hang,
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Wait this long before the status line.
    pub delay: Duration,
    pub body: Body,
    pub end: End,
}

impl Fixture {
    pub fn http(status: u16, headers: &[(&str, &str)], body: &str) -> Self {
        Self {
            status,
            headers: headers.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            delay: Duration::ZERO,
            body: Body::Raw(body.to_owned()),
            end: End::Close,
        }
    }

    /// Reads either file form, by extension.
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        match path.extension().and_then(|e| e.to_str()) {
            Some("http") => Self::parse_http(&text),
            Some("json") => Self::parse_scripted(&text),
            other => panic!("{}: fixtures are .http or .json, not {other:?}", path.display()),
        }
    }

    pub fn parse_http(text: &str) -> Self {
        let (head, body) = text.split_once("\n\n").expect("a blank line after the headers");
        let mut lines = head.lines();
        let status = lines.next().and_then(|l| l.strip_prefix("HTTP ")).expect("HTTP <status>").parse().expect("a status number");
        let mut f = Self::http(status, &[], body);
        for l in lines {
            let (k, v) = l.split_once(": ").expect("header line");
            f.headers.push((k.to_owned(), v.to_owned()));
        }
        f
    }

    pub fn parse_scripted(text: &str) -> Self {
        let v: Value = serde_json::from_str(text).expect("scripted fixture is JSON");
        let ms = |v: &Value| Duration::from_millis(v["delay_ms"].as_u64().unwrap_or(0));
        let mut f = Self::http(v["status"].as_u64().expect("status") as u16, &[], "");
        f.delay = ms(&v);
        for (k, val) in v["headers"].as_object().into_iter().flatten() {
            f.headers.push((k.clone(), val.as_str().expect("header value is a string").to_owned()));
        }
        f.body = match (v["body"].as_str(), v["steps"].as_array()) {
            (Some(b), None) => Body::Raw(b.to_owned()),
            (None, Some(steps)) => Body::Steps(
                steps
                    .iter()
                    .map(|s| {
                        let frame = match s["raw"].as_str() {
                            Some(raw) => raw.to_owned(),
                            None => {
                                let data = match &s["data"] {
                                    Value::String(d) => d.clone(),
                                    other => other.to_string(),
                                };
                                match s["event"].as_str() {
                                    Some(e) => format!("event: {e}\ndata: {data}\n\n"),
                                    None => format!("data: {data}\n\n"),
                                }
                            }
                        };
                        (frame, ms(s))
                    })
                    .collect(),
            ),
            _ => panic!("a scripted fixture has `body` or `steps`, not both and not neither"),
        };
        f.end = match v["end"].as_str().unwrap_or("close") {
            "close" => End::Close,
            "drop" => End::Drop,
            "hang" => End::Hang,
            other => panic!("unknown end {other:?}"),
        };
        f
    }

    /// The SSE frames of a raw body, cut at blank lines.
    fn frames(&self) -> Vec<String> {
        match &self.body {
            Body::Raw(b) => b.split_inclusive("\n\n").map(str::to_owned).collect(),
            Body::Steps(s) => s.iter().map(|(t, _)| t.clone()).collect(),
            Body::Bytes(_) => Vec::new(),
        }
    }

    /// Keeps the first `n` frames, then goes silent with the connection open. Turns a recorded
    /// stream into "the server is still sending" for the cancel check.
    pub fn hang_after(mut self, n: usize) -> Self {
        self.body = Body::Steps(self.frames().into_iter().take(n).map(|f| (f, Duration::ZERO)).collect());
        self.end = End::Hang;
        self
    }

    /// The raw text of the body, for checks that read it (the redactor).
    pub fn body_text(&self) -> String {
        match &self.body {
            Body::Raw(b) => b.clone(),
            Body::Steps(s) => s.iter().map(|(t, _)| t.as_str()).collect(),
            Body::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }
}

/// One request as the server saw it. Header names are lower-case.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct State {
    queue: Mutex<VecDeque<Fixture>>,
    seen: Mutex<Vec<Seen>>,
    hanging: AtomicUsize,
}

pub struct FakeServer {
    addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl FakeServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let task = tokio::spawn({
            let state = state.clone();
            async move {
                while let Ok((sock, _)) = listener.accept().await {
                    tokio::spawn(serve(sock, state.clone()));
                }
            }
        });
        Self { addr, state, task }
    }

    /// The `base_url` to build the provider with.
    pub fn uri(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The next request gets this answer; answers are used in the order they were queued.
    pub fn script(&self, f: Fixture) {
        self.state.queue.lock().unwrap().push_back(f);
    }

    /// Forgets answers nobody asked for.
    pub fn clear_script(&self) {
        self.state.queue.lock().unwrap().clear();
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.state.seen.lock().unwrap().clone()
    }

    /// How many answers have reached their hang so far.
    pub fn hangs(&self) -> usize {
        self.state.hanging.load(Ordering::SeqCst)
    }

    /// Resolves once more than `seen` answers (a `hangs()` read taken before the call under
    /// test) have sent everything they had and are holding the connection open. The guard
    /// timeout only turns a hang into a failure.
    pub async fn wait_hanging(&self, seen: usize) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.hangs() <= seen {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the server reached its hang");
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_request(sock: &mut TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Seen { method, target, headers, body })
}

async fn write_chunk(sock: &mut TcpStream, text: &str) -> std::io::Result<()> {
    write_bytes(sock, text.as_bytes()).await
}

async fn write_bytes(sock: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    sock.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await?;
    sock.write_all(bytes).await?;
    sock.write_all(b"\r\n").await?;
    sock.flush().await
}

async fn serve(mut sock: TcpStream, state: Arc<State>) {
    let Some(seen) = read_request(&mut sock).await else { return };
    state.seen.lock().unwrap().push(seen);
    let fx = state.queue.lock().unwrap().pop_front().unwrap_or_else(|| Fixture::http(599, &[], "unscripted request"));
    let _ = play(&mut sock, fx, &state).await;
}

async fn play(sock: &mut TcpStream, fx: Fixture, state: &State) -> std::io::Result<()> {
    tokio::time::sleep(fx.delay).await;
    let mut head = format!("HTTP/1.1 {} Scripted\r\n", fx.status);
    for (k, v) in &fx.headers {
        let k = k.to_ascii_lowercase();
        if !matches!(k.as_str(), "content-length" | "transfer-encoding" | "connection") {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    head.push_str("transfer-encoding: chunked\r\nconnection: close\r\n\r\n");
    sock.write_all(head.as_bytes()).await?;
    match &fx.body {
        Body::Raw(b) => write_chunk(sock, b).await?,
        Body::Bytes(b) => write_bytes(sock, b).await?,
        Body::Steps(steps) => {
            for (text, delay) in steps {
                tokio::time::sleep(*delay).await;
                write_chunk(sock, text).await?;
            }
        }
    }
    sock.flush().await?;
    match fx.end {
        End::Close => {
            sock.write_all(b"0\r\n\r\n").await?;
            sock.shutdown().await
        }
        End::Drop => Ok(()),
        End::Hang => {
            state.hanging.fetch_add(1, Ordering::SeqCst);
            let mut sink = [0u8; 64];
            while sock.read(&mut sink).await? > 0 {}
            Ok(())
        }
    }
}
