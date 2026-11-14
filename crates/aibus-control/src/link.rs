//! The client's end, for `aibus mcp`: which sessions are live, which one to use, and one
//! connection to it that every call shares (one controlling client at a time, so the server keeps
//! one).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{check_chain, client_name, file, uid, valid_id, LINE_MAX};

/// How long a call waits for the session.
const WAIT: Duration = Duration::from_secs(35);

struct Conn {
    session: String,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

/// A way into the live sessions in `dir`, for one `aibus mcp` process.
pub struct Link {
    dir: PathBuf,
    /// `--session <id>`.
    wanted: Option<String>,
    conn: Mutex<Option<Conn>>,
    /// The user the socket, its token file and the process at the other end have to belong to.
    pub(crate) uid: u32,
}

/// The token of session `id`: a file of `owner` that nobody else can read.
fn token(dir: &Path, id: &str, owner: u32) -> io::Result<String> {
    let path = file(dir, id, "token");
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_file()
        || meta.uid() != owner
        || meta.permissions().mode() & 0o077 != 0
        || meta.len() > 256
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the session's token file is not private",
        ));
    }
    Ok(std::fs::read_to_string(path)?.trim().to_owned())
}

/// The user at the other end of `stream`, from the kernel's peer credentials. The standard
/// library does not offer them yet, so a tokio stream borrows a duplicate of the descriptor for
/// the question; the throwaway runtime is shut down in the background, which is allowed even
/// inside the server's own runtime.
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let asked = {
        let _in = rt.enter();
        let dup = stream.try_clone()?;
        dup.set_nonblocking(true)?;
        let uid = tokio::net::UnixStream::from_std(dup)
            .and_then(|s| s.peer_cred())
            .map(|c| c.uid());
        stream.set_nonblocking(false)?;
        uid
    };
    rt.shutdown_background();
    asked
}

fn read_reply(reader: &mut BufReader<UnixStream>) -> io::Result<Value> {
    let mut line = String::new();
    let n = reader
        .by_ref()
        .take(LINE_MAX as u64 + 1)
        .read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the session closed the connection",
        ));
    }
    serde_json::from_str(&line).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the session sent something that is not JSON",
        )
    })
}

impl Link {
    pub fn new(dir: PathBuf, wanted: Option<String>) -> Self {
        Self {
            dir,
            wanted,
            conn: Mutex::new(None),
            uid: uid(),
        }
    }

    /// The live sessions, oldest first: an info file whose socket answers.
    fn live(&self) -> Result<Vec<Value>, String> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        check_chain(&self.dir).map_err(|e| e.to_string())?;
        let entries = std::fs::read_dir(&self.dir).map_err(|e| e.to_string())?;
        let mut found: Vec<Value> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let id = path.file_stem()?.to_str()?.to_owned();
                if path.extension()? != "json" || !valid_id(&id) || UnixStream::connect(file(&self.dir, &id, "sock")).is_err() {
                    return None;
                }
                let text = std::fs::read_to_string(&path).ok().filter(|t| t.len() <= 64 * 1024)?;
                let info: Value = serde_json::from_str(&text).ok()?;
                let f = |k: &str| info.get(k).cloned().unwrap_or(Value::Null);
                Some(json!({"id": id, "cwd": f("cwd"), "lead": f("lead"), "model": f("model"), "started": f("started")}))
            })
            .collect();
        found.sort_by_key(|s| {
            (
                s["started"].as_str().unwrap_or("").to_owned(),
                s["id"].as_str().unwrap_or("").to_owned(),
            )
        });
        Ok(found)
    }

    /// Connects to session `id` and says hello. The token is read only once the socket file and
    /// the process behind it are known to be this user's, so it never goes to anybody else.
    fn connect(&self, id: &str, client: &str) -> Result<Conn, String> {
        check_chain(&self.dir).map_err(|e| e.to_string())?;
        let sock = file(&self.dir, id, "sock");
        let meta = std::fs::symlink_metadata(&sock)
            .map_err(|e| format!("cannot reach session {id}: {e}"))?;
        {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() || meta.uid() != self.uid {
                return Err(format!(
                    "the socket of session {id} is not one owned by this user"
                ));
            }
        }
        let writer =
            UnixStream::connect(&sock).map_err(|e| format!("cannot reach session {id}: {e}"))?;
        match peer_uid(&writer) {
            Ok(peer) if peer == self.uid => {}
            Ok(peer) => {
                return Err(format!(
                    "the process behind session {id} is uid {peer}, not this user"
                ))
            }
            Err(e) => return Err(format!("cannot tell who is behind session {id}: {e}")),
        }
        let token = token(&self.dir, id, self.uid)
            .map_err(|e| format!("cannot read the token of session {id}: {e}"))?;
        writer
            .set_read_timeout(Some(WAIT))
            .map_err(|e| e.to_string())?;
        writer
            .set_write_timeout(Some(WAIT))
            .map_err(|e| e.to_string())?;
        let reader = BufReader::new(writer.try_clone().map_err(|e| e.to_string())?);
        let mut conn = Conn {
            session: id.to_owned(),
            reader,
            writer,
            next: 0,
        };
        let hello = json!({"hello": 1, "token": token, "client": client_name(client)});
        writeln!(conn.writer, "{hello}").map_err(|e| format!("session {id}: {e}"))?;
        let answer = read_reply(&mut conn.reader).map_err(|e| format!("session {id}: {e}"))?;
        match answer.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(conn),
            _ => Err(answer
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the session refused the connection")
                .to_owned()),
        }
    }
}

impl aibus_mcp::SessionLink for Link {
    fn list(&self) -> Result<Value, String> {
        Ok(json!({ "sessions": self.live()? }))
    }

    fn target(&self) -> Result<String, String> {
        let live = self.live()?;
        let ids: Vec<String> = live
            .iter()
            .map(|s| {
                format!(
                    "{} ({}, started {})",
                    s["id"].as_str().unwrap_or(""),
                    s["cwd"].as_str().unwrap_or("?"),
                    s["started"].as_str().unwrap_or("?")
                )
            })
            .collect();
        if let Some(w) = &self.wanted {
            return match live.iter().any(|s| s["id"] == w.as_str()) {
                true => Ok(w.clone()),
                false => Err(format!(
                    "no live session {}; live: {}",
                    client_name(w),
                    if ids.is_empty() {
                        "none".into()
                    } else {
                        ids.join(", ")
                    }
                )),
            };
        }
        match &live[..] {
            [] => Err("no live aibus session to connect to: start aibus in a terminal first".into()),
            [one] => Ok(one["id"].as_str().unwrap_or("").to_owned()),
            _ => Err(format!(
                "{} live sessions, so the server will not guess: start it with --session <id>, one of {}",
                live.len(),
                ids.join(", ")
            )),
        }
    }

    fn call(&self, client: &str, request: Value) -> Result<Value, String> {
        let target = self.target()?;
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|c| c.session != target) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(self.connect(&target, client)?);
        }
        let conn = guard.as_mut().expect("connected above");
        conn.next += 1;
        let id = conn.next;
        let mut request = request;
        request["id"] = json!(id);
        let answer = writeln!(conn.writer, "{request}").and_then(|()| read_reply(&mut conn.reader));
        let answer = match answer {
            Ok(a) if a["id"] == json!(id) => a,
            Ok(_) => {
                *guard = None;
                return Err(
                    "the session answered out of turn; reconnecting on the next call".into(),
                );
            }
            Err(e) => {
                *guard = None;
                return Err(format!("the session went away: {e}"));
            }
        };
        match answer.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(answer.get("result").cloned().unwrap_or(Value::Null)),
            _ => Err(answer
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the session refused")
                .to_owned()),
        }
    }
}
