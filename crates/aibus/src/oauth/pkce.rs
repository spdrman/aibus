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

/// The page the browser lands on after the redirect: connected, or refused. One self-contained
/// document (inline CSS, inline SVG, no script, no request out); light and dark come from
/// `prefers-color-scheme`. Only the provider name is dynamic, and it is escaped.
pub(super) fn page(connected: bool, provider: &str) -> String {
    const CSS: &str = "\
:root{--ground:#F4F3EE;--card:#FFFFFF;--border:#8C8A82;--text:#1B1B1A;--muted:#5A5955;--rule:#D9D7CF;\
--chip:#EFEDE6;--chip-text:#3A3935;--ok:#1F6B45;--ok-bg:#E4F2EA;--no:#A12B1E;--no-bg:#FBE9E6}\
@media (prefers-color-scheme: dark){:root{--ground:#121211;--card:#1C1C1A;--border:#6B6A64;--text:#F2F1EC;\
--muted:#A9A8A1;--rule:#3A3A36;--chip:#2A2A27;--chip-text:#D9D8D1;--ok:#6FD69B;--ok-bg:#12301F;--no:#FF9A8C;--no-bg:#3A1713}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--ground);color:var(--text);font-family:ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif}\
main{min-height:100vh;display:flex;align-items:center;justify-content:center;padding:16px}\
.card{width:100%;max-width:520px;background:var(--card);border:1px solid var(--border);border-radius:16px;\
padding:40px 44px 36px;display:flex;flex-direction:column;gap:24px}\
.top{display:flex;align-items:center;justify-content:space-between}\
.mark{font-family:ui-monospace,'SF Mono',Menlo,monospace;font-size:15px;font-weight:600;letter-spacing:.02em}\
.top span:last-child{font-size:13px;color:var(--muted)}\
.head{display:flex;flex-direction:column;gap:16px}\
.icon{width:56px;height:56px;border-radius:28px;display:flex;align-items:center;justify-content:center;\
background:var(--bg);color:var(--fg)}\
.ok{--fg:var(--ok);--bg:var(--ok-bg)}.no{--fg:var(--no);--bg:var(--no-bg)}\
svg{fill:none;stroke:currentColor;stroke-width:2.4;stroke-linecap:round;stroke-linejoin:round}\
h1{margin:0;font-size:28px;line-height:1.2;font-weight:650;letter-spacing:-.01em}\
p{margin:0;font-size:16px;line-height:1.55;color:var(--muted)}\
ol{margin:0;padding:0;list-style:none;display:flex;flex-direction:column;gap:12px;font-size:15px;line-height:1.4}\
li{display:flex;align-items:center;gap:12px}\
.n{flex:none;width:24px;height:24px;border-radius:12px;background:var(--chip);color:var(--chip-text);\
font-size:13px;font-weight:600;display:flex;align-items:center;justify-content:center}\
code{font-family:ui-monospace,'SF Mono',Menlo,monospace;font-size:14px;background:var(--chip);color:var(--chip-text);\
padding:2px 6px;border-radius:6px}\
.note{padding-top:20px;border-top:1px solid var(--rule);font-size:13px;line-height:1.5}\
@media (max-width:520px){.card{padding:28px 24px 26px;gap:20px}h1{font-size:24px}}";
    let name = provider
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let (title, class, glyph, headline, body, step, note) = if connected {
        (
            "aibus: connected",
            "ok",
            "M5 12.5l4.5 4.5L19 7.5",
            format!("You're connected to {name}"),
            "aibus has what it needs, so there is nothing more to do in the browser.".to_owned(),
            "Go back to the terminal. aibus carries on by itself.".to_owned(),
            "Your credential is saved in the system keychain on this machine, never in a config file or in this tab.",
        )
    } else {
        (
            "aibus: login refused",
            "no",
            "M7 7l10 10M17 7L7 17",
            "The login was refused".to_owned(),
            format!("{name} did not give aibus access, so you are not connected."),
            "To try again, type <code>/login</code> in aibus.".to_owned(),
            "Nothing was saved. aibus is still waiting in the terminal and gives up after five minutes.",
        )
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"color-scheme\" content=\"light dark\"><title>{title}</title><style>{CSS}</style></head>\
<body><main><div class=\"card\"><div class=\"top\"><span class=\"mark\">aibus</span><span>Login</span></div>\
<div class=\"head\"><div class=\"icon {class}\"><svg width=\"28\" height=\"28\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{glyph}\"/></svg></div>\
<h1>{headline}</h1><p>{body}</p></div>\
<ol><li><span class=\"n\">1</span>Close this tab.</li><li><span class=\"n\">2</span><span>{step}</span></li></ol>\
<p class=\"note\">{note}</p></div></main></body></html>"
    )
}

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
        self.hold("oauth-flow:verifier", &verifier);
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
        let code = tokio::time::timeout(WAIT, wait_for_code(listener, want_state, d.provider_name, paste, cancel))
            .await
            .map_err(|_| "timed out after 5 minutes waiting for the browser; run the command again".to_owned())??;

        self.hold("oauth-flow:code", &code);
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
    name: &str,
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
                        respond(&mut sock, "200 OK", &page(true, name)).await;
                        return Ok(code);
                    }
                    Verdict::Refused(why) => {
                        respond(&mut sock, "200 OK", &page(false, name)).await;
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
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(msg.as_bytes()).await;
    let _ = sock.shutdown().await;
}
