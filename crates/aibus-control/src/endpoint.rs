//! The session's end: the socket, its token and info files, and what a connection has to pass.

use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibus_core::remote::{Ask, Remote};
use aibus_core::Redactor;
use aibus_mcp::RateLimiter;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::{
    check_private, client_name, file, private_dir, uid, valid_id, CHANGES_PER_MINUTE,
    LINES_PER_MINUTE, LINE_MAX,
};

/// How long a new connection has to say `hello`.
const HELLO_WITHIN: Duration = Duration::from_secs(5);
/// How long the session has to answer a call.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// The session's end of the control socket. Dropping it closes the socket and removes its three
/// files.
pub struct Endpoint {
    dir: PathBuf,
    id: String,
    token: String,
    /// The user a peer has to be.
    pub(crate) uid: u32,
    listener: Option<std::os::unix::net::UnixListener>,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// Removes the files of sessions that are gone: a socket nobody listens on (the session died
/// without cleaning up), and a token or info file without a socket.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|e| e.to_str()),
        ) else {
            continue;
        };
        if !valid_id(stem) {
            continue;
        }
        let sock = file(dir, stem, "sock");
        let gone = match ext {
            "sock" => StdStream::connect(&sock).is_err_and(|e| {
                matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                )
            }),
            "token" | "json" => !sock.exists(),
            _ => false,
        };
        if gone {
            for ext in ["sock", "token", "json"] {
                let _ = std::fs::remove_file(file(dir, stem, ext));
            }
        }
    }
}

/// A new file, 0600 from the moment it exists.
fn write_new(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

impl Endpoint {
    /// Opens the socket of session `id` in `dir` (a private directory, see `private_dir`) and
    /// writes its token and `info` next to it. The socket is not served until `serve`. A live
    /// session with the same id is `AddrInUse`; a dead one's files are replaced.
    pub fn open(dir: &Path, id: &str, info: &Value) -> io::Result<Self> {
        if !valid_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a session id is letters and digits",
            ));
        }
        private_dir(dir)?;
        sweep(dir);
        let sock = file(dir, id, "sock");
        if sock.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("session {id} is already live"),
            ));
        }
        let listener = std::os::unix::net::UnixListener::bind(&sock)?;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let token: String = rand::random::<[u8; 32]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let ep = Self {
            dir: dir.to_owned(),
            id: id.to_owned(),
            token,
            uid: uid(),
            listener: Some(listener),
            task: None,
        };
        // A stale token or info file of a dead session with this id went in the sweep.
        write_new(&ep.token_file(), &ep.token)?;
        write_new(&ep.info_file(), &info.to_string())?;
        Ok(ep)
    }

    pub fn socket(&self) -> PathBuf {
        file(&self.dir, &self.id, "sock")
    }

    pub fn token_file(&self) -> PathBuf {
        file(&self.dir, &self.id, "token")
    }

    pub fn info_file(&self) -> PathBuf {
        file(&self.dir, &self.id, "json")
    }

    /// Starts taking connections on the current tokio runtime. What a client asks goes to `tx`,
    /// and every answer goes through `redactor` before it is written back.
    pub fn serve(&mut self, tx: mpsc::Sender<Remote>, redactor: Redactor) -> io::Result<()> {
        let std = self
            .listener
            .take()
            .ok_or_else(|| io::Error::other("the endpoint is already served"))?;
        let listener = UnixListener::from_std(std)?;
        let shared = Arc::new(Shared {
            id: self.id.clone(),
            token: self.token.clone(),
            uid: self.uid,
            busy: Mutex::new(None),
            tx,
            redactor,
        });
        self.task = Some(tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(connection(shared.clone(), stream));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }));
        Ok(())
    }
}

/// The token stays out of any debug print.
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("dir", &self.dir)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // Only files in a directory that is still ours; a removal that fails leaves a stale file
        // the next session sweeps.
        if check_private(&self.dir).is_ok() {
            for path in [self.socket(), self.token_file(), self.info_file()] {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

struct Shared {
    id: String,
    token: String,
    uid: u32,
    /// The client that controls the session now.
    busy: Mutex<Option<String>>,
    tx: mpsc::Sender<Remote>,
    redactor: Redactor,
}

/// A line past `LINE_MAX`.
struct TooLong;

/// The next line, without its newline; `None` at the end. A line longer than `LINE_MAX` is never
/// held whole.
async fn read_line(r: &mut BufReader<OwnedReadHalf>) -> Result<Option<String>, Option<TooLong>> {
    let mut buf = Vec::new();
    loop {
        let avail = r.fill_buf().await.map_err(|_| None)?;
        if avail.is_empty() {
            return Ok((!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned()));
        }
        let (take, end) = match avail.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (avail.len(), false),
        };
        buf.extend_from_slice(&avail[..take]);
        r.consume(take + usize::from(end));
        if buf.len() > LINE_MAX {
            return Err(Some(TooLong));
        }
        if end {
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
    }
}

async fn say(w: &mut OwnedWriteHalf, v: &Value) -> io::Result<()> {
    w.write_all(format!("{v}\n").as_bytes()).await
}

/// One request line as an `Ask`, and whether it changes the session.
fn ask(v: &Value) -> Result<(Ask, bool), String> {
    let field = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing string field {k:?}"))
    };
    let op = field("op")?;
    Ok(match op.as_str() {
        "status" => (Ask::Status, false),
        "transcript" => (
            Ask::Transcript {
                run: v.get("run").and_then(Value::as_str).map(str::to_owned),
            },
            false,
        ),
        "send" => (
            Ask::Send {
                text: field("text")?,
            },
            true,
        ),
        "command" => (
            Ask::Command {
                line: field("line")?,
            },
            true,
        ),
        "cancel" => (Ask::Cancel { run: field("run")? }, true),
        "permission" => (
            Ask::Permission {
                id: field("prompt")?,
                decision: field("decision")?,
            },
            true,
        ),
        _ => {
            return Err(
                "unknown op: want status, transcript, send, command, cancel or permission".into(),
            )
        }
    })
}

async fn connection(shared: Arc<Shared>, stream: UnixStream) {
    // The kernel says who is at the other end; anybody but this user is turned away first.
    let mine = stream.peer_cred().is_ok_and(|c| c.uid() == shared.uid);
    let (r, mut w) = stream.into_split();
    if !mine {
        let _ = say(&mut w, &json!({"ok": false, "error": "refused: the peer is not the user this session runs as"})).await;
        return;
    }
    let mut r = BufReader::new(r);
    let Ok(Ok(Some(hello))) = tokio::time::timeout(HELLO_WITHIN, read_line(&mut r)).await else {
        let _ = say(&mut w, &json!({"ok": false, "error": "refused: no hello"})).await;
        return;
    };
    let hello: Value = serde_json::from_str(&hello).unwrap_or(Value::Null);
    let token = hello.get("token").and_then(Value::as_str).unwrap_or("");
    let good = hello.get("hello").and_then(Value::as_u64) == Some(1)
        && bool::from(token.as_bytes().ct_eq(shared.token.as_bytes()));
    if !good {
        let _ = say(&mut w, &json!({"ok": false, "error": "refused: a hello with this session's token comes first"})).await;
        return;
    }
    let client = client_name(hello.get("client").and_then(Value::as_str).unwrap_or(""));
    let taken = {
        let mut busy = shared.busy.lock().unwrap_or_else(|e| e.into_inner());
        match busy.as_ref() {
            Some(other) => Some(format!(
                "refused: another client ({other}) controls this session, one at a time"
            )),
            None => {
                *busy = Some(client.clone());
                None
            }
        }
    };
    if let Some(error) = taken {
        let _ = say(&mut w, &json!({"ok": false, "error": error})).await;
        return;
    }
    if shared
        .tx
        .send(Remote::Connected {
            client: client.clone(),
        })
        .await
        .is_ok()
        && say(&mut w, &json!({"ok": true, "session": shared.id}))
            .await
            .is_ok()
    {
        serve_lines(&shared, &client, &mut r, &mut w).await;
    }
    *shared.busy.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let _ = shared.tx.send(Remote::Disconnected { client }).await;
}

/// The lines of a client past its hello, until it goes or breaks the size cap.
async fn serve_lines(
    shared: &Shared,
    client: &str,
    r: &mut BufReader<OwnedReadHalf>,
    w: &mut OwnedWriteHalf,
) {
    let mut lines = RateLimiter::new(LINES_PER_MINUTE, Instant::now());
    let mut changes = RateLimiter::new(CHANGES_PER_MINUTE, Instant::now());
    loop {
        let line = match read_line(r).await {
            Ok(Some(l)) => l,
            Ok(None) | Err(None) => return,
            Err(Some(TooLong)) => {
                let error = format!("a line is at most {LINE_MAX} bytes; closing");
                let _ = say(w, &json!({"id": null, "ok": false, "error": error})).await;
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let id = v.get("id").cloned().unwrap_or(Value::Null);
        let answer = match (lines.take(Instant::now()), ask(&v)) {
            (Err(secs), _) => Err(format!(
                "rate limit: at most {LINES_PER_MINUTE} lines a minute, try again in {secs} s"
            )),
            (_, Err(why)) => Err(why),
            (Ok(()), Ok((_, true))) if changes.take(Instant::now()).is_err() => Err(format!(
                "rate limit: at most {CHANGES_PER_MINUTE} changes to the session a minute"
            )),
            (Ok(()), Ok((ask, _))) => {
                let (reply, answered) = oneshot::channel();
                if shared
                    .tx
                    .send(Remote::Call {
                        client: client.to_owned(),
                        ask,
                        reply,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                match tokio::time::timeout(ANSWER_WITHIN, answered).await {
                    Ok(Ok(answer)) => answer,
                    _ => Err("the session did not answer".into()),
                }
            }
        };
        let out = match answer {
            Ok(mut result) => {
                shared.redactor.redact_json(&mut result);
                json!({"id": id, "ok": true, "result": result})
            }
            Err(why) => json!({"id": id, "ok": false, "error": shared.redactor.redact(&why)}),
        };
        if say(w, &out).await.is_err() {
            return;
        }
    }
}
