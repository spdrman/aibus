# 02. Architecture

## 1. Crate layout

A Cargo workspace. The panel's Rust engineer argued for a single crate to start ("split when it hurts"); the architecture and security experts won on two points: the engine must compile without ratatui and the TUI must compile without reqwest, and the only way to make that a build-time fact is crate boundaries. Provider implementations are separate crates so `cargo-deny` can scope their dependency trees and so a new provider never touches core. Recorded in DECISIONS.md as D-01.

```
aibus/
├── Cargo.toml                 # [workspace], resolver = "2"
├── crates/
│   ├── aibus/                 # the binary: clap, wiring, TUI or headless
│   ├── aibus-core/            # engine: definitions, runs, tools, hooks, continuity, orchestrator
│   ├── aibus-graph/           # Causl contract (trait) + mini engine + causl-engine-core host
│   ├── aibus-provider/        # Provider trait, account registry, credentials, fake provider
│   ├── aibus-provider-anthropic/
│   ├── aibus-provider-openai/ # OpenAI, and any OpenAI-compatible dialect (Kimi/Moonshot, local)
│   ├── aibus-tui/             # ratatui widgets, view model derivations, key handling
│   └── aibus-testkit/         # fixtures, fake clock, TestBackend helpers, snapshot macros
├── docs/
├── examples/agents/           # the built-in definitions as files, for reference
└── deny.toml
```

Dependency direction (an arrow means "depends on"):

```
aibus ──► aibus-tui ──► aibus-core ──► aibus-graph
  │                        │
  ├──► aibus-provider-anthropic ──► aibus-provider ◄── aibus-core
  └──► aibus-provider-openai   ──► aibus-provider
```

`aibus-core` depends on `aibus-provider` (the trait), never on a concrete provider. `aibus-tui` never depends on a provider crate. `aibus-graph` depends on nothing in the workspace.

Edition 2021, MSRV pinned in `rust-toolchain.toml` (1.85 at the time of writing; raised only with a changelog note). `#![forbid(unsafe_code)]` on every crate except where a keychain FFI crate needs it, and there it is `deny` with an allow on the one function.

## 2. Runtime

**tokio**, multi-thread scheduler, `rt-multi-thread`, `macros`, `sync`, `time`, `process`, `fs`, `signal`. The alternatives considered were `smol` (smaller, but reqwest and most provider SDK crates assume tokio) and a thread-per-run model with `std::sync::mpsc` (simpler, but provider streaming is naturally async and a session can have eight runs plus hooks plus timers). tokio wins on ecosystem fit; its cost is binary size, which does not matter here.

Thread model:

- **Main thread**: owns the terminal, the graph (03) and the render loop. It never awaits a provider.
- **Runtime worker threads**: provider streams, tool execution, hook processes, file IO.
- **Blocking pool** (`spawn_blocking`): ripgrep, git, SQLite, keychain calls.

The graph is single-owner. Only the main thread commits. Everything else sends `Msg`s.

## 3. Event and message architecture

The panel went three rounds on this (D-03 in DECISIONS.md). The result is an Elm-style loop whose model is a Causl graph, with actors on the side for anything that does IO.

```
                     crossterm         provider stream        hook / tool / timer
                        │                    │                        │
                        ▼                    ▼                        ▼
                    Msg::Key           Msg::Stream(..)           Msg::ToolDone(..)
                        └──────────────┬─────┴────────────────────────┘
                                       ▼
                         mpsc::Receiver<Msg>  (bounded, 4096)
                                       │
                                       ▼ main thread, one Msg at a time
                       reduce(&graph, msg) -> Vec<Write>       pure, no IO
                                       │
                                       ▼
                       graph.commit(intent = msg.name(), writes)   exactly one per Msg
                                       │
                         ┌─────────────┴─────────────┐
                         ▼                           ▼
                 changed_nodes ∩ view deps     effects.run_after(commit)
                 → mark dirty widgets          reads derived nodes, spawns IO,
                 → render if dirty             results come back as new Msgs
```

Three rules make this hold together:

1. **One `Msg`, one commit.** `reduce` returns the writes; the loop commits them under the `Msg`'s variant name as the intent. The commit log is therefore a complete, structured history of the session (03 section 6).
2. **No IO inside `reduce` or inside a derived compute.** The effects executor is the only place that talks to the world, and it only runs after a commit, reading derived nodes.
3. **Back-pressure by design.** Producers hold a `mpsc::Sender<Msg>` with capacity 4096. A producer that cannot send blocks (provider stream) or drops with a counter (tick timer). Stream chunks are coalesced before they become a `Msg` (08 section 2).

### 3.1 `Msg`

```rust
pub enum Msg {
    // terminal
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize { cols: u16, rows: u16 },
    Tick,                                   // 30 Hz when anything is animating, else off
    // user intent (already parsed from keys/commands)
    SubmitPrompt { text: String },
    Command(Command),
    // engine facts
    RunStarted { run: RunId, agent: AgentName, role: Role, parent: Option<RunId>, model: ModelId, account: AccountName },
    Stream { run: RunId, chunks: Vec<StreamChunk> },   // coalesced
    ToolCallRequested { run: RunId, call: ToolCall },
    ToolCallFinished { run: RunId, call_id: ToolCallId, result: ToolResult, duration: Duration },
    PermissionNeeded { request: PermissionRequest },
    PermissionDecided { id: PermissionId, decision: Decision },
    RunFinished { run: RunId, result: RunResult },
    RunFailed { run: RunId, error: RunError },
    UsageUpdate { run: RunId, usage: Usage },
    HookFired { run: Option<RunId>, event: HookEvent, outcome: HookOutcome },
    RouterAnswered { fork: ForkId, question: RouterQuestion, answer: RouterAnswer },
    AdvisorSpoke { checkpoint: Checkpoint, advice: String, tokens_read: u64 },
    CheckpointHit { checkpoint: Checkpoint, detail: String },
    Compacted { run: RunId, pre_tokens: u64 },
    HandoffWritten { path: PathBuf },
    // infra
    AgentsReloaded { defs: Vec<AgentDef>, warnings: Vec<String> },
    AccountsReloaded { accounts: Vec<AccountView> },
    IoError { context: String, error: String },
    Shutdown { reason: ShutdownReason },
}

impl Msg {
    /// The commit intent. Stable names; they end up in the commit log and in tests.
    pub fn name(&self) -> &'static str { /* "Key", "Stream", "RunFinished", ... */ }
}
```

`Command` is the user-facing verb set the TUI and CLI both produce:

```rust
pub enum Command {
    Delegate { agent: AgentName, prompt: String, background: bool, isolation: Isolation },
    Fork { prompt: String },
    Cancel { run: RunId },
    Resume { run: RunId, message: String },
    SendMessage { run: RunId, text: String },
    Permission { id: PermissionId, decision: Decision },
    Retry { run: RunId },
    Focus(Pane), Scroll(Pane, ScrollDelta), Select(Pane, Selection),
    OpenTranscript(RunId), CloseModal, OpenPalette, OpenHelp,
    Workflow { name: String, input: String },
    LedgerUpdate { section: String, text: String },
    CreateHandoff { outcome: Option<Outcome> },
    Compact { run: RunId }, Clear,
    Quit { force: bool },
}
```

### 3.2 Effects executor

```rust
pub struct Effects {
    runtime: tokio::runtime::Handle,
    tx: mpsc::Sender<Msg>,
    providers: Arc<ProviderRegistry>,
    accounts: Arc<AccountRegistry>,
    tools: Arc<ToolExecutor>,
    hooks: Arc<HookRunner>,
    inflight: HashMap<RunId, RunHandle>,     // JoinHandle + CancellationToken + mailbox
}

impl Effects {
    /// Called by the loop after every commit with the commit record.
    pub fn after_commit(&mut self, graph: &dyn Graph, commit: &Commit) {
        // 1. runs the orchestrator says may start now
        for spec in graph.read::<Vec<SpawnSpec>>("model:orchestrator:runnable") { self.spawn_run(spec); }
        // 2. tool calls approved but not yet executing
        for call in graph.read::<Vec<PendingTool>>("model:tools:pending") { self.exec_tool(call); }
        // 3. hooks due
        // 4. router questions queued, advisor checkpoints queued
        // 5. persistence: snapshot if `model:persist:due`
        // 6. cancellations
    }
}
```

Every effect is **idempotent by id**: `runnable` lists `RunId`s that are in state `Queued`; `spawn_run` transitions them to `Starting` by sending a `Msg::RunStarted`, and the executor keeps `inflight` so a repeated read of the same id is a no-op. That is what lets the loop be restartable from a snapshot: hydrate, run `after_commit` once, and anything that was queued starts again.

### 3.3 A run as an actor

Each run is one tokio task with a mailbox:

```rust
pub struct RunHandle {
    pub id: RunId,
    pub cancel: CancellationToken,
    pub mailbox: mpsc::Sender<RunMail>,   // SendMessage, PermissionDecided, Resume
    pub join: JoinHandle<()>,
}

pub enum RunMail {
    UserMessage(String),
    Permission { id: PermissionId, decision: Decision },
    Compact,
}
```

The task runs the tool loop:

```
loop {
    build request from conversation + tools for this run
    stream = provider.stream(req)              // chunks → coalesce → Msg::Stream
    assistant turn completes with 0..n tool calls
    for each tool call:
        pre_tool_use hooks → allow/deny/ask
        if ask: send Msg::PermissionNeeded, await mailbox
        execute (ToolExecutor) → Msg::ToolCallFinished → append result
    if no tool calls: stop hooks → break with Completed
    if turns == max_turns: break with Partial
}
send Msg::RunFinished
```

The task owns its `Conversation` (messages, usage, compaction boundary). The graph holds the run's **status, counters, and a handle to its transcript blocks**, not the conversation itself (03 section 8).

## 4. Module tree

```
crates/aibus-core/src/
├── lib.rs
├── ids.rs             # SessionId, RunId, AgentName, AccountName, ToolCallId (ulid-backed newtypes)
├── agent/
│   ├── def.rs         # AgentDef, Frontmatter, parse(), validate()
│   ├── discover.rs    # scope walk, precedence, extra_dirs, builtins
│   └── builtin.rs     # the built-in definitions as const &str
├── run/
│   ├── mod.rs         # Run, RunStatus, RunResult, RunError
│   ├── conversation.rs# Message, ContentBlock, compaction boundary
│   ├── loop_.rs       # the tool loop task
│   └── result.rs      # wrapping, capping, escaping of child results
├── orchestrator.rs    # spawn rules, limits, runnable derivation inputs
├── tools/
│   ├── mod.rs         # Tool trait, ToolSpec, ToolExecutor, allow-list matching
│   ├── fs.rs          # read, write, edit, glob, ls
│   ├── grep.rs
│   ├── shell.rs       # bash
│   ├── web.rs
│   ├── delegate.rs    # delegate, fork, resume_run, send_message
│   ├── continuity.rs  # ledger_update, create_handoff, recall, memory_*
│   └── user.rs        # ask_user, plan
├── permission.rs      # PermissionMode, PermissionRequest, Decision, rules
├── hooks/
│   ├── mod.rs         # HookEvent, HookSet, HookRunner, matcher
│   └── io.rs          # stdin JSON, exit-code mapping, timeouts
├── advisor.rs         # checkpoints, packet builder
├── router.rs          # RouterQuestion/Answer, verdict, question sites
├── continuity/
│   ├── ledger.rs
│   ├── handoff.rs     # YAML schema, write/read, auto-handoff
│   ├── memory.rs      # scopes, MemoryIndex trait, FTS5 impl
│   └── workflow.rs    # workflow TOML, step runner
├── state/
│   ├── mod.rs         # node id constants, namespaces, Write helpers
│   ├── reduce.rs      # reduce(graph, msg) -> Vec<Write>
│   ├── derive.rs      # registration of every derived node
│   ├── migrate.rs     # snapshot schema migrations
│   └── store.rs       # side arena for heavy values (transcript blocks)
├── effects.rs         # Effects executor
├── persist/
│   ├── paths.rs       # XDG resolution
│   ├── session.rs     # session dir layout, snapshot + commit log writer
│   ├── transcript.rs  # JSONL append, redaction
│   └── index.rs       # SQLite (sessions, runs, memory FTS)
├── config.rs          # Config, load, merge, defaults
├── redact.rs          # secret patterns, tracing layer
└── error.rs           # AibusError, exit code mapping
```

```
crates/aibus-tui/src/
├── lib.rs
├── app.rs             # terminal setup, loop glue, dirty tracking
├── keys.rs            # KeyEvent -> Msg/Command, keymap table
├── theme.rs           # palette tokens (05 section 7)
├── view/              # one derived-node registration per widget
│   ├── dashboard.rs
│   ├── advisor_rail.rs
│   ├── fork_layer.rs
│   ├── cards.rs
│   ├── session_log.rs
│   ├── status_bar.rs
│   ├── transcript.rs
│   ├── agents.rs, accounts.rs, palette.rs, permission.rs, help.rs
└── widgets/           # pure ratatui Widget impls over view structs
```

## 5. Core data types

Identifiers are ULIDs wrapped in newtypes (sortable, 26 chars, no coordination):

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunId(Ulid);
pub struct SessionId(Ulid);
pub struct ToolCallId(Ulid);
pub struct PermissionId(Ulid);
pub struct ForkId(u64);               // monotonic per session, matches the "forks" counter

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentName(SmolStr);        // validated by the regex in 01 section 1.1
pub struct AccountName(SmolStr);
pub struct ProviderId(&'static str);  // "anthropic", "openai", "fake"
pub struct ModelId(SmolStr);          // provider-native id
```

Agent definition:

```rust
pub struct AgentDef {
    pub name: AgentName,
    pub description: String,
    pub role: Role,
    pub tools: ToolPolicy,
    pub model: ModelRef,
    pub account: Option<AccountName>,
    pub permission_mode: PermissionMode,
    pub max_turns: Option<u32>,
    pub max_tokens_total: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub effort: Option<Effort>,
    pub skills: Vec<String>,
    pub hooks: HookSet,
    pub memory: Option<MemoryScope>,
    pub background: bool,
    pub isolation: Isolation,
    pub omit_project_rules: bool,
    pub color: Option<AgentColor>,
    pub checkpoints: Vec<CheckpointSpec>,
    pub router: Option<RouterSpec>,
    pub initial_prompt: Option<String>,
    pub auto_compact_at: Option<u32>,
    pub system_prompt: String,
    pub source: DefSource,
}

pub enum Role { Lead, Worker, Advisor, Router }
pub enum ModelRef { Alias(SmolStr), Full(ModelId), Inherit }
pub enum Isolation { None, Worktree }
pub enum MemoryScope { User, Project, Local }
pub enum PermissionMode { Default, AcceptEdits, DontAsk, Bypass, Plan }
pub enum Effort { Low, Medium, High, Max }

pub struct ToolPolicy {
    pub allow: Option<Vec<ToolPattern>>,   // None = all available for the role
    pub deny: Vec<ToolPattern>,
}
pub struct ToolPattern { pub tool: SmolStr, pub spec: Option<Glob> }  // bash(git *)

pub struct DefSource { pub path: Option<PathBuf>, pub scope: Scope }
pub enum Scope { Cli, Project, User, Extra(PathBuf), Builtin }
```

Runs:

```rust
pub struct Run {
    pub id: RunId,
    pub session: SessionId,
    pub parent: Option<RunId>,
    pub depth: u8,
    pub agent: AgentName,
    pub role: Role,
    pub model: ModelId,
    pub account: AccountName,
    pub status: RunStatus,
    pub background: bool,
    pub isolation: Isolation,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
    pub turns: u32,
    pub usage: Usage,
    pub progress: Progress,          // the card's percentage (05 section 2.6)
    pub label: SmolStr,              // "maps the repo", from the definition or the prompt's first line
    pub transcript: TranscriptRef,   // handle into the store (03 section 8)
}

pub enum RunStatus {
    Queued, Starting, Running, WaitingPermission(PermissionId), WaitingUser,
    Compacting, Completed, Partial, Failed, Cancelled, Denied,
}

pub struct Usage { pub input_tokens: u64, pub output_tokens: u64, pub cache_read: u64, pub cache_write: u64, pub cost_usd: f64 }

pub enum Progress { Indeterminate, Fraction(f32), Done }
```

Conversation:

```rust
pub struct Conversation {
    pub system: String,
    pub messages: Vec<Message>,
    pub compact_boundary: Option<usize>,   // index of the summary message
    pub usage: Usage,
}
pub struct Message { pub role: MessageRole, pub blocks: Vec<ContentBlock>, pub at: SystemTime }
pub enum MessageRole { User, Assistant, System }
pub enum ContentBlock {
    Text(String),
    Thinking { text: String, redacted: bool },
    ToolUse { id: ToolCallId, name: SmolStr, input: serde_json::Value },
    ToolResult { id: ToolCallId, content: String, is_error: bool },
    Image { media_type: SmolStr, data: Bytes },
}
```

Tools:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn spec(&self) -> ToolSpec;                       // name, description, JSON schema
    fn needs_permission(&self, input: &serde_json::Value, mode: PermissionMode) -> Need;
    async fn call(&self, cx: &ToolCx, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
}
pub enum Need { Never, Ask, Always }
pub struct ToolCx { pub run: RunId, pub cwd: PathBuf, pub cancel: CancellationToken, pub limits: ToolLimits, pub tx: mpsc::Sender<Msg> }
pub struct ToolOutput { pub content: String, pub artifacts: Vec<Artifact>, pub truncated: bool }
pub enum ToolError { Denied { reason: String }, Timeout, Io(std::io::Error), Invalid(String), Blocked { hook: String, reason: String } }
```

## 6. Error model

One error type per layer, all carrying an exit code mapping and a redaction-safe `Display`.

```rust
#[derive(Debug, thiserror::Error)]
pub enum AibusError {
    #[error("usage: {0}")]           Usage(String),                 // exit 2
    #[error("config: {0}")]          Config(#[from] ConfigError),   // exit 3
    #[error("auth: {0}")]            Auth(#[from] CredentialError), // exit 4
    #[error("provider: {0}")]        Provider(#[from] ProviderError), // exit 5
    #[error("denied: {0}")]          Denied(String),                // exit 6
    #[error("interrupted")]          Interrupted,                   // exit 7
    #[error("budget exceeded: {0}")] Budget(String),                // exit 8
    #[error("run failed: {0}")]      Run(#[from] RunError),         // exit 10
    #[error(transparent)]            Io(#[from] std::io::Error),    // exit 1
}
```

Provider errors are the ones the UI acts on:

```rust
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
    pub request_id: Option<String>,
    pub message: String,                     // already redacted
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}
pub enum ProviderErrorKind {
    Auth, RateLimit, Overloaded, ContextTooLong, InvalidRequest, ModelNotFound,
    Network, Timeout, Server, Cancelled, Unknown,
}
```

The run loop retries `retryable` errors with exponential backoff (500 ms base, 2x, 30 s cap, 5 attempts, honouring `retry_after`), then fails the run. The card shows `✗ failed · rate limit` and the transcript view offers `r` to retry when `retryable`. `ContextTooLong` triggers compaction once before counting as a failure. `Auth` never retries and surfaces an account-level banner (`account anthropic-work: 401, run aibus account test anthropic-work`).

## 7. Startup sequence

1. Parse CLI. If a subcommand that does not need the engine (`config path`, `completions`), run and exit.
2. Resolve paths (07 section 1), load config, validate.
3. Open the session index (SQLite), create or resume the session directory.
4. Build the graph: register every input and derived node (03 section 5), hydrate from snapshot if resuming.
5. Discover agent definitions, commit `AgentsReloaded`.
6. Load accounts, verify credentials exist (not that they work), commit `AccountsReloaded`.
7. Start the tokio runtime, the hook runner, the file watcher on agent directories.
8. If a TTY and not `run`: enter raw mode, alternate screen, start the render loop. Else: headless loop printing NDJSON.
9. Fire `session_start` hooks, load the latest handoff, commit.
10. Submit `initial_prompt` or the CLI prompt if any.

Shutdown reverses it: `session_end` hooks, handoff if dirty, final snapshot, flush transcripts, restore terminal. `Ctrl+c` twice within two seconds is a forced quit that still restores the terminal (a panic hook does the same).
