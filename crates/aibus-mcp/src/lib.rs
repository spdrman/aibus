//! The MCP server behind `aibus mcp` (epic #95, D-13): a handler over `aibus-core`'s public API
//! with no transport of its own. `serve_stdio` is the one place that touches stdin and stdout,
//! and it writes nothing but JSON-RPC there; logging is the binary's business and goes to stderr.
#![forbid(unsafe_code)]

mod audit;
mod confirm;
mod limits;
pub mod prompts;
mod read;
pub mod resources;
mod run;
mod write;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibus_core::persist::paths::Paths;
use aibus_core::{workflow, AgentName, Redactor};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, Implementation, JsonObject, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
    Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};

use audit::AuditLog;
pub use confirm::ConfirmConfig;
use confirm::Confirm;
pub use limits::{CALLS_PER_MINUTE, MAX_ARGS_BYTES, RUNS_PER_MINUTE, WRITES_PER_MINUTE};
use limits::RateLimiter;
pub use run::RunConfig;

/// Tools per `tools/list` page.
const PAGE: usize = 50;

pub(crate) const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// The peer address of an HTTP request. The HTTP host in the binary puts one in the request's
/// extensions and `call_tool` reads it back from there (rmcp hands the request parts to the
/// handler), so the audit line can say who called. Stdio has none.
#[derive(Clone, Copy, Debug)]
pub struct ClientAddr(pub SocketAddr);

/// Who sent the request, for the audit line: the address an HTTP host stored in the request's
/// extensions, or nothing on stdio.
fn peer(context: &RequestContext<RoleServer>) -> Option<SocketAddr> {
    context
        .extensions
        .get::<http::request::Parts>()
        .and_then(|p| p.extensions.get::<ClientAddr>())
        .map(|a| a.0)
}

/// What the process was started to allow (the epic's capability table). The surface is fixed at
/// start and the model cannot raise it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// `--allow write`: `workflow_create` and `workflow_update`.
    pub write: bool,
    /// `--allow gates`: a written workflow may carry `gate` commands.
    pub gates: bool,
    /// `--allow run`: `workflow_run`, `run_status` and `run_cancel`.
    pub run: bool,
    /// `--allow-tool`: pre-approvals for runs.
    pub allow_tools: Vec<String>,
    /// `--no-provenance`: do not put the `# written through aibus mcp` line in written files.
    pub no_provenance: bool,
}

impl Caps {
    /// `Err` is a usage error: a gate is a shell command that runs later, so `gates` only makes
    /// sense on top of `write`.
    pub fn new(write: bool, gates: bool, run: bool, allow_tools: Vec<String>) -> Result<Self, String> {
        if gates && !write {
            return Err("--allow gates needs --allow write: a gate is part of a written workflow".into());
        }
        Ok(Self { write, gates, run, allow_tools, no_provenance: false })
    }
}

pub struct Server {
    caps: Caps,
    project: PathBuf,
    config: PathBuf,
    home: Option<PathBuf>,
    audit: AuditLog,
    limiter: Mutex<RateLimiter>,
    write_limiter: Mutex<RateLimiter>,
    run_limiter: Mutex<RateLimiter>,
    /// Held from the revision check to the rename, so two writes in this process cannot both
    /// pass the check.
    write_lock: Mutex<()>,
    runs: Option<Arc<run::Runs>>,
    redactor: Redactor,
    confirm: Confirm,
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
            limiter: Mutex::new(RateLimiter::new(CALLS_PER_MINUTE, Instant::now())),
            write_limiter: Mutex::new(RateLimiter::new(WRITES_PER_MINUTE, Instant::now())),
            run_limiter: Mutex::new(RateLimiter::new(RUNS_PER_MINUTE, Instant::now())),
            write_lock: Mutex::new(()),
            runs: None,
            redactor,
            confirm: Confirm::new(ConfirmConfig::default()),
        })
    }

    /// How runs are started (`--allow run`). Nothing is spawned until the first `workflow_run`,
    /// so a client that only reads never causes a session or a credential lookup.
    pub fn with_runs(mut self, config: RunConfig) -> Self {
        self.runs = Some(Arc::new(run::Runs::new(config)));
        self
    }

    /// An `isError` result: the sentence a caller can act on, through the redactor.
    fn fail(&self, message: impl AsRef<str>) -> CallToolResult {
        let text = self.redactor.redact(message.as_ref()).into_owned();
        CallToolResult::error(vec![ContentBlock::text(text)])
    }

    /// A structured result, through the redactor.
    fn done(&self, mut body: Value) -> CallToolResult {
        self.redactor.redact_json(&mut body);
        CallToolResult::structured(body)
    }

    /// The agents a workflow may name: whatever discovery finds for this project and the user.
    /// `agents.extra_dirs` from the config are not read here.
    fn roster(&self) -> Result<Vec<AgentName>, String> {
        let user = self.config.join("agents");
        aibus_core::agent::discover::discover(None, &self.project, Some(&user), &[], &[])
            .map(|found| found.agents.into_keys().collect())
            .map_err(|e| format!("agent discovery failed: {e}"))
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    /// The tools the flags enable, in the epic's table order. A tool that is off is not listed.
    pub fn tools(&self) -> Vec<Tool> {
        let item = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "scope": { "type": "string", "enum": ["project", "user"] },
                "path": { "type": "string" },
                "description": { "type": "string" },
                "ok": { "type": "boolean" },
                "runnable": { "type": "boolean" },
                "bundle": { "type": "string" },
                "error": { "type": "string" },
            },
            "required": ["name", "scope", "path", "description", "ok"],
            "additionalProperties": false,
        });
        let mut tools = vec![Tool::new(
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
        )];
        tools.extend(read::tools());
        if self.caps.write {
            tools.extend(write::tools());
        }
        if self.caps.run {
            tools.extend(run::tools());
        }
        tools
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
        let mut run = None;
        let result = self.dispatch(name, &args, now, &mut run);
        let ok = result.as_ref().is_ok_and(|r| r.is_error != Some(true));
        let run = run.or_else(|| {
            let given = args.get("run")?.as_str()?;
            (given.starts_with("run-") && given.len() <= 40).then(|| given.to_owned())
        });
        self.audit.record(client, addr, "tools/call", Some(name), run.as_deref(), &args, ok, started.elapsed());
        result
    }

    /// `call`, for the transport: `run_status` with `wait_ms` waits (without holding the thread)
    /// until the run changes or the wait is spent, and `run_cancel` waits for the run to end. The
    /// synchronous `call` answers at once either way.
    pub async fn call_async(
        &self,
        client: Option<&Implementation>,
        addr: Option<SocketAddr>,
        name: &str,
        args: Option<JsonObject>,
        now: Instant,
    ) -> Result<CallToolResult, ErrorData> {
        let id = args.as_ref().and_then(|a| a.get("run")).and_then(Value::as_str).map(str::to_owned);
        let wait_ms = args.as_ref().and_then(|a| a.get("wait_ms")).and_then(Value::as_u64);
        let (Some(runs), true, Some(id)) = (&self.runs, self.caps.run, id) else {
            return self.call_from(client, addr, name, args, now);
        };
        if name == "run_status" {
            if let Some(ms) = wait_ms.filter(|ms| *ms > 0) {
                runs.wait_changed(&id, Duration::from_millis(ms)).await;
            }
            return self.call_from(client, addr, name, args, now);
        }
        let answered = self.call_from(client, addr, name, args, now);
        if name == "run_cancel" && answered.as_ref().is_ok_and(|r| r.is_error != Some(true)) {
            runs.wait_ended(&id).await;
            if let Some(body) = runs.body(&id, &self.redactor) {
                return Ok(self.done(body));
            }
        }
        answered
    }

    fn dispatch(
        &self,
        name: &str,
        args: &Value,
        now: Instant,
        run: &mut Option<String>,
    ) -> Result<CallToolResult, ErrorData> {
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
        match name {
            "workflow_list" => {
                if args.as_object().is_some_and(|a| !a.is_empty()) {
                    return Err(ErrorData::invalid_params("workflow_list takes no arguments", None));
                }
                let mut found = self.list_workflows();
                self.redactor.redact_json(&mut found);
                Ok(CallToolResult::structured(found))
            }
            "workflow_create" => self.workflow_write(false, args, now),
            "workflow_update" => self.workflow_write(true, args, now),
            "workflow_run" => self.workflow_run(args, now, run),
            "run_status" | "run_cancel" => self.run_tool(name, args, run),
            // The read tools; it answers `-32602` for a name it does not know either.
            other => self.read_tool(other, args),
        }
    }

    /// `{workflows:[{name, scope, path, description, ok, runnable, bundle?, error?}]}`, paths
    /// relative to the project or to `~`. `bundle` is the `.aibus` file a workflow came from;
    /// `runnable` is false for a broken file and for a bundled workflow with a gate.
    fn list_workflows(&self) -> Value {
        let dirs = workflow::dirs(&self.project, Some(&self.config));
        let shorten = |text: &str| self.shorten(text);
        let rows: Vec<Value> = workflow::list(&dirs)
            .into_iter()
            .map(|found| {
                let mut row = json!({
                    "name": found.name,
                    "scope": found.scope,
                    "path": shorten(&found.path.to_string_lossy().replace('\\', "/")),
                    "description": "",
                    "ok": found.parsed.is_ok(),
                    "runnable": found.runnable().is_ok(),
                });
                if let Some(b) = &found.bundle {
                    row["bundle"] = json!(b);
                }
                match (&found.parsed, found.runnable()) {
                    (Ok(w), Ok(_)) => row["description"] = json!(w.description),
                    (Ok(w), Err(why)) => {
                        row["description"] = json!(w.description);
                        row["error"] = json!(shorten(&why));
                    }
                    (Err(e), _) => row["error"] = json!(shorten(&e.to_string())),
                }
                row
            })
            .collect();
        json!({ "workflows": rows })
    }
}

/// The end of the server, whichever way it comes (stdin closed, an error): every live run is
/// cancelled and gets the SIGTERM budget to write its snapshot.
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(runs) = &self.runs {
            runs.shutdown();
        }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().enable_prompts().build())
            .with_server_info(Implementation::new("aibus", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "aibus workflows. Start with workflow_list; workflow_validate checks a draft before anything is written. This server is read-only unless it was started with --allow flags.",
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
        let addr = peer(&context);
        if let Some(stop) = self.confirm(&request, &context, client.as_ref(), addr).await {
            return Ok(stop);
        }
        self.call_async(client.as_ref(), addr, &request.name, request.arguments, Instant::now())
            .await
            .map(CallToolResponse::from)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let mut all = resources::list();
        if let Some(runs) = self.runs.as_ref().filter(|_| self.caps.run) {
            all.extend(runs.ids().into_iter().map(|id| {
                Resource::new(format!("aibus://runs/{id}"), id)
                    .with_description("A run this server started: its status.")
                    .with_mime_type("application/json")
            }));
        }
        Ok(ListResourcesResult::with_all_items(all))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let mut all = resources::templates();
        if self.caps.run {
            all.extend(resources::run_templates());
        }
        Ok(ListResourceTemplatesResult::with_all_items(all))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let started = Instant::now();
        let read = self.read_resource_text(&request.uri);
        let uri = self.redactor.redact(&request.uri).into_owned();
        self.audit.record(
            context.client_info().as_ref(),
            peer(&context),
            "resources/read",
            None,
            request.uri.strip_prefix("aibus://runs/").and_then(|r| r.split('/').next()).filter(|r| r.starts_with("run-") && r.len() <= 40),
            &json!({ "uri": uri }),
            read.is_ok(),
            started.elapsed(),
        );
        let (mime, text) = read?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, uri).with_mime_type(mime)]).into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompts::list()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let started = Instant::now();
        let args = request.arguments.unwrap_or_default();
        let got = self.get_prompt_text(&request.name, &args);
        let name = self.redactor.redact(&request.name).into_owned();
        self.audit.record(
            context.client_info().as_ref(),
            peer(&context),
            "prompts/get",
            Some(&name),
            None,
            &Value::Object(args),
            got.is_ok(),
            started.elapsed(),
        );
        got.map(GetPromptResponse::from)
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

/// The shared schema object of a tool.
pub(crate) fn schema(v: Value) -> Arc<JsonObject> {
    Arc::new(v.as_object().expect("a schema is an object").clone())
}

/// The arguments of `tool` as an object that has every key of `required` and no key outside
/// `required` and `optional`; anything else is a call the schema forbids (`-32602`).
pub(crate) fn arguments<'a>(
    tool: &str,
    args: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>, ErrorData> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    let map = match args {
        Value::Object(m) => m,
        Value::Null => EMPTY.get_or_init(Map::new),
        _ => return Err(ErrorData::invalid_params(format!("{tool} takes an object"), None)),
    };
    if let Some(k) = required.iter().find(|k| !map.contains_key(**k)) {
        return Err(ErrorData::invalid_params(format!("{tool} needs {k}"), None));
    }
    if let Some(k) = map.keys().find(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str())) {
        return Err(ErrorData::invalid_params(format!("{tool} has no argument {k:.40}"), None));
    }
    Ok(map)
}

/// The string at `key`, which `arguments` has already said is there if it is required.
pub(crate) fn text<'a>(tool: &str, map: &'a Map<String, Value>, key: &str) -> Result<&'a str, ErrorData> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(ErrorData::invalid_params(format!("{tool}: {key} must be a string"), None)),
    }
}

