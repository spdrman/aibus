//! `aibus mcp --http`: the Streamable HTTP listener (E11, #106). It lives in the binary so that
//! `aibus-mcp` keeps no socket and no HTTP stack in its closure; rmcp's Tower service does the
//! protocol, the `Host` and `Origin` checks (403) and the `MCP-Protocol-Version` check (400), and
//! this file adds the three things aibus insists on: the listener is already bound to a loopback
//! address when it gets here, every request needs the bearer token, and the peer address reaches
//! the audit line.
//!
//! The token is a `Secret`, it is compared in constant time (as SHA-256 digests, so its length
//! does not leak either), and nothing here writes it anywhere.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use aibus_mcp::{ClientAddr, Server};
use aibus_provider::Secret;
use bytes::Bytes;
use http::{header, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::signals::{Signal, Signals};

/// The one endpoint (POST and GET both).
const ENDPOINT: &str = "/mcp";

type Body = BoxBody<Bytes, Infallible>;
type Mcp = StreamableHttpService<Arc<Server>, LocalSessionManager>;

/// Serves `server` on `bind` (loopback, checked by the caller) until SIGINT, SIGTERM or SIGHUP.
/// `origins` are accepted on top of the loopback ones. stdout gets one line with the bound URL
/// (port 0 asks the system for one); everything else goes to the log on stderr.
pub fn serve(
    server: Server,
    bind: SocketAddr,
    origins: Vec<String>,
    token: Secret<String>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let listener = TcpListener::bind(bind).await.map_err(|e| format!("cannot listen on {bind}: {e}"))?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        let port = local.port();
        let mut signals = Signals::new().map_err(|e| e.to_string())?;

        // Exact `host:port` and `scheme://host:port`, never a wildcard: the port is the one we
        // are listening on, and a page on another port is another origin.
        let hosts = [local.to_string(), format!("localhost:{port}"), format!("127.0.0.1:{port}"), format!("[::1]:{port}")];
        let mut allowed_origins: Vec<String> = hosts.iter().map(|h| format!("http://{h}")).collect();
        allowed_origins.extend(origins);

        let stop = CancellationToken::new();
        let config = StreamableHttpServerConfig::default()
            .with_allowed_hosts(hosts)
            .with_allowed_origins(allowed_origins)
            .enforce_origin_validation()
            .with_cancellation_token(stop.child_token());
        let shared = Arc::new(server);
        let mcp: Mcp =
            StreamableHttpService::new(move || Ok(shared.clone()), Arc::new(LocalSessionManager::default()), config);
        let token = Arc::new(token);

        println!("aibus mcp listening on http://{local}{ENDPOINT}");
        tracing::info!(%local, "aibus mcp serving Streamable HTTP");

        loop {
            tokio::select! {
                sig = signals.recv() => {
                    if sig == Signal::Stop {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::warn!("accept: {e}");
                            continue;
                        }
                    };
                    let (mcp, token) = (mcp.clone(), token.clone());
                    tokio::spawn(async move {
                        let service = service_fn(move |req| {
                            let (mcp, token) = (mcp.clone(), token.clone());
                            async move { Ok::<_, Infallible>(handle(&mcp, &token, peer, req).await) }
                        });
                        let served = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                        if let Err(e) = served {
                            tracing::debug!(%peer, "connection ended: {e}");
                        }
                    });
                }
            }
        }
        // Ends every open stream; the runtime going away closes the rest.
        stop.cancel();
        tracing::info!("aibus mcp stopped");
        Ok(())
    })
}

async fn handle(
    mcp: &Mcp,
    token: &Secret<String>,
    peer: SocketAddr,
    mut req: Request<Incoming>,
) -> Response<Body> {
    // The token comes first, before the path or anything rmcp would say, so an unauthenticated
    // caller learns nothing about the server except that it wants a token.
    if !bearer_matches(req.headers().get(header::AUTHORIZATION), token.expose()) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, "Bearer")
            .body(Full::from("a bearer token is required").boxed())
            .expect("a static response");
    }
    if req.uri().path() != ENDPOINT {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::from("the endpoint is /mcp").boxed())
            .expect("a static response");
    }
    req.extensions_mut().insert(ClientAddr(peer));
    mcp.handle(req).await
}

/// `Authorization: Bearer <token>` with exactly the token. Both sides are hashed first, so the
/// comparison takes the same time whatever the attacker sends.
fn bearer_matches(header: Option<&header::HeaderValue>, token: &str) -> bool {
    let sent = header
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let ok: bool = Sha256::digest(sent.as_bytes())
        .as_slice()
        .ct_eq(Sha256::digest(token.as_bytes()).as_slice())
        .into();
    // An absent or malformed header hashes like the empty string, which no token equals.
    ok && !sent.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_bearer_token_passes() {
        let v = |s: &str| header::HeaderValue::from_str(s).unwrap();
        assert!(bearer_matches(
            Some(&v("Bearer s3cret-token")),
            "s3cret-token"
        ));
        assert!(!bearer_matches(
            Some(&v("Bearer s3cret-token ")),
            "s3cret-token"
        ));
        assert!(!bearer_matches(
            Some(&v("bearer s3cret-token")),
            "s3cret-token"
        ));
        assert!(!bearer_matches(Some(&v("s3cret-token")), "s3cret-token"));
        assert!(!bearer_matches(Some(&v("Bearer ")), ""));
        assert!(!bearer_matches(None, "s3cret-token"));
    }
}
