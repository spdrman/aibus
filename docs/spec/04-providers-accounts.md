# 04. Providers and accounts

Multiple providers and multiple logins to the same provider, from day one. The security expert signed off on sections 5 and 7 with the two conditions recorded at the end.

## 1. Shape

```
AgentDef.model + AgentDef.account      CLI --model/--account        config [defaults]
                 │                             │                          │
                 └──────────────┬──────────────┴──────────────────────────┘
                                ▼
                     AccountRegistry::select(ModelRef, hints) -> (AccountName, ModelId)
                                │
                                ▼
                    Account { provider: ProviderId, base_url, aliases, rate, key_ref }
                                │                                    │
                                ▼                                    ▼
                    ProviderRegistry::get(provider)        CredentialStore::resolve(key_ref) -> Secret
                                │                                    │
                                └──────────────┬─────────────────────┘
                                               ▼
                                 provider.stream(&creds, request, out, cancel)
```

An **account** is one login. A **provider** is code. The registry maps account names to providers; the credential store maps account names to secrets, and nothing else ever holds a secret.

## 2. The `Provider` trait

```rust
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    /// Stable id used in config: "anthropic", "openai", "fake". Lower-case, no spaces.
    fn id(&self) -> ProviderId;

    /// What this provider can do, so core can degrade instead of guessing.
    fn capabilities(&self) -> Capabilities;

    /// Known models for alias resolution and context sizes. May be a static table
    /// refreshed by `refresh_models` when the API supports listing.
    fn models(&self) -> Vec<ModelInfo>;
    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Map a model reference to this provider's native id, or say it cannot.
    fn resolve_model(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId>;

    /// One streaming completion. Chunks go to `out`; the final response is returned.
    /// Must honour `cancel` within 100 ms and must never log `creds`.
    async fn stream(
        &self,
        creds: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError>;

    /// Token count for budgeting; `None` when the provider has no endpoint and
    /// core should use the heuristic.
    async fn count_tokens(&self, creds: &Credential, req: &ChatRequest) -> Option<Result<u64, ProviderError>>;

    /// A cheap authenticated call. Used by `aibus account test`.
    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError>;

    /// Pricing per model, for cost estimates. Static table; `None` when unknown.
    fn price(&self, model: &ModelId) -> Option<Price>;
}

pub struct Capabilities {
    pub tools: bool,
    pub parallel_tool_calls: bool,
    pub streaming: bool,
    pub thinking: bool,
    pub prompt_cache: bool,
    pub logprobs: bool,              // the router prefers providers with this
    pub effort: bool,
    pub images: bool,
    pub system_role: SystemRole,     // Separate | FirstMessage
    pub max_tools: Option<u32>,
}

pub struct ModelInfo { pub id: ModelId, pub context_window: u32, pub max_output: u32, pub family: SmolStr, pub deprecated: bool }
pub struct Price { pub input_per_mtok: f64, pub output_per_mtok: f64, pub cache_read_per_mtok: Option<f64>, pub cache_write_per_mtok: Option<f64> }
pub struct AccountInfo { pub label: String, pub org: Option<String>, pub models: Vec<ModelId> }
```

Request and response are provider-neutral:

```rust
pub struct ChatRequest {
    pub model: ModelId,
    pub system: String,
    pub messages: Vec<Message>,          // 02 section 5
    pub tools: Vec<ToolSpec>,
    pub tool_choice: ToolChoice,         // Auto | None | Required | Named(SmolStr)
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
    pub effort: Option<Effort>,
    pub thinking: Option<ThinkingBudget>,
    pub cache_hint: CacheHint,           // where a cache breakpoint may go
    pub logprobs: bool,
    pub metadata: RequestMetadata,       // run id, session id (for provider-side tracing, no secrets)
}

pub enum StreamChunk {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id: ToolCallId, name: SmolStr },
    ToolUseInputDelta { id: ToolCallId, json_fragment: String },
    ToolUseEnd { id: ToolCallId },
    Usage(Usage),
    Logprob { token: String, logprob: f32 },
    Done(StopReason),
}

pub struct ChatResponse { pub message: Message, pub usage: Usage, pub stop: StopReason, pub request_id: Option<String> }
pub enum StopReason { EndTurn, ToolUse, MaxTokens, Refusal, Other(SmolStr) }
```

Every provider error maps to `ProviderError { kind, retryable, retry_after, request_id, message, source }` (02 section 6). The mapping table per provider lives next to its client code and is unit-tested against recorded error bodies.

## 3. Provider crates

| Crate | Provider ids | Notes |
|---|---|---|
| `aibus-provider` | `fake` | The trait, registry, credential store, and the deterministic fake (10 section 3). |
| `aibus-provider-anthropic` | `anthropic` | Messages API, SSE streaming, cache breakpoints, thinking, effort. Also serves Bedrock/Vertex through `base_url` + `auth = sigv4|gcloud` in M6. |
| `aibus-provider-openai` | `openai`, `kimi`, `openai-compatible` | Chat Completions and Responses API dialects. `kimi` is the OpenAI-compatible Moonshot endpoint with `base_url = https://api.moonshot.ai/v1` and its model table. `openai-compatible` is the generic dialect for local servers (Ollama, vLLM, LM Studio). |

Kimi is a dialect, not a crate, because its wire format is OpenAI's. The `ProviderId` is still `kimi` so config, pricing and the model table are distinct.

### 3.1 Adding a provider without touching core

1. New crate `aibus-provider-<name>` depending on `aibus-provider` only.
2. Implement `Provider` and a `ProviderFactory`:

```rust
pub trait ProviderFactory: Send + Sync {
    fn id(&self) -> ProviderId;
    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError>;
    fn auth_methods(&self) -> &'static [AuthMethodKind];   // which `auth` values this provider accepts
}
```

3. Register it in the binary crate's `providers.rs`:

```rust
pub fn builtin_factories() -> Vec<Box<dyn ProviderFactory>> {
    vec![
        Box::new(aibus_provider::fake::Factory),
        Box::new(aibus_provider_anthropic::Factory),
        Box::new(aibus_provider_openai::Factory::openai()),
        Box::new(aibus_provider_openai::Factory::kimi()),
        Box::new(aibus_provider_openai::Factory::compatible()),
        // Box::new(aibus_provider_gemini::Factory),   <- the one line a new provider adds
    ]
}
```

4. Add a `deny.toml` entry if the crate brings new licences, and a recorded-fixture test (10 section 3.1).

`aibus-core` never names a provider. The binary's `providers.rs` is the only registration point, and it is a list, not logic. The panel considered `inventory`-style link-time registration and rejected it: an explicit list is greppable and cargo-deny can see it.

## 4. Accounts and selection

```rust
pub struct Account {
    pub name: AccountName,                // "anthropic-work", "anthropic-personal", "kimi"
    pub provider: ProviderId,
    pub label: String,                    // shown in the UI; "Work (org acme)"
    pub base_url: Option<Url>,
    pub auth: AuthMethod,
    pub default_model: Option<ModelRef>,
    pub aliases: AliasTable,              // sonnet = "claude-sonnet-4-5"
    pub rate: RateLimits,                 // max_concurrent, requests_per_minute, tokens_per_minute
    pub tags: Vec<SmolStr>,               // "cheap", "eu", used by selection hints
    pub enabled: bool,
    pub fallback: Vec<AccountName>,       // tried in order when this account fails before the first token (4.3)
}

pub enum AuthMethod {
    ApiKey { key_ref: KeyRef },           // header set by the provider
    Bearer { token_ref: KeyRef },
    Env { var: String },                  // read at request time, never stored
    None,                                 // local servers
    // OAuth { .. } is M6 (11-delivery)
}

pub enum KeyRef { Keyring { service: SmolStr, user: SmolStr }, Env { var: String }, File { path: PathBuf } }
```

Two Anthropic accounts are two `[[account]]` entries with different names and `key_ref`s. Nothing in the provider knows which account it is serving beyond the credential and base URL it was handed.

### 4.1 Selection chain

`AccountRegistry::select(model: &ModelRef, hints: &SelectHints) -> Result<(AccountName, ModelId), SelectError>` applies, in order, the first that yields exactly one account:

1. `hints.explicit` (CLI `--account`, or the `account` argument of a `delegate` call).
2. The agent definition's `account`.
3. `hints.inherit` (the parent run's account) when `model` is `Inherit` or resolves on that account.
4. Config `[defaults] account`, when the model resolves there.
5. Config `[models]` alias table entries of the form `alias = "account/model"`.
6. Every enabled account whose provider `resolve_model` returns `Some`. If exactly one, use it. If several, prefer the one tagged in `hints.prefer_tags`, then the first in config order, and log `info: model sonnet served by anthropic-work (also: anthropic-personal)`.
7. Otherwise `SelectError::NoAccount { model }` (exit code 4 on the CLI, a banner in the TUI).

The chosen pair is written to `model:run:<id>:meta` so the card and the log show it (`search · Haiku 5.5` in frames f01 to f37 is the model label; aibus adds the account name on hover/transcript header: `search · haiku · anthropic-work`).

### 4.2 Per-account concurrency and rate limits

`RateLimits { max_concurrent: 4, requests_per_minute: Option<u32>, tokens_per_minute: Option<u32> }`. A token-bucket per account in `aibus-provider` gates `stream`; a run waiting on it shows `◌ waiting for anthropic-work slot` on its card. 429s with `retry_after` tighten the bucket for 60 s.

### 4.3 Account fallback

With several accounts for one model, a spent quota or a long overload on one should move the run to the next instead of ending it. This is the only place in the epic that touches the run loop, and it is off unless configured.

```toml
[defaults]
fallback_on = ["quota", "auth", "overloaded"]   # the default; "rate_limit" is not in it, waiting usually wins

[[account]]
name = "anthropic-work"
provider = "anthropic"
fallback = ["anthropic-personal", "bedrock"]    # in order
```

* **Config.** `fallback` names accounts; a name that is not an account, or the account's own, is a config error with its line. An agent definition can set `fallback: [...]` to replace the account's list, and `fallback: []` to stay put.
* **Before the first token.** A switch happens only before the first `TextDelta`, `ThinkingDelta` or `ToolUseStart` of a turn, so the user has seen nothing from the failed account and the conversation goes to the next one unchanged. After the first token a `Quota` ends the run as it did before.
* **Budgets.** The retry budget is per account: a retryable error (`Overloaded`, and `RateLimit` when it is in `fallback_on`) retries five times on the account, and only then moves. `Quota` and `Auth` are not retried and move at once. A kind that is not in `fallback_on` retries or ends as before.
* **Model.** The model is resolved again on the new account. An account where it does not resolve, or whose credential is missing, is left out of the chain with a log line.
* **Where it shows.** The run's header reads `↷ <account>` after a switch, `model:run:<id>:meta` carries the final account (and `fallback_from`), and the transcript records a `switch` line after the `start` line that names the first account.
* **Cooldown.** An account that triggered a switch is skipped by the selection chain for a while, in memory only: after a `Quota` until the `resets_in` the response's headers gave (`RateLimitInfo`), or 30 minutes; after an `Overloaded` for 60 seconds; after a `RateLimit` for its `retry_after`; after an `Auth` until `aibus account test` passes (a reload of the accounts, which a login or logout causes, clears it). An explicit `--account` is a demand and ignores it, and an account that is the only one able to serve a model is used anyway.
* **Header-driven limits.** The account's limiter already reads `requests_remaining`, `tokens_remaining` and the reset on every response, so an account at zero waits for its window, and a `Quota` on it goes straight to the fallback.
* **Seats.** The advisor and the router get the same behaviour through their `Seat`: a failed consultation is asked again on the next account, and a seat that cannot be answered still never blocks the lead.

## 5. Credential storage

Security expert's section.

- **Primary**: the OS keychain through the `keyring` crate: macOS Keychain, Linux Secret Service (and `keyutils` fallback), Windows Credential Manager. Service name `aibus`, user `account:<name>`. `aibus account add <name> --provider anthropic` prompts for the key with echo off (or reads `--key-stdin`), verifies it with `provider.verify`, and stores it. The config file gets `auth = { keyring = true }` and never the key.
- **Env**: `auth = { env = "ANTHROPIC_API_KEY_WORK" }` reads the variable at request time. Useful for CI. The value is never written anywhere.
- **File**: `auth = { file = "/run/secrets/anthropic" }` for container secrets. aibus refuses the file unless its mode is `0600` or stricter and it is owned by the current user; it prints the fix (`chmod 600`) and exits 4 otherwise.
- **Never**: a key literal in `config.toml`, in an agent definition, on the command line (`--key` does not exist; `--key-stdin` does), in logs, in transcripts, in the graph, in snapshots, in error messages.

```rust
pub struct Secret<T>(T);                       // no Serialize, Debug prints "***", Drop zeroises
impl<T: Zeroize> Drop for Secret<T> { .. }
pub struct Credential { pub account: AccountName, pub secret: Secret<String>, pub kind: AuthMethodKind, pub resolved_at: Instant }

pub trait CredentialStore: Send + Sync {
    fn resolve(&self, account: &Account) -> Result<Credential, CredentialError>;
    fn store(&self, account: &AccountName, secret: Secret<String>) -> Result<(), CredentialError>;
    fn remove(&self, account: &AccountName) -> Result<(), CredentialError>;
    fn probe(&self) -> StoreHealth;           // is a keychain available at all
}
```

Credentials are resolved per request and dropped after; the cache is at most 60 s in memory, so a key rotated in the keychain takes effect within a minute.

Where no keychain exists (a headless Linux box without Secret Service), `aibus account add` says so, offers `--env` or `--file`, and never silently falls back to a plaintext file. `aibus doctor` reports `credential store: keyring (Secret Service)` or `credential store: unavailable, env/file only`.

## 6. Config

```toml
# ~/.config/aibus/config.toml
[defaults]
account = "anthropic-work"
model = "sonnet"
advisor_model = "opus"
router_model = "fast"
swarm_model = "haiku"

[models]                  # global aliases; "account/model" pins an account
sonnet = "claude-sonnet-4-5"
opus   = "anthropic-work/claude-opus-4-1"
haiku  = "claude-haiku-4-5"
fast   = "kimi/kimi-k2-turbo"
gpt    = "openai-personal/gpt-5"

[[account]]
name = "anthropic-work"
provider = "anthropic"
label = "Work"
auth = { keyring = true }
rate = { max_concurrent = 4 }
tags = ["default"]

[[account]]
name = "anthropic-personal"
provider = "anthropic"
label = "Personal"
auth = { keyring = true }
rate = { max_concurrent = 2 }

[[account]]
name = "openai-personal"
provider = "openai"
auth = { env = "OPENAI_API_KEY" }

[[account]]
name = "kimi"
provider = "kimi"
auth = { keyring = true }
aliases = { fast = "kimi-k2-turbo" }

[[account]]
name = "local"
provider = "openai-compatible"
base_url = "http://127.0.0.1:11434/v1"
auth = { none = true }
```

`aibus account list` prints name, provider, label, auth kind, default model, and `ok | missing credential | disabled`. `--json` gives the same without secrets (there are none to give).

## 7. Security sign-off

The security expert's two conditions, both now in the spec:

1. The graph, the snapshot, the commit log and the `Msg` recording never carry a credential, because `Account` in the graph is the redacted `AccountView` and `Credential` is resolved outside the graph on the worker thread (03 section 10).
2. Provider crates receive `&Credential` for the duration of one call and are forbidden (by review checklist and a `tracing` test that greps captured logs for the fake's known key) from logging request headers. The HTTP client is built with `reqwest::Client` per account with `.default_headers()` empty; auth headers are added per request by the provider from the `Credential`, so a stray `{:?}` of the client cannot print them.
