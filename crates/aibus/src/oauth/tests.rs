//! #115: the two login flows against a scripted authorization server and a browser played by
//! the test. No network beyond 127.0.0.1, no real browser. Time is paused where a test measures
//! a wait (the five minute timeout, the device interval), so those cost nothing.
//!
//! A PKCE test where the browser completes the login runs on the real clock instead (#217).
//! The flow keeps the five minute timeout and the 5 s head timeout armed while the browser's
//! request is out, and with a paused clock tokio jumps to the next timer whenever the runtime
//! waits on a real socket: the five minute timeout fired mid-request, dropped the listener and
//! the browser got a reset. None of those tests measure time, and on the real clock the login
//! is over in milliseconds, long before any of the flow's or the client's timers.

use std::sync::Arc;
use std::time::Duration;

use aibus_core::{OAuthFlow, Redactor};
use aibus_provider_http::reqwest::{Client, Url};
use aibus_testkit::oauth::FakeAuthServer;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{Def, Kind, Login, Outcome, StateMode};

const KEY: &str = "sk-or-v1-test-key-0123456789abcdef";

fn pkce_def(auth: &FakeAuthServer, state: StateMode) -> Def {
    Def {
        kind: Kind::PkceLoopback,
        authorize_url: auth.url("/auth"),
        token_url: auth.url("/api/v1/auth/keys"),
        device_url: None,
        client_id: None,
        scopes: vec![],
        callback_port: None,
        port_pinned: false,
        redirect_param: "callback_url",
        state,
        result: Outcome::ApiKey,
    }
}

/// Runs the login with a task that plays the browser: `visit` gets the authorize URL and the
/// callback address it names and returns whatever it wants to send there.
async fn login_with<F, Fut>(
    login: Login,
    visit: F,
) -> (Result<aibus_core::OAuthTokens, String>, Url)
where
    F: FnOnce(Url, String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let cancel = CancellationToken::new();
    let notify = move |line: &str| {
        let _ = tx.send(line.to_owned());
    };
    let flow = async { login.authorize(&notify, &cancel).await };
    let browser = async {
        let url = loop {
            let line = rx.recv().await.expect("a line with the URL");
            if let Some(u) = line.split_whitespace().find(|w| w.starts_with("http")) {
                break Url::parse(u).unwrap();
            }
        };
        let param = url.query_pairs().find(|(k, _)| k == "callback_url" || k == "redirect_uri");
        let cb = param.map(|(_, v)| v.into_owned()).expect("a callback in the URL");
        visit(url.clone(), cb).await;
        url
    };
    let (out, url) = tokio::join!(flow, browser);
    (out, url)
}

fn http() -> Client {
    Client::new()
}

#[tokio::test]
async fn the_openrouter_style_flow_trades_the_code_and_the_verifier_for_a_key() {
    let auth = FakeAuthServer::start().await;
    auth.token_status(200, json!({ "key": KEY })).await;
    let login = Login::new(pkce_def(&auth, StateMode::None));
    let (out, url) = login_with(login, |_, cb| async move {
        let r = http().get(format!("{cb}?code=the-code")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.text().await.unwrap().contains("return to"));
    })
    .await;
    let tokens = out.unwrap();
    // The key comes back as the access token, with nothing to refresh.
    assert_eq!((tokens.access.expose().as_str(), tokens.refresh.is_none(), tokens.expires_at), (KEY, true, None));

    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    let sent = auth.bodies().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["code"], "the-code");
    let verifier = sent[0]["code_verifier"].as_str().unwrap();
    assert!(verifier.len() >= 43, "RFC 7636 wants 43 to 128 characters");
    assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())), q["code_challenge"]);
    assert!(!url.as_str().contains(verifier), "the verifier never travels in the URL");
}

#[tokio::test]
async fn the_listener_refuses_a_wrong_state_a_wrong_path_and_a_foreign_host_then_takes_the_real_one() {
    let auth = FakeAuthServer::start().await;
    auth.token_status(200, json!({ "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600 })).await;
    let mut def = pkce_def(&auth, StateMode::Echoed);
    def.result = Outcome::Tokens;
    def.redirect_param = "redirect_uri";
    let (out, url) = login_with(Login::new(def), |url, cb| async move {
        let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();
        let c = http();
        let bad_state = c.get(format!("{cb}?code=x&state=not-it")).send().await.unwrap();
        assert_eq!(bad_state.status(), 400);
        let base = cb.trim_end_matches("/callback").to_owned();
        let wrong_path = c.get(format!("{base}/elsewhere?code=x&state={state}")).send().await.unwrap();
        assert_eq!(wrong_path.status(), 404);
        // A raw request with a Host that is not loopback.
        let addr = Url::parse(&cb).unwrap();
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", addr.port().unwrap())).await.unwrap();
        let req = format!("GET /callback?code=x&state={state} HTTP/1.1\r\nHost: evil.example\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        let ok = c.get(format!("{cb}?code=real&state={state}")).send().await.unwrap();
        assert_eq!(ok.status(), 200);
        // One request completes the login; the listener is gone for a second.
        assert!(c.get(format!("{cb}?code=again&state={state}")).send().await.is_err());
    })
    .await;
    let t = out.unwrap();
    assert_eq!(t.access.expose(), "at-1");
    assert_eq!(t.refresh.as_ref().map(|r| r.expose().as_str()), Some("rt-1"));
    assert!(t.expires_at.is_some());
    let sent = auth.bodies().await;
    assert_eq!(sent.len(), 1, "only the accepted callback reached the token endpoint");
    assert_eq!(sent[0]["code"], "real");
    assert!(url.as_str().contains("state="));
}

#[tokio::test(start_paused = true)]
async fn nobody_calling_back_times_out_after_five_minutes_with_a_clear_message() {
    let auth = FakeAuthServer::start().await;
    let t0 = tokio::time::Instant::now();
    let (out, _) = login_with(Login::new(pkce_def(&auth, StateMode::None)), |_, _| async {}).await;
    let e = out.unwrap_err();
    assert!(e.contains("timed out after 5 minutes") && e.contains("run the command again"), "{e}");
    assert_eq!(t0.elapsed(), Duration::from_secs(300));
}

#[tokio::test(start_paused = true)]
async fn cancelling_ends_the_wait_and_frees_the_port() {
    let auth = FakeAuthServer::start().await;
    let login = Login::new(pkce_def(&auth, StateMode::None));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let notify = move |l: &str| {
        let _ = tx.send(l.to_owned());
    };
    let run = login.authorize(&notify, &cancel);
    let stop = async {
        let mut cb = None;
        while cb.is_none() {
            let line = rx.recv().await.unwrap();
            cb = line.split_whitespace().find(|w| w.starts_with("http")).map(str::to_owned);
        }
        cancel.cancel();
        cb.unwrap()
    };
    let (out, url) = tokio::join!(run, stop);
    assert_eq!(out.unwrap_err(), "cancelled");
    let cb = Url::parse(&url).unwrap().query_pairs().find(|(k, _)| k == "callback_url").unwrap().1.into_owned();
    assert!(http().get(format!("{cb}?code=late")).send().await.is_err());
}

#[tokio::test]
async fn a_busy_preferred_port_falls_back_unless_the_definition_pins_it() {
    let auth = FakeAuthServer::start().await;
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    auth.token_status(200, json!({ "key": KEY })).await;

    let mut def = pkce_def(&auth, StateMode::None);
    def.callback_port = Some(port);
    let (out, _) = login_with(Login::new(def.clone()), |_, cb| async move {
        assert_ne!(Url::parse(&cb).unwrap().port(), Some(port));
        http().get(format!("{cb}?code=c")).send().await.unwrap();
    })
    .await;
    assert!(out.is_ok());

    def.port_pinned = true;
    let cancel = CancellationToken::new();
    let e = Login::new(def).authorize(&|_| {}, &cancel).await.unwrap_err();
    assert!(e.contains(&port.to_string()) && e.contains("in use"), "{e}");
}

#[tokio::test]
async fn a_token_endpoint_refusal_is_an_error_that_does_not_echo_the_code_or_the_verifier() {
    let auth = FakeAuthServer::start().await;
    auth.token_status(400, json!({ "error": "invalid_grant" })).await;
    let (out, _) = login_with(Login::new(pkce_def(&auth, StateMode::None)), |_, cb| async move {
        http().get(format!("{cb}?code=secret-code")).send().await.unwrap();
    })
    .await;
    let e = out.unwrap_err();
    assert!(e.contains("400") && e.contains("invalid_grant"), "{e}");
    let sent = auth.bodies().await;
    assert!(!e.contains("secret-code") && !e.contains(sent[0]["code_verifier"].as_str().unwrap()), "{e}");
}

fn device_def(auth: &FakeAuthServer) -> Def {
    Def {
        kind: Kind::DeviceCode,
        authorize_url: String::new(),
        token_url: auth.url("/token"),
        device_url: Some(auth.url("/device")),
        client_id: Some("aibus-test".into()),
        scopes: vec!["read".into()],
        callback_port: None,
        port_pinned: false,
        redirect_param: "redirect_uri",
        state: StateMode::None,
        result: Outcome::Tokens,
    }
}

async fn device_run(auth: &FakeAuthServer) -> (Result<aibus_core::OAuthTokens, String>, Duration, Vec<String>) {
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = lines.clone();
    let notify = move |l: &str| sink.lock().unwrap().push(l.to_owned());
    // A plain client, without the product's 10 s connect and 30 s request timers (#215). With
    // the clock paused, tokio jumps to the next timer whenever the runtime waits, and a request
    // to the fake server is a real socket wait. With those timers armed the clock ran ahead
    // mid-request and fired them; with no timer armed during a request there is nothing to jump
    // to, so the runtime blocks on the socket and only the flow's own sleeps move the clock.
    let login = Login { http: http(), ..Login::new(device_def(auth)) };
    let t0 = tokio::time::Instant::now();
    let out = login.authorize(&notify, &CancellationToken::new()).await;
    let seen = lines.lock().unwrap().clone();
    (out, t0.elapsed(), seen)
}

#[tokio::test(start_paused = true)]
async fn device_code_waits_through_pending_and_slow_down_then_returns_tokens() {
    let auth = FakeAuthServer::start().await;
    auth.device_start(json!({
        "device_code": "dev-123", "user_code": "WDJB-MJHT",
        "verification_uri": "https://auth.invalid/activate", "expires_in": 900, "interval": 5
    }))
    .await;
    auth.token_script(vec![
        (400, json!({ "error": "authorization_pending" })),
        (400, json!({ "error": "slow_down" })),
        (200, json!({ "access_token": "at-d", "refresh_token": "rt-d", "expires_in": 600 })),
    ])
    .await;
    let (out, took, lines) = device_run(&auth).await;
    let t = out.unwrap();
    assert_eq!(t.access.expose(), "at-d");
    // 5 s, pending, 5 s, slow_down (+5 s from now on), 10 s, done.
    assert_eq!(took, Duration::from_secs(20));
    let shown = lines.join("\n");
    assert!(shown.contains("WDJB-MJHT") && shown.contains("https://auth.invalid/activate"), "{shown}");
    assert!(!shown.contains("dev-123"), "the device code stays private");
    let sent = auth.bodies().await;
    assert_eq!(sent[1]["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
}

#[tokio::test(start_paused = true)]
async fn device_code_stops_on_expired_token_and_access_denied() {
    for (code, wants) in [("expired_token", "expired"), ("access_denied", "denied")] {
        let auth = FakeAuthServer::start().await;
        auth.device_start(json!({
            "device_code": "d", "user_code": "U", "verification_uri": "https://auth.invalid/a",
            "expires_in": 900
        }))
        .await;
        auth.token_script(vec![(400, json!({ "error": code }))]).await;
        let (out, took, _) = device_run(&auth).await;
        let e = out.unwrap_err();
        assert!(e.contains(wants), "{code}: {e}");
        // No `interval` in the reply: the documented 5 s.
        assert_eq!(took, Duration::from_secs(5));
    }
}

#[tokio::test(start_paused = true)]
async fn device_code_gives_up_when_the_code_lifetime_runs_out() {
    let auth = FakeAuthServer::start().await;
    auth.device_start(json!({
        "device_code": "d", "user_code": "U", "verification_uri": "https://auth.invalid/a",
        "expires_in": 12, "interval": 5
    }))
    .await;
    auth.token_status(400, json!({ "error": "authorization_pending" })).await;
    let (out, _, _) = device_run(&auth).await;
    assert!(out.unwrap_err().contains("expired"));
}

/// A token endpoint that stops at the request: it hands the body to `look` while the flow is
/// still waiting for the answer, then replies 400 with `reply(body)` as the error code. The
/// handle gives back whatever `look` returned (and re-raises its panic).
async fn probe<T: Send + 'static>(
    reply: impl FnOnce(&str) -> String + Send + 'static,
    look: impl FnOnce(&str) -> T + Send + 'static,
) -> (String, tokio::task::JoinHandle<T>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let body = loop {
            let n = sock.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).into_owned();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let want = head.to_ascii_lowercase().lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                if body.len() >= want {
                    break body.to_owned();
                }
            }
        };
        let seen = look(&body);
        let reply = json!({ "error": reply(&body) }).to_string();
        let head = format!("HTTP/1.1 400 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", reply.len());
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(reply.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();
        seen
    });
    (url, task)
}

fn field(body: &str, name: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v[name].as_str().map(str::to_owned))
        .or_else(|| body.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_owned)))
        .unwrap_or_else(|| panic!("no {name} in {body}"))
}

/// #191: the PKCE verifier and the authorization code are short-lived credentials, and while the
/// token request is out they are registered with the redactor, so a server that echoes them in
/// its error cannot get them into the error text. They are unregistered when the flow ends.
#[tokio::test]
async fn the_pkce_verifier_and_the_code_are_registered_while_the_token_request_is_out() {
    let redactor = Redactor::new();
    let in_flight = redactor.clone();
    let (token_url, probe) = probe(
        |b| format!("bad grant for code {} with verifier {}", field(b, "code"), field(b, "code_verifier")),
        move |b| {
            let (code, verifier) = (field(b, "code"), field(b, "code_verifier"));
            for (what, v) in [("code", &code), ("verifier", &verifier)] {
                assert_ne!(in_flight.redact(v), v.as_str(), "the {what} is not registered while the token request is out");
            }
            (code, verifier)
        },
    )
    .await;
    let auth = FakeAuthServer::start().await;
    let mut def = pkce_def(&auth, StateMode::None);
    def.token_url = token_url;
    let mut login = Login::new(def);
    login.redactor = redactor.clone();
    let (out, _) = login_with(login, |_, cb| async move {
        http().get(format!("{cb}?code=auth-code-Zq81KpLw0Nn5Vd3E")).send().await.unwrap();
    })
    .await;
    let (code, verifier) = probe.await.unwrap();
    let e = out.unwrap_err();
    assert!(e.contains("400") && e.contains("bad grant"), "{e}");
    assert!(!e.contains(&code) && !e.contains(&verifier), "the server's echo reached the error text: {e}");
    for v in [&code, &verifier] {
        assert_eq!(redactor.redact(v), v.as_str(), "still registered after the flow ended");
    }
}

#[tokio::test(start_paused = true)]
async fn the_device_code_is_registered_while_the_token_request_is_out() {
    let redactor = Redactor::new();
    let in_flight = redactor.clone();
    let (token_url, probe) = probe(
        |b| format!("denied for device {}", field(b, "device_code")),
        move |b| {
            let code = field(b, "device_code");
            assert_ne!(in_flight.redact(&code), code.as_str(), "the device code is not registered while the token request is out");
            code
        },
    )
    .await;
    let auth = FakeAuthServer::start().await;
    auth.device_start(json!({
        "device_code": "dc-Zq81KpLw0Nn5Vd3Ey7Ux2Hc9", "user_code": "WDJB-MJHT",
        "verification_uri": "https://auth.invalid/activate", "expires_in": 900, "interval": 1
    }))
    .await;
    let mut def = device_def(&auth);
    def.token_url = token_url;
    // The plain client for the same reason as in `device_run`.
    let login = Login { http: http(), redactor: redactor.clone(), ..Login::new(def) };
    let notify = |_: &str| {};
    let e = login.authorize(&notify, &CancellationToken::new()).await.unwrap_err();
    let code = probe.await.unwrap();
    assert!(e.contains("denied for device"), "{e}");
    assert!(!e.contains(&code), "the server's echo reached the error text: {e}");
    assert_eq!(redactor.redact(&code), code.as_str(), "still registered after the flow ended");
}
