//! Builds the runs the orchestrator starts (01 section 4): the lead and every child get their
//! account, model and credential here, from the same chain (04 section 4.1).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

use aibus_core::config::{Config, Defaults};
use aibus_core::{
    expand, AccountCredential, AccountName, AccountRegistry, AdvisorSeat, AgentDef, AgentName, Fallback,
    AccountView, AibusError, Assist, Budgets, CredentialSource, CredentialStore, HookRunner, Isolation,
    Launcher, ModelCompactor, ModelDefaults, ModelId, ModelSwitch, ModelTarget, Redactor, RouterSeat, RunConfig, RunDeps,
    RunId, Seat, SelectHints, SpawnSpec, Start, ToolExecutor, ToolLimits, ToolPattern,
};
use aibus_provider::fake::FAKE_KEY;
use aibus_provider::{AuthMethodKind, Credential, ModelRef, Provider, Secret};
use tokio::sync::mpsc;

use crate::events;

/// What `pick` settled for one run.
pub struct Picked {
    pub account: AccountName,
    pub model: ModelId,
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<dyn CredentialSource>,
}

/// What a login in the terminal replaces as one unit. A pick reads all four from one snapshot, so
/// it can never pair the new registry with the fake's credential or the fake's default model.
#[derive(Clone)]
pub(crate) struct Live {
    /// The accounts a pick chooses from.
    pub registry: Arc<AccountRegistry>,
    /// No account is configured: the built-in fake stands in with its fixed key.
    pub builtin: bool,
    pub defaults: ModelDefaults,
    pub session_default: ModelRef,
}

/// Everything a child run is built from. The lead goes through `pick` too, so there is one place
/// that turns a model and an account into a provider and a credential.
pub struct ChildLauncher {
    /// The definitions a child can be started from; a reload replaces them.
    pub agents: RwLock<BTreeMap<AgentName, AgentDef>>,
    /// The accounts and the model defaults; a login or a logout in the terminal swaps them.
    pub(crate) live: RwLock<Live>,
    pub store: Arc<CredentialStore>,
    pub redactor: Redactor,
    pub tools: Arc<ToolExecutor>,
    pub hooks: Arc<HookRunner>,
    /// `--deny`, which holds for every run of the session.
    pub deny: Vec<ToolPattern>,
    pub cwd: PathBuf,
    pub runs_dir: PathBuf,
    pub limits: ToolLimits,
    pub budgets: Budgets,
    pub data_home: Option<PathBuf>,
    pub max_spawn_depth: u8,
    /// Each started run's account and model, so a child can inherit its parent's.
    pub chosen: Mutex<HashMap<RunId, (AccountName, ModelId)>>,
    /// The definition each started run got (with `--deny` applied), so a fork runs its parent's
    /// and a resume its own, even when the files were edited since.
    pub defs: Mutex<HashMap<RunId, AgentDef>>,
    /// `<cache>/worktrees/<session>`: where an isolated run's checkout goes (09 section 7).
    pub worktrees: PathBuf,
    /// The advisor, the router and the ledger every run of the session shares; set once at
    /// startup by `set_assist`.
    pub assist: OnceLock<Arc<Assist>>,
    /// Where workflow files are looked for (`.aibus/workflows`, then the user's).
    pub workflow_dirs: Vec<PathBuf>,
    /// The binary added `[mcp.aibus]` itself, for the workflow author: nobody else is offered it.
    pub author_server: bool,
    /// The next `workflow-author` run may write workflows: the owner started it with
    /// `/workflow new --write` or `/workflow edit`. Taken (and so cleared) when that run starts.
    pub author_write: AtomicBool,
}

/// What was remembered for `run`.
fn recall<T: Clone>(m: &Mutex<HashMap<RunId, T>>, run: RunId) -> Option<T> {
    m.lock().unwrap_or_else(PoisonError::into_inner).get(&run).cloned()
}

impl ChildLauncher {
    /// The accounts and defaults as they are now, as one snapshot.
    pub(crate) fn live(&self) -> Live {
        self.live.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The accounts as they are now.
    pub fn registry(&self) -> Arc<AccountRegistry> {
        self.live().registry
    }

    /// Swaps in the accounts of a reloaded config, so a login or a logout is usable by the next
    /// pick. A run that already started keeps its provider and credential.
    ///
    /// A session that started on the built-in fake leaves it here: the fake key and the fake's
    /// default model go with it, and the defaults are rebuilt from the config's `defaults.*`, else
    /// from the first account's default model, else from the first model it lists. When there is
    /// none to default to, nothing is swapped and the fake stays whole.
    pub fn set_accounts(
        &self,
        registry: Arc<AccountRegistry>,
        views: &[AccountView],
        cfg: &Defaults,
    ) -> Result<(), String> {
        let mut live = self.live.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = Live { registry: registry.clone(), ..live.clone() };
        if live.builtin {
            let first = views.iter().find_map(|v| v.default_model.clone().or_else(|| v.models.first().map(|m| m.id.clone())));
            let default = cfg
                .model
                .as_deref()
                .map(|m| registry.model_ref(m))
                .or(first.map(ModelRef::Full))
                .ok_or("the new account lists no model: set defaults.model")?;
            let side = |named: &Option<String>| match named.as_deref() {
                Some("off") => None,
                Some(m) => Some(registry.model_ref(m)),
                None => Some(default.clone()),
            };
            next.builtin = false;
            next.defaults = ModelDefaults {
                default_model: Some(default.clone()),
                advisor_model: side(&cfg.advisor_model),
                router_model: side(&cfg.router_model),
                swarm_model: side(&cfg.swarm_model),
            };
            next.session_default = default;
        }
        *live = next;
        Ok(())
    }

    /// One side role's seat: its definition, and the account and model it runs on, apart from the
    /// lead's. `None` (with a note on stderr when the user asked for it or has accounts) when it
    /// is turned off or no account serves its model (Q6). `fallback` is the alias used when
    /// nothing names a model, and `logprobs` prefers an account that reports them.
    fn side_seat(
        &self,
        role: &str,
        flag: Option<&str>,
        configured: Option<&str>,
        enabled: bool,
        fallback: &str,
        logprobs: bool,
    ) -> Option<(Seat, AgentDef)> {
        let note = |why: &str| {
            eprintln!("aibus: note: the {role} is off: {why}");
            None
        };
        if !enabled {
            return note(&format!("{role}.enabled is false"));
        }
        let named = flag.or(configured);
        if named == Some("off") {
            return note("it was turned off");
        }
        // A fresh install has no account to name: the built-in fake serves the lead, and nobody
        // else is consulted unless the user asks.
        if named.is_none() && self.live().builtin {
            return None;
        }
        let name = role.parse::<AgentName>().ok()?;
        let def = self.agents.read().unwrap_or_else(PoisonError::into_inner).get(&name).cloned();
        let Some(def) = def else {
            return note("no definition of that name");
        };
        let reserved = format!("{role}_model");
        let target = match &def.model {
            ModelRef::Alias(a) if *a == *reserved => {
                self.registry().model_ref(named.unwrap_or(fallback))
            }
            other => expand(other, &self.live().defaults),
        };
        let hints = SelectHints {
            agent: def.account.clone(),
            logprobs,
            ..SelectHints::default()
        };
        match self.pick(&target, &hints) {
            Ok(p) => {
                let fallback = self.fallback_for(&p.account, &target, def.fallback.as_deref());
                Some((
                Seat {
                    provider: p.provider,
                    creds: p.creds,
                    model: p.model,
                    account: p.account,
                    fallback,
                },
                def,
            ))
            }
            Err(e) => note(&e.to_string()),
        }
    }

    /// Builds the session's `Assist` from the flags and `[advisor]` / `[router]`, and keeps it
    /// for every run launched from here on. Returns it for the lead.
    pub fn set_assist(
        &self,
        advisor_flag: Option<&str>,
        router_flag: Option<&str>,
        cfg: &Config,
    ) -> Option<Arc<Assist>> {
        let advisor = self
            .side_seat(
                "advisor",
                advisor_flag,
                cfg.defaults.advisor_model.as_deref(),
                cfg.advisor.enabled,
                "opus",
                false,
            )
            .map(|(seat, def)| AdvisorSeat {
                seat,
                def,
                enabled: cfg.advisor.checkpoints.clone(),
                packet_max_tokens: cfg.advisor.packet_max_tokens,
            });
        let router = self
            .side_seat(
                "router",
                router_flag,
                cfg.defaults.router_model.as_deref(),
                cfg.router.enabled,
                "fast",
                true,
            )
            .map(|(seat, def)| RouterSeat {
                seat,
                system: def.system_prompt,
                threshold: cfg.router.threshold as f32,
                max_per_turn: cfg.router.max_per_turn,
                label: cfg.router.label.clone(),
            });
        if advisor.is_none() && router.is_none() {
            return None;
        }
        let assist = Assist::new(advisor, router);
        // Set once at startup; a second call keeps the first.
        let _ = self.assist.set(assist.clone());
        Some(assist)
    }

    /// The account for `target` under `hints`, its provider, and a credential that works (so a
    /// missing key fails now, with exit 4, rather than on the first request).
    pub fn pick(&self, target: &ModelRef, hints: &SelectHints) -> Result<Picked, AibusError> {
        let live = self.live();
        let (account, model) = live.registry.select(target, hints)?;
        let provider = live.registry.provider(&account)?;
        let creds: Arc<dyn CredentialSource> = if live.builtin {
            self.redactor.register(&account, FAKE_KEY);
            Arc::new(Credential {
                account: account.clone(),
                secret: Secret::new(FAKE_KEY.to_owned()),
                kind: AuthMethodKind::None,
                resolved_at: Instant::now(),
            })
        } else {
            let configured = live.registry.account(&account).cloned().ok_or_else(|| {
                aibus_core::CredentialError::UnknownAccount(account.clone())
            })?;
            Arc::new(AccountCredential {
                store: self.store.clone(),
                account: configured,
            })
        };
        creds.credential()?;
        Ok(Picked {
            account,
            model,
            provider,
            creds,
        })
    }

    /// The accounts a run on `from` moves to when it fails before its first token (F9): the
    /// definition's `fallback` over the account's, each one picked for `model` like any other
    /// account, so a key that is missing or a model that does not resolve there leaves it out
    /// with a log line. `None` when nothing is left to move to.
    pub fn fallback_for(&self, from: &AccountName, model: &ModelRef, pinned: Option<&[AccountName]>) -> Option<Arc<Fallback>> {
        let registry = self.registry();
        let chain: Vec<ModelSwitch> = registry
            .fallback_names(from, pinned)
            .into_iter()
            .filter_map(|name| {
                let hints = SelectHints { explicit: Some(name.clone()), ..SelectHints::default() };
                match self.pick(model, &hints) {
                    Ok(p) => Some(ModelSwitch { provider: p.provider, creds: p.creds, model: p.model, account: p.account }),
                    Err(e) => {
                        tracing::info!("fallback account {name} skipped: {e}");
                        None
                    }
                }
            })
            .collect();
        (!chain.is_empty()).then(|| Arc::new(Fallback { chain, on: registry.fallback_on().to_vec(), accounts: registry }))
    }

    /// `/workflow <name> <input>` or `/<name> <input>` for a workflow that exists: the `workflow`
    /// call a lead opens with in place of its first request (01 section 8.4).
    pub fn slash_workflow(&self, prompt: &str) -> Option<(String, serde_json::Value)> {
        let known: Vec<String> =
            aibus_core::workflow::list(&self.workflow_dirs).into_iter().map(|found| found.name).collect();
        let (name, input) = aibus_core::workflow::parse_slash(prompt, &known)?;
        Some(("workflow".to_owned(), serde_json::json!({"name": name, "input": input})))
    }

    /// Swaps in reloaded definitions.
    pub fn reload(&self, defs: &[AgentDef]) {
        *self.agents.write().unwrap_or_else(PoisonError::into_inner) =
            defs.iter().map(|d| (d.name.clone(), d.clone())).collect();
    }

    /// Records what `run` runs on, for the children it will have, and what it is defined as.
    pub fn remember(&self, run: RunId, account: &AccountName, model: &ModelId, def: &AgentDef) {
        self.chosen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run, (account.clone(), model.clone()));
        self.defs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run, def.clone());
    }
}

impl Launcher for ChildLauncher {
    fn switch_model(&self, target: &ModelTarget, model: &str) -> Result<Option<ModelSwitch>, String> {
        let role = match target {
            ModelTarget::Advisor => "advisor",
            ModelTarget::Router => "router",
            ModelTarget::Run(_) | ModelTarget::Lead => "",
        };
        let seat_assist = || {
            let assist = self.assist.get().ok_or_else(|| format!("the {role} is off in this session"))?;
            let configured = if *target == ModelTarget::Advisor { assist.advisor.is_some() } else { assist.router.is_some() };
            // A seat that started off has no definition to speak from.
            configured.then_some(assist).ok_or_else(|| format!("the {role} was off at startup"))
        };
        if model == "off" {
            seat_assist()?.switch(*target, None);
            return Ok(None);
        }
        // The picker names the row it was clicked on as `account/model`; a prefix that is not an
        // account (`meta-llama/...`) is part of the model id.
        let (pinned, model) = match model.split_once('/') {
            Some((acct, rest)) if self.registry().account(&AccountName(acct.into())).is_some() => {
                (Some(AccountName(acct.into())), rest)
            }
            _ => (None, model),
        };
        let reference = self.registry().model_ref(model);
        let def = match target {
            ModelTarget::Run(run) => recall(&self.defs, *run).ok_or_else(|| format!("run {run} is not known"))?,
            ModelTarget::Advisor | ModelTarget::Router => {
                seat_assist()?;
                let name = role.parse::<AgentName>().map_err(|e| e.to_string())?;
                let agents = self.agents.read().unwrap_or_else(PoisonError::into_inner);
                agents.get(&name).cloned().ok_or_else(|| format!("no definition of the {role}"))?
            }
            ModelTarget::Lead => return Err("the lead is switched through its run".into()),
        };
        let hints = SelectHints { agent: def.account.clone(), explicit: pinned, logprobs: *target == ModelTarget::Router, ..SelectHints::default() };
        let p = self.pick(&reference, &hints).map_err(|e| e.to_string())?;
        let switch = ModelSwitch { provider: p.provider, creds: p.creds, model: p.model, account: p.account };
        match target {
            ModelTarget::Run(run) => self.remember(*run, &switch.account, &switch.model, &def),
            _ => seat_assist()?.switch(
                *target,
                Some(Seat {
                    provider: switch.provider.clone(),
                    creds: switch.creds.clone(),
                    model: switch.model.clone(),
                    account: switch.account.clone(),
                    fallback: self.fallback_for(&switch.account, &reference, def.fallback.as_deref()),
                }),
            ),
        }
        Ok(Some(switch))
    }

    fn launch(
        &self,
        spec: &SpawnSpec,
        prompt: String,
        tx: mpsc::Sender<aibus_core::Msg>,
    ) -> Result<(RunConfig, RunDeps), String> {
        // A fork is its parent over again, a resume is the run again; anything else starts from
        // the definition by name.
        let again = if spec.fork { spec.parent } else { spec.resume.then_some(spec.run) };
        let mut def: AgentDef = match again.and_then(|r| recall(&self.defs, r)) {
            Some(def) => def,
            None => {
                let mut def = self
                    .agents
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&spec.agent)
                    .cloned()
                    .ok_or_else(|| format!("no agent named {}", spec.agent))?;
                def.tools.deny.extend(self.deny.iter().cloned());
                let deny = |tool: &str| ToolPattern { tool: tool.into(), spec: None };
                if def.name.as_str() != "workflow-author" {
                    // The write child belongs to the author's one run, and the read-only child
                    // (when the binary started it) to the author alone.
                    def.tools.deny.push(deny("mcp:aibus-write:*"));
                    if self.author_server {
                        def.tools.deny.push(deny("mcp:aibus:*"));
                    }
                } else if self.author_write.swap(false, Ordering::SeqCst) {
                    // This run may write: the write child (which lists the read tools too)
                    // replaces the read-only one, so the author sees one set of tools.
                    if let Some(allow) = def.tools.allow.as_mut() {
                        allow.retain(|p| p.tool != "mcp:aibus:*");
                        allow.push(deny("mcp:aibus-write:*"));
                    }
                }
                def
            }
        };
        // The author gets a quarter of the session's cost cap unless its definition names one.
        if def.name.as_str() == "workflow-author" && def.max_cost_usd.is_none() {
            def.max_cost_usd = Some(self.budgets.max_cost_usd / 4.0);
        }
        if spec.fork {
            // The same agent as the parent, so it is not offered what only a definition's own
            // name could reach.
            def.name = spec.agent.clone();
        }

        let live = self.live();
        let parent = spec.parent.and_then(|p| recall(&self.chosen, p));
        let same = again.and_then(|r| recall(&self.chosen, r));
        // The caller's model, else the definition's. `inherit` takes the parent's, or the
        // session's when the parent is unknown. A fork and a resume keep the model and the
        // account of the run they continue.
        let (target, hints) = match same {
            Some((account, model)) => (
                ModelRef::Full(model),
                SelectHints {
                    explicit: Some(account),
                    ..SelectHints::default()
                },
            ),
            None => (
                match (&spec.model, expand(&def.model, &live.defaults), &parent) {
                    (Some(m), _, _) => live.registry.model_ref(m),
                    (None, ModelRef::Inherit, None) => live.session_default.clone(),
                    (None, other, _) => other,
                },
                SelectHints {
                    explicit: spec.account.clone(),
                    agent: def.account.clone(),
                    inherit: parent,
                    ..SelectHints::default()
                },
            ),
        };
        let picked = self.pick(&target, &hints).map_err(|e| e.to_string())?;
        self.remember(spec.run, &picked.account, &picked.model, &def);
        let fallback = self.fallback_for(&picked.account, &target, def.fallback.as_deref());

        // `isolation: worktree` from the call or the definition (09 section 7). A resumed run
        // finds its kept checkout where it left it.
        let (cwd, worktree) = if spec.isolation == Isolation::Worktree
            || def.isolation == Isolation::Worktree
        {
            let at = self.worktrees.join(spec.run.to_string());
            let info = aibus_core::worktree::create(&self.cwd, &at)?;
            (info.cwd.clone(), Some(info))
        } else {
            (self.cwd.clone(), None)
        };
        // The TUI starts its lead here, with whatever the first prompt was.
        let forced = (def.role == aibus_core::Role::Lead && spec.parent.is_none() && !spec.fork && !spec.resume)
            .then(|| self.slash_workflow(&prompt))
            .flatten();
        let compactor: Arc<dyn aibus_core::Compactor> = Arc::new(ModelCompactor {
            provider: picked.provider.clone(),
            creds: picked.creds.clone(),
            model: picked.model.clone(),
            handoff: None,
        });

        Ok((
            RunConfig {
                run: spec.run,
                def,
                prompt,
                parent: spec.parent,
                depth: spec.depth,
                model: picked.model,
                account: picked.account,
                cwd,
                runs_dir: self.runs_dir.clone(),
                limits: self.limits,
                budgets: self.budgets,
                date: events::date(SystemTime::now()),
                data_home: self.data_home.clone(),
                context: Vec::new(),
                background: spec.background,
                max_spawn_depth: self.max_spawn_depth,
                forced: forced.map(|(tool, input)| (tool.into(), input)),
                start: Start::New,
                worktree,
            },
            RunDeps {
                provider: picked.provider,
                creds: picked.creds,
                tools: self.tools.clone(),
                tx,
                hooks: self.hooks.clone(),
                compactor: Some(compactor),
                assist: self.assist.get().cloned(),
                fallback,
            },
        ))
    }
}
