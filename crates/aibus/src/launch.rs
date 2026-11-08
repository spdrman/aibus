//! Builds the runs the orchestrator starts (01 section 4): the lead and every child get their
//! account, model and credential here, from the same chain (04 section 4.1).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

use aibus_core::{
    expand, AccountCredential, AccountName, AccountRegistry, AgentDef, AgentName, AibusError,
    Budgets, CredentialSource, CredentialStore, HookRunner, Isolation, Launcher, ModelCompactor,
    ModelDefaults, ModelId, Redactor, RunConfig, RunDeps, RunId, SelectHints, SpawnSpec, Start,
    ToolExecutor, ToolLimits, ToolPattern,
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
    /// The definition each started run got (with `--deny` applied), so a fork runs its parent's
    /// and a resume its own, even when the files were edited since.
    pub defs: Mutex<HashMap<RunId, AgentDef>>,
    /// `<cache>/worktrees/<session>`: where an isolated run's checkout goes (09 section 7).
    pub worktrees: PathBuf,
}

/// What was remembered for `run`.
fn recall<T: Clone>(m: &Mutex<HashMap<RunId, T>>, run: RunId) -> Option<T> {
    m.lock().unwrap_or_else(PoisonError::into_inner).get(&run).cloned()
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
                def
            }
        };
        if spec.fork {
            // The same agent as the parent, so it is not offered what only a definition's own
            // name could reach.
            def.name = spec.agent.clone();
        }

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
                match (&spec.model, expand(&def.model, &self.defaults), &parent) {
                    (Some(m), _, _) => self.registry.model_ref(m),
                    (None, ModelRef::Inherit, None) => self.session_default.clone(),
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
        let compactor: Arc<dyn aibus_core::Compactor> = Arc::new(ModelCompactor {
            provider: picked.provider.clone(),
            creds: picked.creds.clone(),
            model: picked.model.clone(),
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
                forced: None,
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
            },
        ))
    }
}
