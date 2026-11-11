//! The MCP server behind `aibus mcp` (epic #95, D-13): a handler over `aibus-core`'s public API
//! with no transport of its own. `serve_stdio` is the one place that touches stdin and stdout,
//! and it writes nothing but JSON-RPC there; logging is the binary's business and goes to stderr.
#![forbid(unsafe_code)]

mod audit;
mod limits;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aibus_core::persist::paths::Paths;
use aibus_core::{workflow, Redactor};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::{json, Value};

use audit::AuditLog;
pub use limits::{CALLS_PER_MINUTE, MAX_ARGS_BYTES};
use limits::RateLimiter;

/// Tools per `tools/list` page.
const PAGE: usize = 50;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// The peer address of an HTTP request. The HTTP host in the binary puts one in the request's
/// extensions and `call_tool` reads it back from there (rmcp hands the request parts to the
/// handler), so the audit line can say who called. Stdio has none.
#[derive(Clone, Copy, Debug)]
pub struct ClientAddr(pub SocketAddr);

/// What the process was started to allow (the epic's capability table). The surface is fixed at
/// start and the model cannot raise it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// `--allow write`: create and update workflows (no tool for it yet).
    pub write: bool,
    /// `--allow gates`: a written workflow may carry `gate` commands.
    pub gates: bool,
    /// `--allow run`: start and watch runs (no tool for it yet).
    pub run: bool,
    /// `--allow-tool`: pre-approvals for runs.
    pub allow_tools: Vec<String>,
}

impl Caps {
    /// `Err` is a usage error: a gate is a shell command that runs later, so `gates` only makes
    /// sense on top of `write`.
    pub fn new(write: bool, gates: bool, run: bool, allow_tools: Vec<String>) -> Result<Self, String> {
        if gates && !write {
            return Err("--allow gates needs --allow write: a gate is part of a written workflow".into());
        }
        Ok(Self { write, gates, run, allow_tools })
    }
}

pub struct Server {
    caps: Caps,
    project: PathBuf,
    config: PathBuf,
    home: Option<PathBuf>,
    audit: AuditLog,
    limiter: Mutex<RateLimiter>,
    redactor: Redactor,
}

impl Server {
    /// A server for the project at `project`, with the user's workflows under `paths.config` and
    /// its audit log under `paths.state`. `home` is only what `~` stands for in the paths it
    /// shows. The first audit line records the flags.
    pub fn new(
        caps: Caps,
        project: PathBuf,
        paths: &Paths,
        home: Option<PathBuf>,
        redactor: Redactor,
    ) -> std::io::Result<Self> {
        let audit = AuditLog::create(&paths.state.join("logs").join("mcp"), redactor.clone())?;
        audit.record(
            None,
            None,
            "server/start",
            None,
            &json!({
                "write": caps.write,
                "gates": caps.gates,
                "run": caps.run,
                "allow_tool": caps.allow_tools,
            }),
            true,
            std::time::Duration::ZERO,
        );
        Ok(Self {
            caps,
            project,
            config: paths.config.clone(),
            home,
            audit,
            limiter: Mutex::new(RateLimiter::new(Instant::now())),
            redactor,
        })
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    /// The tools the flags enable, in the epic's table order. A tool that is off is not listed.
    pub fn tools(&self) -> Vec<Tool> {
        let schema = |v: Value| -> Arc<JsonObject> {
            Arc::new(v.as_object().expect("a schema is an object").clone())
        };
        let item = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "scope": { "type": "string", "enum": ["project", "user"] },
                "path": { "type": "string" },
                "description": { "type": "string" },
                "ok": { "type": "boolean" },
                "error": { "type": "string" },
            },
            "required": ["name", "scope", "path", "description", "ok"],
            "additionalProperties": false,
        });
        vec![Tool::new(
            "workflow_list",
            "List the workflows of this project and of the user, with the parse error of any file that is broken.",
            schema(json!({
                "$schema": DRAFT_2020_12,
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            })),
        )
        .with_raw_output_schema(schema(json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": { "workflows": { "type": "array", "items": item } },
            "required": ["workflows"],
            "additionalProperties": false,
        })))
        .with_annotations(
            ToolAnnotations::new().read_only(true).destructive(false).idempotent(true).open_world(false),
        )]
    }

    /// One `tools/call`, the only way in. It holds what a caller must not be able to skip: the
    /// argument cap, the unknown-tool check, the rate limit, the redaction of the result and the
    /// audit line. `client` is who the request says it is from, `now` is the clock the rate limit
    /// reads.
    ///
    /// A protocol error (`-32602`) is a call that cannot be routed: an unknown tool, arguments
    /// over the cap, arguments the tool's schema forbids. Everything a caller can fix by asking
    /// differently or later is an `isError` result with a sentence.
    pub fn call(
        &self,
        client: Option<&Implementation>,
        name: &str,
        args: Option<JsonObject>,
        now: Instant,
    ) -> Result<CallToolResult, ErrorData> {
        self.call_from(client, None, name, args, now)
    }

    /// `call` for a request that came from a socket: `addr` goes on the audit line.
    pub fn call_from(
        &self,
        client: Option<&Implementation>,
        addr: Option<SocketAddr>,
        name: &str,
        args: Option<JsonObject>,
        now: Instant,
    ) -> Result<CallToolResult, ErrorData> {
        let started = Instant::now();
        let args = args.map_or(Value::Null, Value::Object);
        let result = self.dispatch(name, &args, now);
        let ok = result.as_ref().is_ok_and(|r| r.is_error != Some(true));
        self.audit.record(client, addr, "tools/call", Some(name), &args, ok, started.elapsed());
        result
    }

    fn dispatch(&self, name: &str, args: &Value, now: Instant) -> Result<CallToolResult, ErrorData> {
        if args.to_string().len() > MAX_ARGS_BYTES {
            return Err(ErrorData::invalid_params(
                format!("the arguments are over {} KiB", MAX_ARGS_BYTES / 1024),
                None,
            ));
        }
        if !self.tools().iter().any(|t| t.name == name) {
            // The name is the client's text, so it goes through the redactor like everything else.
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", self.redactor.redact(name)),
                None,
            ));
        }
        let limited = self.limiter.lock().unwrap_or_else(|e| e.into_inner()).take(now);
        if let Err(secs) = limited {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "rate limit: at most {CALLS_PER_MINUTE} tool calls a minute, try again in {secs} s"
            ))]));
        }
        // `workflow_list` is the only tool, and takes nothing.
        if args.as_object().is_some_and(|a| !a.is_empty()) {
            return Err(ErrorData::invalid_params("workflow_list takes no arguments", None));
        }
        let mut found = self.list_workflows();
        self.redactor.redact_json(&mut found);
        Ok(CallToolResult::structured(found))
    }

    /// `{workflows:[{name, scope, path, description, ok, error?}]}`, paths relative to the
    /// project or to `~`.
    fn list_workflows(&self) -> Value {
        let dirs = workflow::dirs(&self.project, Some(&self.config));
        let shorten = |text: &str| -> String {
            let mut out = text.replace(&format!("{}/", self.project.display()), "");
            if let Some(home) = &self.home {
                out = out.replace(&home.display().to_string(), "~");
            }
            out
        };
        let rows: Vec<Value> = workflow::list(&dirs)
            .into_iter()
            .map(|(name, path, parsed)| {
                let scope = if path.starts_with(&dirs[0]) { "project" } else { "user" };
                let mut row = json!({
                    "name": name,
                    "scope": scope,
                    "path": shorten(&path.to_string_lossy().replace('\\', "/")),
                    "description": "",
                    "ok": parsed.is_ok(),
                });
                match parsed {
                    Ok(w) => row["description"] = json!(w.description),
                    Err(e) => row["error"] = json!(shorten(&e.to_string())),
                }
                row
            })
            .collect();
        json!({ "workflows": rows })
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("aibus", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "aibus workflows. Start with workflow_list. This server is read-only unless it was started with --allow flags.",
            )
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.tools();
        let start = match request.and_then(|r| r.cursor) {
            None => 0,
            Some(c) => c
                .strip_prefix('p')
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n <= tools.len())
                .ok_or_else(|| ErrorData::invalid_params("invalid cursor", None))?,
        };
        let end = (start + PAGE).min(tools.len());
        let mut page = ListToolsResult::with_all_items(tools[start..end].to_vec());
        page.next_cursor = (end < tools.len()).then(|| format!("p{end}"));
        Ok(page)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = context.client_info();
        let addr = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|p| p.extensions.get::<ClientAddr>())
            .map(|a| a.0);
        self.call_from(client.as_ref(), addr, &request.name, request.arguments, Instant::now())
            .map(CallToolResponse::from)
    }
}

/// Serves `server` on stdin and stdout until the client closes stdin. Nothing else writes to
/// stdout.
pub fn serve_stdio(server: Server) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tracing::debug!(version = env!("CARGO_PKG_VERSION"), "aibus mcp serving on stdio");
        let running = server.serve(rmcp::transport::stdio()).await.map_err(|e| e.to_string())?;
        running.waiting().await.map(|_| ()).map_err(|e| e.to_string())
    })
}
