//! The startup sequence (02 section 7) as ten ordered steps, and shutdown as the same steps
//! stopped in reverse. A step that fails stops the ones already started before the error goes up,
//! so a half-started session never leaves a runtime or a registered secret behind.
//!
//! Step 1 is where the command line, already parsed by clap in `main`, gets its prompt and its
//! cross-flag checks. What the later tasks fill in is named at the step that will host it: the
//! SQLite index (B2.4) at 3, hydrating a snapshot (B2.4) at 4, the hook runner and file watcher
//! (B1.3) at 7 and 9, the TUI at 8.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aibus_core::agent::discover::{discover, scope_dirs, Discovered};
use aibus_core::config::{self, Loaded};
use aibus_core::persist::fs::{create_private_dir, write_private};
use aibus_core::persist::paths::{project_dir, Paths};
use aibus_core::permission::bypass_allowed;
use aibus_core::{
    expand, AccountCredential, AccountName, AccountRegistry, AccountView, AgentName, AibusError,
    App, Bash, CredentialSource, CredentialStore, Gate, Msg, ModelDefaults, ModelId,
    PermissionMode, Redactor, RunConfig, RunDeps, RunHandle, RunResult, SelectHints, SessionId,
    ToolExecutor, ToolLimits, ToolPattern, ToolRegistry, MSG_CAPACITY,
};
use aibus_graph::mini::MiniGraph;
use aibus_provider::fake::{FakeProvider, Script, FAKE_KEY};
use aibus_provider::{
    AuthMethodKind, ConfigError, Credential, ModelRef, Provider, ProviderConfig, ProviderFactory,
    ProviderId, ProviderRegistry, Secret,
};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::agent_watch::{self, AgentWatch};
use crate::cli::{Global, RunArgs};
use crate::events;
use crate::keychain::OsKeychain;
use crate::providers::{builtin_factories, provider_configs, refresh_stale_models};
use crate::signals::Signals;

pub struct Session {
    pub id: SessionId,
    pub dir: PathBuf,
    started: String,
}

/// What step 6 settles: the accounts a session can pick from, and where their keys come from.
pub struct Accounts {
    pub registry: AccountRegistry,
    pub store: Arc<CredentialStore>,
    pub script_text: String,
    /// No account is configured, so the built-in fake stands in with its fixed key.
    pub builtin: bool,
}

/// Everything the steps hand to each other. A field is `None` until its step has run.
pub struct Ctx {
    pub global: Global,
    pub run: RunArgs,
    pub redactor: Redactor,
    pub prompt: String,
    pub cwd: PathBuf,
    pub paths: Option<Paths>,
    pub loaded: Option<Loaded>,
    pub session: Option<Session>,
    pub app: Option<App<MiniGraph>>,
    pub agents: Option<Discovered>,
    pub accounts: Option<Accounts>,
    pub rt: Option<Runtime>,
    pub signals: Option<Signals>,
    pub agent_watch: Option<AgentWatch>,
    pub plan: Option<(RunConfig, RunDeps)>,
    pub rx: Option<mpsc::Receiver<Msg>>,
    pub handle: Option<RunHandle>,
    pub finished: Option<RunResult>,
    pub interrupted: bool,
    /// `start <name>` and `stop <name>`, in the order they happened.
    pub trace: Vec<String>,
}

impl Ctx {
    pub fn new(global: Global, run: RunArgs, redactor: Redactor) -> Self {
        Self {
            global,
            run,
            redactor,
            prompt: String::new(),
            cwd: PathBuf::new(),
            paths: None,
            loaded: None,
            session: None,
            app: None,
            agents: None,
            accounts: None,
            rt: None,
            signals: None,
            agent_watch: None,
            plan: None,
            rx: None,
            handle: None,
            finished: None,
            interrupted: false,
            trace: Vec::new(),
        }
    }
}

pub trait Step {
    fn name(&self) -> &'static str;
    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError>;
    fn stop(&mut self, _ctx: &mut Ctx) -> Result<(), AibusError> {
        Ok(())
    }
}

/// A step ran before one it depends on. A bug in the list below, not a user error.
pub fn not_ready(what: &str) -> AibusError {
    std::io::Error::other(format!("startup order: {what} is not ready")).into()
}

/// The ten steps of 02 section 7, in order.
pub fn steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(ParseCli),
        Box::new(LoadConfig),
        Box::new(OpenSession),
        Box::new(BuildGraph),
        Box::new(DiscoverAgents),
        Box::new(LoadAccounts),
        Box::new(StartRuntime),
        Box::new(EnterLoop),
        Box::new(StartHooks),
        Box::new(SubmitPrompt),
    ]
}

/// Starts the steps in order. On a failure the steps already started are stopped in reverse and
/// the error is returned; on success the started steps come back for `stop`.
pub fn start(
    steps: Vec<Box<dyn Step>>,
    ctx: &mut Ctx,
) -> Result<Vec<Box<dyn Step>>, AibusError> {
    let mut started: Vec<Box<dyn Step>> = Vec::new();
    for mut step in steps {
        ctx.trace.push(format!("start {}", step.name()));
        match step.start(ctx) {
            Ok(()) => started.push(step),
            Err(e) => {
                // The failed step may have got part of the way, so it is stopped as well.
                started.push(step);
                let _ = stop(started, ctx);
                return Err(e);
            }
        }
    }
    Ok(started)
}

/// Stops in reverse. Every step gets its turn even if an earlier one failed; the first error is
/// the one returned.
pub fn stop(mut started: Vec<Box<dyn Step>>, ctx: &mut Ctx) -> Result<(), AibusError> {
    let mut first = None;
    while let Some(mut step) = started.pop() {
        ctx.trace.push(format!("stop {}", step.name()));
        if let Err(e) = step.stop(ctx) {
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

// 1 ---------------------------------------------------------------------------------------

struct ParseCli;

impl Step for ParseCli {
    fn name(&self) -> &'static str {
        "parse-cli"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let usage = |m: &str| AibusError::Usage(m.to_owned());
        if ctx.run.workflow.is_some() {
            return Err(usage("--workflow: not yet"));
        }
        if ctx.global.permission_mode.as_deref() == Some("bypass")
            && std::env::var("AIBUS_I_KNOW").as_deref() != Ok("1")
        {
            return Err(usage("bypass also needs AIBUS_I_KNOW=1 in the environment"));
        }
        let g = &ctx.global;
        if g.trace.is_some() {
            eprintln!("aibus: warning: --trace: not yet, ignored");
        }
        ctx.cwd = g.project_dir()?;
        ctx.prompt = match ctx.run.prompt.as_deref() {
            Some(p) if p != "-" => p.to_owned(),
            None if std::io::stdin().is_terminal() => String::new(),
            _ => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text.trim_end_matches(['\n', '\r']).to_owned()
            }
        };
        if ctx.prompt.trim().is_empty() {
            return Err(usage(
                "aibus run [OPTIONS] <PROMPT>  (or pipe the prompt on stdin)",
            ));
        }
        Ok(())
    }
}

// 2 ---------------------------------------------------------------------------------------

struct LoadConfig;

impl Step for LoadConfig {
    fn name(&self) -> &'static str {
        "config"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let paths = ctx.global.paths()?;
        let project = project_dir(&ctx.cwd).join("settings.toml");
        let loaded = config::load(&paths.config_file(), Some(&project))?;
        for w in &loaded.warnings {
            eprintln!("aibus: warning: {w}");
        }

        // `-v`, `-vv`, `-q` pick the level; the file is private and everything written to it
        // passes the redactor. Never the terminal. Rotation (7 files kept) comes with the
        // persistence task.
        let level = match (ctx.global.quiet, ctx.global.verbose) {
            (true, _) => LevelFilter::ERROR,
            (_, 0) => LevelFilter::WARN,
            (_, 1) => LevelFilter::INFO,
            (_, 2) => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        };
        let dir = paths.state.join("logs");
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match create_private_dir(&dir).and_then(|()| opts.open(dir.join("aibus.log"))) {
            Ok(f) => {
                let layer = aibus_core::redact::layer(ctx.redactor.clone(), Arc::new(f))
                    .with_filter(level);
                // A second call in one process (the tests) keeps the first subscriber.
                let _ = tracing::subscriber::set_global_default(
                    tracing_subscriber::registry().with(layer),
                );
            }
            Err(e) => eprintln!("aibus: warning: no log file in {}: {e}", dir.display()),
        }
        ctx.paths = Some(paths);
        ctx.loaded = Some(loaded);
        Ok(())
    }
}

// 3 ---------------------------------------------------------------------------------------

struct OpenSession;

impl OpenSession {
    fn write(ctx: &Ctx, outcome: Option<&str>) -> Result<(), AibusError> {
        let s = ctx.session.as_ref().ok_or_else(|| not_ready("session"))?;
        let json = serde_json::json!({
            "schema_version": 1,
            "id": s.id.to_string(),
            "started": s.started,
            "topic": ctx.global.topic,
            "cwd": ctx.cwd,
            "lead": ctx.global.agent.as_deref().unwrap_or("lead"),
            "outcome": outcome,
        });
        write_private(
            s.dir.join("session.json"),
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        )?;
        Ok(())
    }
}

impl Step for OpenSession {
    fn name(&self) -> &'static str {
        "session"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let paths = ctx.paths.as_ref().ok_or_else(|| not_ready("paths"))?;
        let id = SessionId::new();
        let dir = ctx
            .run
            .session_dir
            .clone()
            .unwrap_or_else(|| paths.state.join("sessions").join(id.to_string()));
        create_private_dir(&dir.join("runs"))?;
        ctx.session = Some(Session {
            id,
            dir,
            started: events::rfc3339(SystemTime::now()),
        });
        Self::write(ctx, None)
    }

    fn stop(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        if ctx.session.is_none() {
            return Ok(());
        }
        let outcome = match (&ctx.finished, ctx.interrupted) {
            (Some(r), _) => r.status.as_value(),
            (None, true) => "interrupted".to_owned(),
            (None, false) => "unknown".to_owned(),
        };
        Self::write(ctx, Some(&outcome))
    }
}

// 4 ---------------------------------------------------------------------------------------

struct BuildGraph;

impl Step for BuildGraph {
    fn name(&self) -> &'static str {
        "graph"
    }

    /// `App::new` registers every static node. The effects sink stays idle on purpose: the run
    /// task executes its own tool calls and also sends `ToolCallRequested`, which the reducer
    /// turns into an approved `Effect::Tool`; a sink that executed it would run every tool twice.
    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        ctx.app = Some(App::new(MiniGraph::new(), now_ms));
        Ok(())
    }
}

// 5 ---------------------------------------------------------------------------------------

struct DiscoverAgents;

impl Step for DiscoverAgents {
    fn name(&self) -> &'static str {
        "agents"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let paths = ctx.paths.as_ref().ok_or_else(|| not_ready("paths"))?;
        let cfg = &ctx.loaded.as_ref().ok_or_else(|| not_ready("config"))?.config;
        let dirs = |v: &[String]| -> Vec<PathBuf> { v.iter().map(|d| ctx.cwd.join(d)).collect() };
        let found = discover(
            ctx.global.agents.as_deref(),
            &ctx.cwd,
            Some(&paths.config.join("agents")),
            &dirs(&cfg.agents.extra_dirs),
            &dirs(&cfg.agents.trusted_dirs),
        )
        .map_err(|e| AibusError::Usage(e.to_string()))?;
        for w in &found.warnings {
            eprintln!("aibus: warning: {w}");
        }
        let app = ctx.app.as_mut().ok_or_else(|| not_ready("graph"))?;
        app.step(Msg::AgentsReloaded {
            defs: found.agents.values().cloned().collect(),
            warnings: found.warnings.clone(),
        })?;
        ctx.agents = Some(found);
        Ok(())
    }
}

// 6 ---------------------------------------------------------------------------------------

struct LoadAccounts;

/// Hands out the one scripted fake the session was started with, whatever its config.
struct Scripted(Arc<dyn Provider>);

impl ProviderFactory for Scripted {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }

    fn build(&self, _: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(self.0.clone())
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey, AuthMethodKind::None]
    }
}

impl Step for LoadAccounts {
    fn name(&self) -> &'static str {
        "accounts"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let cfg = &ctx.loaded.as_ref().ok_or_else(|| not_ready("config"))?.config;

        let script_text = match &ctx.run.script {
            Some(p) => std::fs::read_to_string(p)?,
            None => include_str!("../../../fixtures/scripts/explore.ron").to_owned(),
        };
        let script = Script::from_ron(&script_text)
            .map_err(|e| AibusError::Usage(format!("script: {e}")))?;
        let mut factories: Vec<Box<dyn ProviderFactory>> =
            vec![Box::new(Scripted(Arc::new(FakeProvider::new(script))))];
        factories.extend(builtin_factories().into_iter().filter(|f| f.id().0 != "fake"));
        let paths = ctx.paths.as_ref().ok_or_else(|| not_ready("paths"))?;
        let configs = provider_configs(&cfg.accounts, paths)?;
        let providers = Arc::new(ProviderRegistry::new(&factories, &configs)?);
        let store = Arc::new(CredentialStore::new(
            Arc::new(OsKeychain),
            ctx.redactor.clone(),
        ));
        let builtin = cfg.accounts.is_empty();
        if !builtin {
            refresh_stale_models(&cfg.accounts, providers.clone(), store.clone(), paths.cache.join("models"));
        }

        let mut effective = cfg.clone();
        if builtin {
            effective.accounts.push(config::Account {
                name: "fake".into(),
                provider: "fake".into(),
                auth: config::Auth {
                    none: true,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let views = effective
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .map(|a| AccountView {
                name: a.account_name(),
                provider: a.provider.as_str().into(),
                label: a.label.clone().unwrap_or_else(|| a.name.clone()),
                default_model: a.default_model.as_deref().map(|m| ModelId(m.into())),
                key_ref: a.key_ref().map_or_else(|| "none".into(), |k| k.to_string()),
            })
            .collect();
        ctx.accounts = Some(Accounts {
            registry: AccountRegistry::new(&effective, providers),
            store,
            script_text,
            builtin,
        });
        let app = ctx.app.as_mut().ok_or_else(|| not_ready("graph"))?;
        app.step(Msg::AccountsReloaded { accounts: views })
    }

    fn stop(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        if ctx.accounts.as_ref().is_some_and(|a| a.builtin) {
            ctx.redactor.unregister(&AccountName("fake".into()));
        }
        Ok(())
    }
}

// 7 ---------------------------------------------------------------------------------------

/// The tokio runtime and the signal handlers. The hook runner starts here once it exists (B1.3);
/// the watcher on the agent directories starts with the loop, which owns the channel it feeds.
struct StartRuntime;

impl Step for StartRuntime {
    fn name(&self) -> &'static str {
        "runtime"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        // Registered now, so a signal during steps 8 to 10 is queued rather than killing us.
        ctx.signals = Some(Signals::new()?);
        ctx.rt = Some(rt);
        Ok(())
    }

    fn stop(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        ctx.signals = None;
        if let Some(rt) = ctx.rt.take() {
            rt.shutdown_timeout(Duration::from_secs(1));
        }
        Ok(())
    }
}

// 8 ---------------------------------------------------------------------------------------

/// Headless is the only loop so far (`aibus run`); the TUI loop, with raw mode and the alternate
/// screen, is entered here when it is wired to a terminal. This step settles the lead run.
struct EnterLoop;

impl Step for EnterLoop {
    fn name(&self) -> &'static str {
        "loop"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let cfg = &ctx.loaded.as_ref().ok_or_else(|| not_ready("config"))?.config;
        let acc = ctx.accounts.as_ref().ok_or_else(|| not_ready("accounts"))?;
        let agents = ctx.agents.as_ref().ok_or_else(|| not_ready("agents"))?;
        let session = ctx.session.as_ref().ok_or_else(|| not_ready("session"))?;
        let paths = ctx.paths.as_ref().ok_or_else(|| not_ready("paths"))?;

        let name: AgentName = ctx
            .global
            .agent
            .as_deref()
            .unwrap_or("lead")
            .parse()
            .map_err(|e: aibus_core::InvalidAgentName| AibusError::Usage(e.to_string()))?;
        let mut def = agents
            .agents
            .get(&name)
            .ok_or_else(|| AibusError::Usage(format!("no agent named {name}")))?
            .clone();

        // `--permission-mode` overrides the definition; `bypass` from either needs the variable.
        if let Some(mode) = ctx.global.permission_mode.as_deref() {
            def.permission_mode = parse_mode(mode)?;
        }
        let i_know = std::env::var("AIBUS_I_KNOW").ok();
        let bypass = bypass_allowed(def.permission_mode, i_know.as_deref())?;
        let pattern = |flag: &str, text: &str| {
            text.parse::<ToolPattern>()
                .map_err(|()| AibusError::Usage(format!("{flag} {text}: want tool or tool(specifier)")))
        };
        for text in &ctx.global.deny {
            def.tools.deny.push(pattern("--deny", text)?);
        }
        let mut pre_approved = Vec::new();
        for text in &cfg.tools.allow {
            pre_approved.push(pattern("tools.allow", text)?);
        }
        for text in &ctx.global.allow {
            pre_approved.push(pattern("--allow", text)?);
        }

        let session_default = acc.registry.model_ref(
            ctx.global
                .model
                .as_deref()
                .or(cfg.defaults.model.as_deref())
                .unwrap_or("fake-small"),
        );
        let pick = |cli: &Option<String>, conf: &Option<String>| match cli.as_ref().or(conf.as_ref())
        {
            Some(s) if s == "off" => None,
            Some(s) => Some(acc.registry.model_ref(s)),
            None => Some(session_default.clone()),
        };
        let defaults = ModelDefaults {
            default_model: Some(session_default.clone()),
            advisor_model: pick(&ctx.global.advisor, &cfg.defaults.advisor_model),
            router_model: pick(&ctx.global.router, &cfg.defaults.router_model),
            swarm_model: pick(&None, &cfg.defaults.swarm_model),
        };

        // The roles that are not the lead only warn: nothing runs them yet.
        for (role, cli, conf, picked) in [
            ("advisor", &ctx.global.advisor, &cfg.defaults.advisor_model, &defaults.advisor_model),
            ("router", &ctx.global.router, &cfg.defaults.router_model, &defaults.router_model),
        ] {
            if let (Some(model), true) = (picked, cli.is_some() || conf.is_some()) {
                if let Err(e) = acc.registry.select(model, &SelectHints::default()) {
                    eprintln!("aibus: warning: {role} model: {e}");
                }
            }
        }

        // The lead: `--account`, then the definition's `account`, then the chain (04 section 4.1).
        // `inherit` has no parent here, so it means the session's model.
        let target = match expand(&def.model, &defaults) {
            ModelRef::Inherit => session_default.clone(),
            r => r,
        };
        let hints = SelectHints {
            explicit: ctx.global.account.as_deref().map(|a| AccountName(a.into())),
            agent: def.account.clone(),
            ..SelectHints::default()
        };
        let (account, model) = acc.registry.select(&target, &hints)?;
        let provider = acc.registry.provider(&account)?;
        let creds: Arc<dyn CredentialSource> = if acc.builtin {
            ctx.redactor.register(&account, FAKE_KEY);
            Arc::new(Credential {
                account: account.clone(),
                secret: Secret::new(FAKE_KEY.to_owned()),
                kind: AuthMethodKind::None,
                resolved_at: Instant::now(),
            })
        } else {
            let configured = acc
                .registry
                .account(&account)
                .cloned()
                .ok_or_else(|| not_ready("account"))?;
            Arc::new(AccountCredential {
                store: acc.store.clone(),
                account: configured,
            })
        };
        // Fail now, with exit 4, rather than on the first request.
        creds.credential()?;

        let (tx, rx) = mpsc::channel(MSG_CAPACITY);
        let run_cfg = RunConfig {
            run: aibus_core::RunId::new(),
            def,
            prompt: ctx.prompt.clone(),
            parent: None,
            depth: 0,
            model,
            account,
            cwd: ctx.cwd.clone(),
            runs_dir: session.dir.join("runs"),
            limits: ToolLimits {
                result_max_bytes: cfg.limits.result_max_bytes,
                bash_max_output_bytes: cfg.tools.bash_max_output_bytes,
            },
            date: events::date(SystemTime::now()),
            data_home: Some(paths.data.clone()),
        };
        // Nobody can answer a prompt in `aibus run` (the `--json` control channel is not wired
        // yet), so `default` mode refuses what would ask, with `PermissionNeeded` then
        // `PermissionDecided { denied, "no tty" }`.
        let bash = Arc::new(Bash::new(
            cfg.tools.bash_timeout_secs,
            Vec::new(),
            std::env::vars().collect(),
            ctx.cwd.clone(),
            session.id.to_string(),
        ));
        let gate = Gate {
            allow: pre_approved,
            deny_patterns: cfg.tools.deny_patterns.clone(),
            tty: false,
            redactor: Some(ctx.redactor.clone()),
            home: std::env::var_os("HOME").map(PathBuf::from),
        };
        let tools = ToolExecutor::with_gate(ToolRegistry::standard(bash), gate)
            .map_err(|e| AibusError::Usage(format!("tools.deny_patterns: {e}")))?;
        let deps = RunDeps {
            provider,
            creds,
            tools: Arc::new(tools),
            tx: tx.clone(),
        };
        ctx.plan = Some((run_cfg, deps));
        ctx.rx = Some(rx);
        if bypass {
            if let Some(app) = ctx.app.as_mut() {
                app.enable_bypass()?;
            }
        }

        if cfg.agents.reload_on_change {
            let (cli_json, cwd) = (ctx.global.agents.clone(), ctx.cwd.clone());
            let user_dir = paths.config.join("agents");
            let dirs = |v: &[String]| -> Vec<PathBuf> { v.iter().map(|d| cwd.join(d)).collect() };
            let (extra, trusted) = (dirs(&cfg.agents.extra_dirs), dirs(&cfg.agents.trusted_dirs));
            let watched: Vec<PathBuf> = scope_dirs(&cwd, Some(&user_dir), &extra)
                .into_iter()
                .map(|(_, d)| d)
                .collect();
            let reload = move || {
                let found = discover(cli_json.as_deref(), &cwd, Some(&user_dir), &extra, &trusted).ok()?;
                Some(Msg::AgentsReloaded {
                    defs: found.agents.values().cloned().collect(),
                    warnings: found.warnings,
                })
            };
            match agent_watch::start(&watched, &tx, reload) {
                Ok(w) => ctx.agent_watch = Some(w),
                Err(e) => eprintln!("aibus: warning: not watching agent directories: {e}"),
            }
        }
        Ok(())
    }

    fn stop(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        ctx.agent_watch = None;
        Ok(())
    }
}

// 9 ---------------------------------------------------------------------------------------

/// `session_start` hooks and the latest handoff arrive with the hook runner (B1.3) and the
/// continuity files (B2.x). Until then the step says what it is skipping.
struct StartHooks;

impl Step for StartHooks {
    fn name(&self) -> &'static str {
        "hooks"
    }

    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let cfg = &ctx.loaded.as_ref().ok_or_else(|| not_ready("config"))?.config;
        let n: usize = cfg.hooks.values().map(Vec::len).sum();
        if ctx.global.no_hooks {
            eprintln!("aibus: warning: --no-hooks: no hooks will run");
        } else if n > 0 {
            eprintln!("aibus: warning: {n} hooks configured, the hook runner is not wired yet");
        }
        Ok(())
    }
}

// 10 --------------------------------------------------------------------------------------

struct SubmitPrompt;

impl Step for SubmitPrompt {
    fn name(&self) -> &'static str {
        "prompt"
    }

    /// The lead run starts with the prompt already in its config.
    fn start(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        let rt = ctx.rt.as_ref().ok_or_else(|| not_ready("runtime"))?;
        let (cfg, deps) = ctx.plan.take().ok_or_else(|| not_ready("loop"))?;
        let _in_runtime = rt.enter();
        ctx.handle = Some(aibus_core::spawn(cfg, deps));
        Ok(())
    }

    /// Cancels the run if it is still going and waits a moment for it to flush.
    fn stop(&mut self, ctx: &mut Ctx) -> Result<(), AibusError> {
        if let (Some(h), Some(rt)) = (ctx.handle.take(), ctx.rt.as_ref()) {
            h.cancel.cancel();
            // `drive` may already have joined it; a finished handle must not be polled again.
            if !h.join.is_finished() {
                let _ = rt
                    .block_on(async { tokio::time::timeout(Duration::from_secs(2), h.join).await });
            }
        }
        Ok(())
    }
}

/// The `--permission-mode` values, in the kebab case the flag documents or the snake case the
/// definitions use.
fn parse_mode(s: &str) -> Result<PermissionMode, AibusError> {
    Ok(match s.replace('-', "_").as_str() {
        "default" => PermissionMode::Default,
        "accept_edits" => PermissionMode::AcceptEdits,
        "dont_ask" => PermissionMode::DontAsk,
        "bypass" => PermissionMode::Bypass,
        "plan" => PermissionMode::Plan,
        _ => {
            return Err(AibusError::Usage(format!(
                "--permission-mode {s}: want default, accept-edits, dont-ask, bypass or plan"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn ctx() -> Ctx {
        Ctx::new(
            Global::default(),
            RunArgs {
                prompt: Some("hi".into()),
                workflow: None,
                outcome: None,
                script: None,
                record: None,
                session_dir: None,
            },
            Redactor::new(),
        )
    }

    struct Probe {
        name: &'static str,
        fail: bool,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Step for Probe {
        fn name(&self) -> &'static str {
            self.name
        }
        fn start(&mut self, _: &mut Ctx) -> Result<(), AibusError> {
            self.log.lock().unwrap().push(format!("up {}", self.name));
            if self.fail {
                Err(AibusError::Usage("boom".into()))
            } else {
                Ok(())
            }
        }
        fn stop(&mut self, _: &mut Ctx) -> Result<(), AibusError> {
            self.log.lock().unwrap().push(format!("down {}", self.name));
            Ok(())
        }
    }

    fn probes(fail_at: Option<usize>, log: &Arc<Mutex<Vec<String>>>) -> Vec<Box<dyn Step>> {
        ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Box::new(Probe {
                    name,
                    fail: fail_at == Some(i),
                    log: log.clone(),
                }) as Box<dyn Step>
            })
            .collect()
    }

    #[test]
    fn the_real_steps_are_the_ten_of_the_spec_in_order() {
        let names: Vec<_> = steps().iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [
                "parse-cli", "config", "session", "graph", "agents", "accounts", "runtime",
                "loop", "hooks", "prompt"
            ]
        );
    }

    #[test]
    fn steps_start_in_order_and_stop_in_reverse() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut c = ctx();
        let started = start(probes(None, &log), &mut c).unwrap();
        stop(started, &mut c).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["up a", "up b", "up c", "up d", "down d", "down c", "down b", "down a"]
        );
        assert_eq!(c.trace.len(), 8);
        assert_eq!(c.trace[0], "start a");
        assert_eq!(c.trace[7], "stop a");
    }

    #[test]
    fn a_failing_step_unwinds_the_ones_before_it_in_reverse() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut c = ctx();
        let err = start(probes(Some(2), &log), &mut c).err().unwrap();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(
            *log.lock().unwrap(),
            ["up a", "up b", "up c", "down c", "down b", "down a"]
        );
    }
}
