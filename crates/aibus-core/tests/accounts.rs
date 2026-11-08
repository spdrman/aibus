//! B1.4: a definition's `account` decides whose key the provider sees, and nothing but the
//! store ever holds it.

use std::collections::BTreeMap;
use std::sync::Arc;

use aibus_core::agent::discover::discover;
use aibus_core::config::Config;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{
    AuthMethodKind, ConfigError, ModelRef, Provider, ProviderConfig, ProviderFactory, ProviderId,
    ProviderRegistry, Secret,
};
use tempfile::TempDir;
use tokio::sync::mpsc;

const WORK_KEY: &str = "sk-work-aaaaaaaaaaaa";
const PERSONAL_KEY: &str = "sk-personal-bbbbbbbbbbbb";

const CONFIG: &str = r#"
[defaults]
account = "anthropic-work"

[[account]]
name = "anthropic-work"
provider = "fake"
auth = { keyring = true }

[[account]]
name = "anthropic-personal"
provider = "fake"
auth = { keyring = true }
"#;

/// Hands out one prepared fake, which accepts each account only with its own key.
struct Shared(Arc<FakeProvider>);

impl ProviderFactory for Shared {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }
    fn build(&self, _: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(self.0.clone())
    }
    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey]
    }
}

fn name(n: &str) -> AccountName {
    AccountName(n.into())
}

fn general() -> AgentDef {
    let dir = TempDir::new().unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    found.agents[&"general".parse::<AgentName>().unwrap()].clone()
}

struct Rig {
    fake: Arc<FakeProvider>,
    registry: AccountRegistry,
    store: Arc<CredentialStore>,
    redactor: Redactor,
    cfg: Config,
}

/// `personal_key` is what the keychain holds under `anthropic-personal`.
fn rig(personal_key: &str) -> Rig {
    let cfg: Config = toml::from_str(CONFIG).unwrap();
    let fake = Arc::new(
        FakeProvider::new(Script::new().then(Step::Text("done".into())))
            .with_account("anthropic-work", WORK_KEY)
            .with_account("anthropic-personal", PERSONAL_KEY),
    );
    let providers = ProviderRegistry::new(&[Box::new(Shared(fake.clone()))], &BTreeMap::new())
        .unwrap();
    let redactor = Redactor::new();
    let keychain = Arc::new(MemoryKeychain::default());
    let store = Arc::new(CredentialStore::new(keychain, redactor.clone()));
    store.store(&name("anthropic-work"), &Secret::new(WORK_KEY.into())).unwrap();
    store
        .store(&name("anthropic-personal"), &Secret::new(personal_key.into()))
        .unwrap();
    Rig {
        fake,
        registry: AccountRegistry::new(&cfg, Arc::new(providers)),
        store,
        redactor,
        cfg,
    }
}

impl Rig {
    /// Starts a run of `general` with `account` set as given and collects its messages.
    async fn run(&self, account: Option<&str>) -> Vec<Msg> {
        let mut def = general();
        def.account = account.map(name);
        let hints = SelectHints {
            agent: def.account.clone(),
            ..SelectHints::default()
        };
        let (chosen, model) = self
            .registry
            .select(&ModelRef::Full(ModelId("fake-small".into())), &hints)
            .unwrap();
        let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
        let dir = TempDir::new().unwrap();
        let handle = spawn(
            RunConfig {
                run: RunId::new(),
                def,
                prompt: "hi".into(),
                parent: None,
                depth: 0,
                model,
                account: chosen.clone(),
                cwd: dir.path().to_path_buf(),
                runs_dir: dir.path().join("runs"),
                limits: ToolLimits::default(),
                date: "2026-11-08".into(),
                data_home: None,
                budgets: Budgets::default(),
                context: Vec::new(),
                background: false,
                max_spawn_depth: 3,
                forced: None,
                start: Start::New,
                worktree: None,
            },
            RunDeps {
                provider: self.registry.provider(&chosen).unwrap(),
                creds: Arc::new(AccountCredential {
                    store: self.store.clone(),
                    account: self.registry.account(&chosen).cloned().unwrap(),
                }),
                tools: Arc::new(ToolExecutor::new(ToolRegistry::read_only())),
                tx,
                hooks: Arc::new(HookRunner::default()),
                compactor: None,
                assist: None,
            },
        );
        let mut msgs = Vec::new();
        while let Some(m) = rx.recv().await {
            let last = m.name() == "RunFinished";
            msgs.push(m);
            if last {
                break;
            }
        }
        handle.join.await.unwrap();
        msgs
    }
}

fn status(msgs: &[Msg]) -> RunStatus {
    msgs.iter()
        .find_map(|m| match m {
            Msg::RunFinished { result, .. } => Some(result.status),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_definition_with_account_anthropic_personal_makes_the_fake_see_that_accounts_key() {
    let rig = rig(PERSONAL_KEY);
    let msgs = rig.run(Some("anthropic-personal")).await;

    // The fake accepts a credential only with its own account's key, so a completed run means
    // it got the personal key and not the default account's.
    assert_eq!(status(&msgs), RunStatus::Completed);
    let seen = rig.fake.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].account, name("anthropic-personal"));

    // The pair the chain chose is what the run announces, so it is what `model:run:<id>:meta`
    // holds.
    let Some(Msg::RunStarted { account, model, .. }) = msgs.first() else {
        panic!("no RunStarted")
    };
    assert_eq!((account, model.0.as_str()), (&name("anthropic-personal"), "fake-small"));
}

#[tokio::test]
async fn without_an_account_on_the_definition_the_default_account_serves() {
    let rig = rig(PERSONAL_KEY);
    let msgs = rig.run(None).await;
    assert_eq!(status(&msgs), RunStatus::Completed);
    assert_eq!(rig.fake.requests()[0].account, name("anthropic-work"));
}

#[tokio::test]
async fn a_wrong_key_under_the_accounts_name_fails_the_run_with_a_provider_auth_error() {
    let rig = rig("sk-somebody-elses-key");
    let msgs = rig.run(Some("anthropic-personal")).await;
    assert_eq!(status(&msgs), RunStatus::Failed);
}

#[tokio::test]
async fn the_key_is_registered_with_the_redactor_and_never_reaches_a_message() {
    let rig = rig(PERSONAL_KEY);
    let msgs = rig.run(Some("anthropic-personal")).await;
    assert_eq!(
        rig.redactor.redact(&format!("Authorization: {PERSONAL_KEY}")),
        "Authorization: [redacted:anthropic-personal]"
    );
    // Not in the messages a run sends, and so not in anything the graph or a recording holds.
    for m in &msgs {
        let line = format!("{m:?} {:?}", m.summary());
        assert!(!line.contains(PERSONAL_KEY) && !line.contains(WORK_KEY), "{line}");
    }
    // Not in the config the accounts were read from either.
    assert!(!toml::to_string(&rig.cfg).unwrap().contains(PERSONAL_KEY));
    // And what the fake recorded carries `***` where a key would be.
    let recorded = format!("{:?}", rig.fake.requests());
    assert!(!recorded.contains(PERSONAL_KEY));
}
