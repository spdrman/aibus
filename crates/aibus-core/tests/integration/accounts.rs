//! 10 section 4, bullets 12 and 13: two accounts of one provider, and no credential on disk.

use std::collections::BTreeMap;
use std::sync::Arc;

use aibus_core::config::Config;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{
    AuthMethodKind, ConfigError, ModelRef, Provider, ProviderConfig, ProviderFactory, ProviderId,
    ProviderRegistry, Secret, StopReason,
};
use aibus_testkit::{capture_logs, xdg_temp, LogCapture, Xdg};
use tokio::sync::mpsc;

use crate::solo::bypass;

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

struct Accounts {
    fake: Arc<FakeProvider>,
    registry: AccountRegistry,
    store: Arc<CredentialStore>,
    xdg: Xdg,
    logs: LogCapture,
}

fn accounts() -> Accounts {
    let logs = capture_logs();
    let mut xdg = xdg_temp();
    xdg.forbid(WORK_KEY);
    xdg.forbid(PERSONAL_KEY);
    logs.forbid(WORK_KEY);
    logs.forbid(PERSONAL_KEY);
    // The config names the accounts and where their keys live, never the keys.
    std::fs::write(xdg.paths.config.join("config.toml"), CONFIG).unwrap();
    let cfg: Config = toml::from_str(CONFIG).unwrap();
    let fake = Arc::new(
        FakeProvider::new(
            Script::new()
                .then(Step::Text("done".into()))
                .then(Step::Stop(StopReason::EndTurn))
                .then(Step::Text("done".into())),
        )
            .with_account("anthropic-work", WORK_KEY)
            .with_account("anthropic-personal", PERSONAL_KEY),
    );
    let providers = ProviderRegistry::new(&[Box::new(Shared(fake.clone()))], &BTreeMap::new()).unwrap();
    let store = Arc::new(CredentialStore::new(Arc::new(MemoryKeychain::default()), Redactor::new()));
    store.store(&name("anthropic-work"), &Secret::new(WORK_KEY.into())).unwrap();
    store.store(&name("anthropic-personal"), &Secret::new(PERSONAL_KEY.into())).unwrap();
    Accounts {
        fake,
        registry: AccountRegistry::new(&cfg, Arc::new(providers)),
        store,
        xdg,
        logs,
    }
}

impl Accounts {
    /// Runs `general` with `account` set as given and returns what it sent.
    async fn run(&self, account: Option<&str>) -> Vec<Msg> {
        let mut def = bypass("general");
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
        let cwd = self.xdg.root().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let handle = spawn(
            RunConfig {
                run: RunId::new(),
                def,
                prompt: "hi".into(),
                parent: None,
                depth: 0,
                model,
                account: chosen.clone(),
                cwd,
                runs_dir: self.xdg.paths.state.join("runs"),
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
async fn two_anthropic_accounts_a_definition_with_account_anthropic_personal_uses_that_key() {
    let a = accounts();
    let personal = a.run(Some("anthropic-personal")).await;
    // The fake accepts a credential only with its own account's key, so a completed run means
    // the request carried the personal key and not the default account's.
    assert_eq!(status(&personal), RunStatus::Completed);
    assert_eq!(a.fake.requests()[0].account, name("anthropic-personal"));
    let Some(Msg::RunStarted { account, .. }) = personal.first() else {
        panic!("no RunStarted")
    };
    assert_eq!(account, &name("anthropic-personal"));

    // A definition without one is served by the default account, with the other key.
    let work = a.run(None).await;
    assert_eq!(status(&work), RunStatus::Completed);
    assert_eq!(a.fake.requests()[1].account, name("anthropic-work"));
}

#[tokio::test]
async fn no_credential_appears_in_a_file_under_the_temp_xdg_dirs_or_in_the_log() {
    let a = accounts();
    let msgs = a.run(Some("anthropic-personal")).await;
    assert_eq!(status(&msgs), RunStatus::Completed);
    for m in &msgs {
        let line = format!("{m:?} {:?}", m.summary());
        assert!(!line.contains(PERSONAL_KEY) && !line.contains(WORK_KEY), "{line}");
    }
    // The run left its conversation and result under the state dir, and the config is there.
    assert!(a.xdg.paths.state.join("runs").is_dir());
    assert!(a.xdg.paths.config.join("config.toml").is_file());
    a.xdg.assert_no_secret_on_disk();
    assert!(!a.logs.text().contains(PERSONAL_KEY) && !a.logs.text().contains(WORK_KEY));
}
