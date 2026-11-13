//! `aibus mcp`: the stdio MCP server of `aibus-mcp`. stdout carries JSON-RPC and nothing else;
//! the log goes to stderr, through the redactor, at the level `-v`, `AIBUS_LOG` or `RUST_LOG`
//! give (warnings by default).

use aibus_core::config;
use aibus_core::persist::paths::project_dir;
use aibus_core::{AccountName, AibusError, Redactor};
use aibus_mcp::{Caps, ConfirmConfig, RunConfig, Server};
use aibus_provider::Secret;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::cli::{ConfirmMode, Global, McpArgs};
use crate::logging;
use crate::mcp_http;

pub fn mcp(g: &Global, args: &McpArgs, redactor: &Redactor) -> Result<u8, AibusError> {
    let has = |cap: &str| args.allow.iter().any(|a| a == cap);
    let mut caps = Caps::new(has("write"), has("gates"), has("run"), args.allow_tool.clone())
        .map_err(AibusError::Usage)?;
    caps.no_provenance = args.no_provenance;
    let capped = args.max_runs.is_some() || args.max_concurrent_runs.is_some() || args.max_cost_usd.is_some();
    if capped && !caps.run {
        return Err(AibusError::Usage(
            "--max-runs, --max-concurrent-runs and --max-cost-usd need --allow run".into(),
        ));
    }
    if args.max_runs == Some(0) || args.max_concurrent_runs == Some(0) {
        return Err(AibusError::Usage("--max-runs and --max-concurrent-runs are at least 1".into()));
    }
    if args.max_cost_usd.is_some_and(|c| !c.is_finite() || c <= 0.0) {
        return Err(AibusError::Usage("--max-cost-usd is a positive number".into()));
    }

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
    let (project, paths) = (g.project_dir()?, g.paths()?);
    // `--max-cost-usd` defaults to the configured per-run limit. Reading the config reads no
    // credential; the first run's own process does that, when it starts.
    let runs = if caps.run {
        let loaded = config::load(&paths.config_file(), Some(&project_dir(&project).join("settings.toml")))?;
        let mut runs = RunConfig::new(
            std::env::current_exe()?,
            args.max_cost_usd.unwrap_or(loaded.config.limits.max_cost_usd),
        );
        runs.max_runs = args.max_runs.unwrap_or(runs.max_runs);
        runs.max_concurrent = args.max_concurrent_runs.unwrap_or(runs.max_concurrent);
        Some(runs)
    } else {
        None
    };
    let confirm = ConfirmConfig {
        elicit: args.confirm == ConfirmMode::Elicit,
        run_cap_usd: runs.as_ref().map(|r| r.max_cost_usd),
        ..ConfirmConfig::default()
    };
    let mut server = Server::new(
        caps,
        project,
        &paths,
        std::env::var_os("HOME").map(Into::into),
        redactor.clone(),
    )?
    .with_confirm(confirm);
    if let Some(runs) = runs {
        server = server.with_runs(runs);
    }
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
