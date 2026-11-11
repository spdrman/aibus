//! `aibus mcp`: the stdio MCP server of `aibus-mcp`. stdout carries JSON-RPC and nothing else;
//! the log goes to stderr, through the redactor, at the level `-v`, `AIBUS_LOG` or `RUST_LOG`
//! give (warnings by default).

use aibus_core::{AccountName, AibusError, Redactor};
use aibus_mcp::{Caps, ConfirmConfig, Server};
use aibus_provider::Secret;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::cli::{ConfirmMode, Global, McpArgs};
use crate::logging;
use crate::mcp_http;

pub fn mcp(g: &Global, args: &McpArgs, redactor: &Redactor) -> Result<u8, AibusError> {
    if args.max_runs.is_some() || args.max_concurrent_runs.is_some() || args.max_cost_usd.is_some() {
        return Err(AibusError::Usage(
            "--max-runs, --max-concurrent-runs and --max-cost-usd are not available yet: \
             runs over MCP come with the run tools"
                .into(),
        ));
    }
    let has = |cap: &str| args.allow.iter().any(|a| a == cap);
    let mut caps = Caps::new(has("write"), has("gates"), has("run"), args.allow_tool.clone())
        .map_err(AibusError::Usage)?;
    caps.no_provenance = args.no_provenance;

    // The address and the token are settled before anything is created: a refused address leaves
    // no audit file and no socket behind.
    let http = args.http.as_deref().map(|text| http_setup(text, redactor)).transpose()?;

    let directives = std::env::var("AIBUS_LOG").or_else(|_| std::env::var("RUST_LOG")).ok();
    let (targets, bad) = logging::filter(g, directives.as_deref());
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(
        aibus_core::redact::layer(redactor.clone(), std::io::stderr).with_filter(targets),
    ));
    for d in bad {
        tracing::warn!("log directive {d:?} ignored");
    }

    tracing::info!(write = caps.write, gates = caps.gates, run = caps.run, "aibus mcp starting");
    let server = Server::new(
        caps,
        g.project_dir()?,
        &g.paths()?,
        std::env::var_os("HOME").map(Into::into),
        redactor.clone(),
    )?
    .with_confirm(ConfirmConfig { elicit: args.confirm == ConfirmMode::Elicit, ..ConfirmConfig::default() });
    match http {
        Some((bind, token)) => mcp_http::serve(server, bind, args.origin.clone(), token),
        None => aibus_mcp::serve_stdio(server),
    }
    .map_err(|e| AibusError::Io(std::io::Error::other(e)))?;
    Ok(0)
}

/// `--http <addr>`: the loopback address (anything else is a usage error, and there is no flag
/// to allow it) and the bearer token. The token is `AIBUS_MCP_TOKEN`, or 32 random bytes printed
/// once to stderr with the header line to paste; it is never written anywhere else, and it is
/// registered with the redactor so a log line that quoted it would be scrubbed.
fn http_setup(text: &str, redactor: &Redactor) -> Result<(std::net::SocketAddr, Secret<String>), AibusError> {
    let bind: std::net::SocketAddr = text.parse().map_err(|_| {
        AibusError::Usage(format!("--http wants <ip>:<port> such as 127.0.0.1:7777, not {text:?}"))
    })?;
    if !bind.ip().is_loopback() {
        return Err(AibusError::Usage(format!(
            "--http binds the loopback interface only (127.0.0.0/8 or ::1), and {} is not on it; \
             there is no flag to change that. For a client on another machine use an SSH tunnel, \
             see docs/MCP.md",
            bind.ip()
        )));
    }
    let token = match std::env::var("AIBUS_MCP_TOKEN") {
        Ok(t) if t.len() >= 16 && t.bytes().all(|b| b.is_ascii_graphic()) => t,
        Ok(_) => {
            return Err(AibusError::Usage(
                "AIBUS_MCP_TOKEN must be at least 16 visible ASCII characters".into(),
            ))
        }
        Err(_) => {
            let t: String = rand::random::<[u8; 32]>().iter().map(|b| format!("{b:02x}")).collect();
            eprintln!(
                "aibus mcp: AIBUS_MCP_TOKEN is not set, so this run made a token. It is shown once and not stored.\n\
                 Authorization: Bearer {t}"
            );
            t
        }
    };
    redactor.register(&AccountName("mcp-http-token".into()), &token);
    Ok((bind, Secret::new(token)))
}
