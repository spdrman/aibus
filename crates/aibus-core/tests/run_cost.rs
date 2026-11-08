//! The run adds up cost once: from the provider when it costed the turn itself (the real ones
//! do, cache buckets included), from the price table when it did not (the fake).

use std::sync::Arc;

use aibus_core::agent::discover::discover;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{
    AccountInfo, AliasTable, Capabilities, ChatRequest, ChatResponse, Credential, ModelInfo,
    ModelRef, Price, Provider, ProviderError, ProviderId, StreamChunk,
};
use aibus_testkit::fake_credential;
use async_trait::async_trait;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The fake, with every turn costed by the provider at a figure the price table does not give.
struct Costed(FakeProvider);

#[async_trait]
impl Provider for Costed {
    fn id(&self) -> ProviderId {
        self.0.id()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn models(&self) -> Vec<ModelInfo> {
        self.0.models()
    }
    async fn refresh_models(&self, c: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        self.0.refresh_models(c).await
    }
    fn resolve_model(&self, r: &ModelRef, a: &AliasTable) -> Option<ModelId> {
        self.0.resolve_model(r, a)
    }
    async fn stream(
        &self,
        c: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError> {
        let mut resp = self.0.stream(c, req, out, cancel).await?;
        resp.usage.cost_usd = 0.25;
        Ok(resp)
    }
    async fn count_tokens(&self, c: &Credential, r: &ChatRequest) -> Option<Result<u64, ProviderError>> {
        self.0.count_tokens(c, r).await
    }
    async fn verify(&self, c: &Credential) -> Result<AccountInfo, ProviderError> {
        self.0.verify(c).await
    }
    fn price(&self, m: &ModelId) -> Option<Price> {
        self.0.price(m)
    }
}

async fn cost_of(provider: Arc<dyn Provider>) -> f64 {
    let dir = TempDir::new().unwrap();
    let runs = TempDir::new().unwrap();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let def = found.agents[&"explore".parse::<AgentName>().unwrap()].clone();
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let handle = spawn(
        RunConfig {
            run: RunId::new(),
            def,
            prompt: "hi".into(),
            parent: None,
            depth: 0,
            model: ModelId("fake-small".into()),
            account: AccountName("fake".into()),
            cwd: dir.path().to_path_buf(),
            runs_dir: runs.path().to_path_buf(),
            limits: ToolLimits::default(),
            date: "2026-11-08".into(),
            data_home: None,
            budgets: Budgets::default(),
            context: Vec::new(),
            background: false,
            max_spawn_depth: 3,
            forced: None,
        },
        RunDeps {
            provider,
            creds: Arc::new(fake_credential()),
            tools: Arc::new(ToolExecutor::new(ToolRegistry::read_only())),
            tx,
            hooks: Arc::new(HookRunner::default()),
            compactor: None,
        },
    );
    let mut cost = None;
    while let Some(m) = rx.recv().await {
        if let Msg::RunFinished { result, .. } = m {
            cost = Some(result.usage.cost_usd);
            break;
        }
    }
    handle.join.await.unwrap();
    cost.expect("the run finished")
}

fn script() -> Script {
    Script::new().then(Step::Text("done".into())).then(Step::Usage { input: 1_000_000, output: 0 })
}

#[tokio::test]
async fn a_cost_from_the_provider_is_not_added_to_again_from_the_price_table() {
    let cost = cost_of(Arc::new(Costed(FakeProvider::new(script())))).await;
    assert!((cost - 0.25).abs() < 1e-12, "{cost}");
}

#[tokio::test]
async fn without_one_the_price_table_costs_the_turn() {
    // The fake prices input at $1 per million tokens.
    let cost = cost_of(Arc::new(FakeProvider::new(script()))).await;
    assert!((cost - 1.0).abs() < 1e-12, "{cost}");
}
