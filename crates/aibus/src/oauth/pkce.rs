//! RFC 7636 (S256) with a loopback redirect (RFC 8252). The listener is one hand-written
//! HTTP/1.1 reader on 127.0.0.1: exact path, `Host` must be loopback, `state` must match when
//! the server echoes it. A request that fails a check is answered and ignored, the first one
//! that passes ends the login, and the socket closes with it.

use std::process::{Command, Stdio};
use std::time::Duration;

use aibus_core::OAuthTokens;
use aibus_provider_http::reqwest::Url;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::{form, Login, Outcome, StateMode};

const PATH: &str = "/callback";
const WAIT: Duration = Duration::from_secs(5 * 60);
/// A request head is read this far, and for this long.
const HEAD_BYTES: usize = 8 * 1024;
const HEAD_WAIT: Duration = Duration::from_secs(5);

const PAGE: &str = "<!doctype html><meta charset=utf-8><title>aibus</title>\
<p>You are logged in. You can close this tab and return to the terminal.";

fn random_b64() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

impl Login {
    pub(super) async fn pkce(
        &self,
        notify: &(dyn for<'n> Fn(&'n str) + Send + Sync + '_),
        cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String> {
        let d = &self.def;
        let verifier = random_b64();
        let state = random_b64();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

        let listener = match d.callback_port {
            Some(p) => match TcpListener::bind(("127.0.0.1", p)).await {
                Ok(l) => l,
                Err(e) if d.port_pinned => {
                    return Err(format!("port {p} is in use ({e}); this login only accepts that port, free it and run the command again"))
                }
                Err(_) => bind_any().await?,
            },
            None => bind_any().await?,
        };
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let redirect = format!("http://127.0.0.1:{port}{PATH}");

        let mut url = Url::parse(&d.authorize_url).map_err(|e| format!("bad authorize URL: {e}"))?;
        {
            let mut q = url.query_pairs_mut();
            if let Some(id) = &d.client_id {
                q.append_pair("response_type", "code").append_pair("client_id", id);
            }
            q.append_pair(d.redirect_param, &redirect);
            if !d.scopes.is_empty() {
                q.append_pair("scope", &d.scopes.join(" "));
            }
            if d.state == StateMode::Echoed {
                q.append_pair("state", &state);
            }
            q.append_pair("code_challenge", &challenge).append_pair("code_challenge_method", "S256");
        }
        notify(&format!("open this address to log in:\n  {url}"));
        if self.open_browser {
            open(url.as_str());
        }
        let paste = self.paste.then(|| {
            notify("or paste the address your browser ended up on and press enter:");
            paste_line()
        });

        let want_state = (d.state == StateMode::Echoed).then_some(state.as_str());
        let code = tokio::time::timeout(WAIT, wait_for_code(listener, want_state, paste, cancel))
            .await
            .map_err(|_| "timed out after 5 minutes waiting for the browser; run the command again".to_owned())??;

        let (body, json) = if d.result == Outcome::ApiKey {
            let b = serde_json::json!({ "code": code, "code_verifier": verifier, "code_challenge_method": "S256" });
            (b.to_string(), true)
        } else {
            let mut pairs = vec![
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect.as_str()),
                ("code_verifier", verifier.as_str()),
            ];
            if let Some(id) = &d.client_id {
                pairs.push(("client_id", id));
            }
            (form(&pairs), false)
        };
        let (status, reply) = self.post(&d.token_url, body, json, cancel).await?;
        self.tokens(status, &reply)
    }
}

async fn bind_any() -> Result<TcpListener, String> {
    TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| format!("could not listen on 127.0.0.1: {e}"))
}

/// The platform opener, with the URL as one argv element and no shell. A failure only means
/// the user opens the printed address by hand.
pub(crate) fn open(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        Command::new("xdg-open")
    };
    let _ = cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// One line from stdin on its own thread (a blocked read cannot be cancelled).
fn paste_line() -> tokio::sync::oneshot::Receiver<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() {
            let _ = tx.send(line);
        }
    });
    rx
}

/// Waits for the first request that passes every check, or a pasted address that does.
async fn wait_for_code(
    listener: TcpListener,
    state: Option<&str>,
    mut paste: Option<tokio::sync::oneshot::Receiver<String>>,
    cancel: &CancellationToken,
) -> Result<String, String> {
    loop {
        let pasted = async {
            match paste.as_mut() {
                Some(rx) => rx.await.ok(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = cancel.cancelled() => return Err("cancelled".into()),
            Some(line) = pasted => {
                paste = None;
                let query = Url::parse(line.trim()).ok().filter(|u| u.path() == PATH).map(|u| u.query().unwrap_or_default().to_owned());
                match query.map(|q| check(&q, state)) {
                    Some(Verdict::Code(code)) => return Ok(code),
                    Some(Verdict::Refused(why)) => return Err(why),
                    _ => {}
                }
            }
            accepted = listener.accept() => {
                let Ok((mut sock, _)) = accepted else { continue };
                let reply = tokio::time::timeout(HEAD_WAIT, read_head(&mut sock)).await;
                let Ok(Some(head)) = reply else {
                    respond(&mut sock, "400 Bad Request", "bad request").await;
                    continue;
                };
                match judge(&head, state) {
                    Verdict::Code(code) => {
                        respond(&mut sock, "200 OK", PAGE).await;
                        return Ok(code);
                    }
                    Verdict::Refused(why) => {
                        respond(&mut sock, "200 OK", "The login was refused. You can close this tab.").await;
                        return Err(why);
                    }
                    Verdict::Ignore(status, why) => respond(&mut sock, status, why).await,
                }
            }
        }
    }
}

async fn read_head(sock: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > HEAD_BYTES {
            return None;
        }
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8(buf).ok()
}

/// What a request to the callback address amounts to.
enum Verdict {
    Code(String),
    /// The server sent `error=...`: the login is over.
    Refused(String),
    /// Answer with this status and text, and keep waiting.
    Ignore(&'static str, &'static str),
}

fn judge(head: &str, state: Option<&str>) -> Verdict {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (first.next().unwrap_or_default(), first.next().unwrap_or_default());
    let host = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.trim())
        .unwrap_or_default();
    // `[::1]:port`, `127.0.0.1:port`, `localhost:port`, with or without the port.
    let name = match host.find(']') {
        Some(i) => &host[..=i],
        None => host.split(':').next().unwrap_or_default(),
    };
    if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        return Verdict::Ignore("400 Bad Request", "unexpected Host");
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method != "GET" || path != PATH {
        return Verdict::Ignore("404 Not Found", "not found");
    }
    check(query, state)
}

/// The `code` of a callback query, after the `state` and `error` checks.
fn check(query: &str, state: Option<&str>) -> Verdict {
    let Ok(q) = Url::parse(&format!("http://x/?{query}")) else {
        return Verdict::Ignore("400 Bad Request", "bad query");
    };
    let get = |k: &str| q.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    if state.is_some_and(|want| get("state").as_deref() != Some(want)) {
        return Verdict::Ignore("400 Bad Request", "state does not match");
    }
    if let Some(e) = get("error") {
        return Verdict::Refused(format!("the provider refused the login: {e}"));
    }
    match get("code").filter(|c| !c.is_empty()) {
        Some(c) => Verdict::Code(c),
        None => Verdict::Ignore("400 Bad Request", "no code"),
    }
}

async fn respond(sock: &mut TcpStream, status: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(msg.as_bytes()).await;
    let _ = sock.shutdown().await;
}
