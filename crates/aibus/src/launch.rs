//! Builds the runs the orchestrator starts (01 section 4): the lead and every child get their
//! account, model and credential here, from the same chain (04 section 4.1).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

use aibus_core::config::Config;
use aibus_core::{
    expand, AccountCredential, AccountName, AccountRegistry, AdvisorSeat, AgentDef, AgentName,
    AibusError, Assist, Budgets, CredentialSource, CredentialStore, HookRunner, Isolation,
    Launcher, ModelDefaults, ModelId, Redactor, RouterSeat, RunConfig, RunDeps, RunId, Seat,
    SelectHints, SpawnSpec, ToolExecutor, ToolLimits, ToolPattern,
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

/// Everything a child run is built from. The lead goes through `pick` too, so there is one place
/// that turns a model and an account into a provider and a credential.
pub struct ChildLauncher {
    /// The definitions a child can be started from; a reload replaces them.
    pub agents: RwLock<BTreeMap<AgentName, AgentDef>>,
    pub registry: Arc<AccountRegistry>,
    pub store: Arc<CredentialStore>,
    /// No account is configured: the built-in fake stands in with its fixed key.
    pub builtin: bool,
    pub redactor: Redactor,
    pub defaults: ModelDefaults,
    pub session_default: ModelRef,
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
    /// The advisor, the router and the ledger every run of the session shares; set once at
    /// startup by `set_assist`.
    pub assist: OnceLock<Arc<Assist>>,
}

impl ChildLauncher {
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
        if named.is_none() && self.builtin {
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
                self.registry.model_ref(named.unwrap_or(fallback))
            }
            other => expand(other, &self.defaults),
        };
        let hints = SelectHints {
            agent: def.account.clone(),
            logprobs,
            ..SelectHints::default()
        };
        match self.pick(&target, &hints) {
            Ok(p) => Some((
                Seat {
                    provider: p.provider,
                    creds: p.creds,
                    model: p.model,
                    account: p.account,
                },
                def,
            )),
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
        let (account, model) = self.registry.select(target, hints)?;
        let provider = self.registry.provider(&account)?;
        let creds: Arc<dyn CredentialSource> = if self.builtin {
            self.redactor.register(&account, FAKE_KEY);
            Arc::new(Credential {
                account: account.clone(),
                secret: Secret::new(FAKE_KEY.to_owned()),
                kind: AuthMethodKind::None,
                resolved_at: Instant::now(),
            })
        } else {
            let configured = self.registry.account(&account).cloned().ok_or_else(|| {
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

    /// Swaps in reloaded definitions.
    pub fn reload(&self, defs: &[AgentDef]) {
        *self.agents.write().unwrap_or_else(PoisonError::into_inner) =
            defs.iter().map(|d| (d.name.clone(), d.clone())).collect();
    }

    /// Records what `run` runs on, for the children it will have.
    pub fn remember(&self, run: RunId, account: &AccountName, model: &ModelId) {
        self.chosen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run, (account.clone(), model.clone()));
    }
}

impl Launcher for ChildLauncher {
    fn launch(
        &self,
        spec: &SpawnSpec,
        prompt: String,
        tx: mpsc::Sender<aibus_core::Msg>,
    ) -> Result<(RunConfig, RunDeps), String> {
        if spec.isolation == Isolation::Worktree {
            return Err("isolation: worktree is not available yet".into());
        }
        let mut def = self
            .agents
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&spec.agent)
            .cloned()
            .ok_or_else(|| format!("no agent named {}", spec.agent))?;
        def.tools.deny.extend(self.deny.iter().cloned());

        let parent = spec.parent.and_then(|p| {
            self.chosen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&p)
                .cloned()
        });
        // The caller's model, else the definition's. `inherit` takes the parent's, or the
        // session's when the parent is unknown.
        let target = match (&spec.model, expand(&def.model, &self.defaults), &parent) {
            (Some(m), _, _) => self.registry.model_ref(m),
            (None, ModelRef::Inherit, None) => self.session_default.clone(),
            (None, other, _) => other,
        };
        let hints = SelectHints {
            explicit: spec.account.clone(),
            agent: def.account.clone(),
            inherit: parent,
            ..SelectHints::default()
        };
        let picked = self.pick(&target, &hints).map_err(|e| e.to_string())?;
        self.remember(spec.run, &picked.account, &picked.model);

        Ok((
            RunConfig {
                run: spec.run,
                def,
                prompt,
                parent: spec.parent,
                depth: spec.depth,
                model: picked.model,
                account: picked.account,
                cwd: self.cwd.clone(),
                runs_dir: self.runs_dir.clone(),
                limits: self.limits,
                budgets: self.budgets,
                date: events::date(SystemTime::now()),
                data_home: self.data_home.clone(),
                context: Vec::new(),
                background: spec.background,
                max_spawn_depth: self.max_spawn_depth,
                forced: None,
            },
            RunDeps {
                provider: picked.provider,
                creds: picked.creds,
                tools: self.tools.clone(),
                tx,
                hooks: self.hooks.clone(),
                compactor: None,
                assist: self.assist.get().cloned(),
            },
        ))
    }
}
