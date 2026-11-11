//! `aibus mcp`: the stdio MCP server of `aibus-mcp`. stdout carries JSON-RPC and nothing else;
//! the log goes to stderr, through the redactor, at the level `-v`, `AIBUS_LOG` or `RUST_LOG`
//! give (warnings by default).

use aibus_core::{AibusError, Redactor};
use aibus_mcp::{Caps, Server};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::cli::{Global, McpArgs};
use crate::logging;

pub fn mcp(g: &Global, args: &McpArgs, redactor: &Redactor) -> Result<u8, AibusError> {
    if args.max_runs.is_some() || args.max_concurrent_runs.is_some() || args.max_cost_usd.is_some() {
        return Err(AibusError::Usage(
            "--max-runs, --max-concurrent-runs and --max-cost-usd are not available yet: \
             runs over MCP come with the run tools"
                .into(),
        ));
    }
    let has = |cap: &str| args.allow.iter().any(|a| a == cap);
    let caps = Caps::new(has("write"), has("gates"), has("run"), args.allow_tool.clone())
        .map_err(AibusError::Usage)?;

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
    )?;
    aibus_mcp::serve_stdio(server).map_err(|e| AibusError::Io(std::io::Error::other(e)))?;
    Ok(0)
}
