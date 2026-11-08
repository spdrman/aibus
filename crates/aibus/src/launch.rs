//! Builds the runs the orchestrator starts (01 section 4): the lead and every child get their
//! account, model and credential here, from the same chain (04 section 4.1).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

use aibus_core::{
    expand, AccountCredential, AccountName, AccountRegistry, AgentDef, AgentName, AibusError,
    Budgets, CredentialSource, CredentialStore, HookRunner, Isolation, Launcher, ModelDefaults,
    ModelId, Redactor, RunConfig, RunDeps, RunId, SelectHints, SpawnSpec, ToolExecutor, ToolLimits,
    ToolPattern,
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
}

impl ChildLauncher {
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
            },
        ))
    }
}
