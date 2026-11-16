//! `aibus sessions` (#355, phase 1 of #348): list the Claude Code sessions open in other
//! terminals and tap in to one, read-only. The texts come from `aibus_tap::show`, which builds on
//! `aibus_trace::view`, so this prints what `/sessions` and `/tap` show and what `aibus runs
//! inspect` prints for the graph and timeline. Nothing here sends anything to a session.

use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use aibus_core::{AibusError, Redactor};
use aibus_tap::{agents, hooks, show, tap, transcript, Env};
use bytes::Bytes;
use http::{header, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::json;

use crate::cli::{Global, SessionsArgs, SessionsCmd, SessionsHooksCmd};

fn usage(s: impl Into<String>) -> AibusError {
    AibusError::Usage(s.into())
}

fn env(g: &Global, config_dir: &Option<PathBuf>) -> Result<Env, AibusError> {
    let config = config_dir
        .clone()
        .or_else(Env::claude_config_from_env)
        .ok_or_else(|| usage("cannot tell where Claude Code keeps its config: set CLAUDE_CONFIG_DIR or pass --config-dir"))?;
    Ok(Env {
        state: g.paths()?.state,
        config,
    })
}

fn claude_only(client: &str) -> Result<(), AibusError> {
    if client == "claude" {
        Ok(())
    } else {
        Err(usage(format!(
            "client {}: phase 1 of tap-in knows only `claude`",
            aibus_trace::view::clean(client, 40)
        )))
    }
}

pub fn sessions(args: &SessionsArgs, g: &Global, redactor: &Redactor) -> Result<u8, AibusError> {
    let mut out = std::io::stdout().lock();
    match args.cmd.as_ref().unwrap_or(&SessionsCmd::List) {
        SessionsCmd::List => {
            let e = env(g, &None)?;
            if g.json {
                let mut list = agents::parse_agents(&agents::run_agents_json().map_err(usage)?)
                    .map_err(usage)?;
                agents::merge_local(&mut list, &e.config.join("sessions"));
                let rows: Vec<_> = list
                    .iter()
                    .map(|s| {
                        json!({
                            "client": "claude", "session_id": s.session_id, "job_id": s.job_id,
                            "name": s.name, "cwd": s.cwd, "kind": s.kind, "state": s.state,
                            "started_ms": s.started_ms, "pid": s.pid, "can_do": ["watch"],
                        })
                    })
                    .collect();
                writeln!(out, "{}", serde_json::Value::Array(rows))?;
            } else {
                let text = show::list(&e, &agents::run_agents_json, redactor).map_err(usage)?;
                out.write_all(text.as_bytes())?;
            }
        }
        SessionsCmd::Tap {
            session,
            follow,
            config_dir,
        } => {
            let e = env(g, config_dir)?;
            let text = show::tap(&e, &agents::run_agents_json, redactor, session).map_err(usage)?;
            out.write_all(text.as_bytes())?;
            if *follow {
                out.flush()?;
                drop(out);
                return follow_session(&e, session, redactor).map(|()| 0);
            }
        }
        SessionsCmd::Hooks(h) => {
            let (cmd, client, config_dir, yes) = match h {
                SessionsHooksCmd::Install {
                    client,
                    config_dir,
                    yes,
                } => (true, client, config_dir, *yes),
                SessionsHooksCmd::Uninstall {
                    client,
                    config_dir,
                    yes,
                } => (false, client, config_dir, *yes),
            };
            claude_only(client)?;
            let e = env(g, config_dir)?;
            let path = e.config.join("settings.json");
            let ep = hooks::Endpoint::load_or_create(&e.state).map_err(usage)?;
            // The script holds no secret, so it is written before the diff names it; the
            // settings change only after the yes.
            let command = if cmd {
                Some(hooks::command_for(
                    &hooks::install_files(&e.state, &ep).map_err(usage)?,
                ))
            } else {
                None
            };
            let mut confirm = |diff: &str| {
                let _ = writeln!(out, "change to {}:\n{diff}", path.display());
                if yes {
                    return true;
                }
                let _ = write!(out, "write it? [y/N] ");
                let _ = out.flush();
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line).is_ok()
                    && matches!(line.trim(), "y" | "Y" | "yes")
            };
            match hooks::apply(&path, command.as_deref(), &mut confirm).map_err(usage)? {
                hooks::Outcome::Changed if cmd => writeln!(
                    out,
                    "hooks installed (command hooks running a script in aibus's state directory; the token is in a 0600 file next to it, not in your settings). Claude Code reloads hook edits into sessions that are already open, so they start posting to aibus; if aibus is not listening, the script gives up at once (one second at most) and prints nothing. `aibus sessions hooks uninstall --client claude` takes them out."
                )?,
                hooks::Outcome::Changed => {
                    // Whoever still holds the old token can post nothing more.
                    ep.rotate(&e.state).map_err(usage)?;
                    writeln!(out, "hooks removed, and the receiver's token rotated.")?
                }
                hooks::Outcome::Declined => writeln!(out, "nothing written.")?,
                hooks::Outcome::Unchanged => writeln!(out, "nothing to change.")?,
            }
        }
        SessionsCmd::AllowTranscripts { client, revoke } => {
            claude_only(client)?;
            let e = env(g, &None)?;
            transcript::set_allowed(&e.state, !revoke).map_err(usage)?;
            if *revoke {
                writeln!(out, "transcripts: off for claude.")?;
            } else {
                writeln!(
                    out,
                    "transcripts: on for claude. `aibus sessions tap` may read the end of a session's transcript: read-only, redacted before it is shown, never copied into the store, never sent anywhere. Revoke with --revoke."
                )?;
            }
        }
    }
    out.flush()?;
    Ok(0)
}

/// Listens for the hook events of every session on the install's loopback endpoint, records the
/// tapped one's, and prints the tap text again (at most twice a second) when it changed.
fn follow_session(e: &Env, arg: &str, redactor: &Redactor) -> Result<(), AibusError> {
    let list = agents::parse_agents(&agents::run_agents_json().map_err(usage)?).map_err(usage)?;
    let id = agents::resolve(&list, arg)
        .map(|s| s.session_id.clone())
        .unwrap_or_else(|_| arg.to_owned());
    let ep = hooks::Endpoint::load_or_create(&e.state).map_err(usage)?;
    let tapper = Arc::new(Mutex::new(
        tap::Tap::open(&e.taps(), &id, redactor).map_err(usage)?,
    ));
    let dirty = Arc::new(AtomicBool::new(false));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let addr = SocketAddr::from(([127, 0, 0, 1], ep.port));
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|err| {
            usage(format!("cannot listen on {addr}: {err} (the port is the one the hooks were installed with)"))
        })?;
        eprintln!("listening on http://{addr}{} for hook events of session {}; Ctrl-C stops", hooks::HOOK_PATH, aibus_trace::view::clean(&id, 64));
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if dirty.swap(false, Ordering::Relaxed) {
                        let cached = serde_json::to_vec(&list_json(&list)).unwrap_or_default();
                        match show::tap(e, &|| Ok(cached.clone()), redactor, &id) {
                            Ok(t) => println!("\n--- {} ---\n{t}", aibus_core::persist::time::rfc3339(SystemTime::now())),
                            Err(err) => eprintln!("{err}"),
                        }
                    }
                }
                conn = listener.accept() => {
                    let Ok((stream, peer)) = conn else { continue };
                    if !peer.ip().is_loopback() { continue; }
                    let (ep, tapper, dirty) = (ep.clone(), tapper.clone(), dirty.clone());
                    tokio::spawn(async move {
                        let svc = service_fn(move |req| receive(req, ep.clone(), tapper.clone(), dirty.clone()));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), svc)
                            .await;
                    });
                }
            }
        }
    })
}

/// The list as `claude agents --json` printed it, for re-rendering without asking again.
fn list_json(list: &[agents::Session]) -> serde_json::Value {
    serde_json::Value::Array(
        list.iter()
            .map(|s| json!({"sessionId": s.session_id, "id": s.job_id, "name": s.name, "cwd": s.cwd, "kind": s.kind, "startedAt": s.started_ms, "state": s.state, "pid": s.pid}))
            .collect(),
    )
}

/// One hook POST. Only the right method, path, `Host` and token get past; the body is capped at
/// 1 MiB; an event of another session is answered 200 and dropped. The answer never carries a
/// decision, so a hook of aibus's can neither allow nor deny anything.
async fn receive(
    req: Request<Incoming>,
    ep: hooks::Endpoint,
    tapper: Arc<Mutex<tap::Tap>>,
    dirty: Arc<AtomicBool>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let reply = |s: StatusCode| {
        Ok(Response::builder()
            .status(s)
            .body(Full::new(Bytes::new()))
            .unwrap_or_default())
    };
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h == format!("127.0.0.1:{}", ep.port));
    if req.method() != Method::POST || req.uri().path() != hooks::HOOK_PATH || !host_ok {
        return reply(StatusCode::NOT_FOUND);
    }
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    if !hooks::accepts(&ep, auth.as_deref()) {
        return reply(StatusCode::UNAUTHORIZED);
    }
    let Ok(body) = Limited::new(req.into_body(), 1 << 20).collect().await else {
        return reply(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let body = body.to_bytes();
    if let Ok(mut t) = tapper.lock() {
        if t.ingest(&body, SystemTime::now()).is_ok_and(|n| n > 0) {
            dirty.store(true, Ordering::Relaxed);
        }
    }
    reply(StatusCode::OK)
}
