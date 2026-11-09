# 03. State: the Causl graph

aibus keeps all application state in one Causl graph. This section says what Causl is as far as aibus is concerned, what I verified about the Rust engine, what the panel decided to do about the gap between the two, and how the graph is laid out.

## 1. The contract aibus relies on

Causl is a reactive dependency-graph engine with denotational, glitch-free, transactional semantics. The contract below is lifted from its SPEC (§3, §5, §12) and is what aibus programs against. These are invariants, not preferences; a backend that breaks one is a bug.

1. **Every value is a function of its inputs at the current `GraphTime`.** All mutation happens inside an atomic `commit(intent, writes)`; outside a commit the graph is read-only.
2. **Commits are atomic.** An error inside a commit leaves the graph byte-identical to before and subscribers do not fire. One commit creates exactly one new time or zero.
3. **Glitch-free.** A derived at time `t` is a pure function of its inputs at the same `t`. Diamond dependencies settle once; a subscriber fires exactly once per changed node per commit.
4. **Compute is pure.** No IO, no side effects, no panics in a derived's compute. Dependencies are discovered by tracked `get(node)` calls inside compute, not declared up front; stale edges are dropped on recompute.
5. **`GraphTime` advances by exactly one per commit, including `hydrate`.** Memoisation keys are `commit.time` or a per-node version counter, never pointer identity.
6. **Subscriber order** is insertion order per node, then `changed_nodes` insertion order.
7. **Batch related writes into one commit.** Many tiny commits is the anti-pattern.
8. **Strict layering.** Information model, controller state and engine substrate live in separate node-id namespaces. UI selection never becomes a model input.

The public surface aibus uses (Causl SPEC §12, the canonical seven plus the second tier):

`create`, `input`, `derived`, `commit`, `read`, `subscribe`, `explain`, and `subscribe_commits`, `commit_log`, `snapshot` / `hydrate`, `read_at` / `snapshot_at`, `simulate`, `dependencies` / `dependents`, `commit_metadata_derived`, `now`.

## 2. What I verified about the Rust engine

The coordinator pointed me at `github.com/causljs/causl-wasm`. That name returns 404 on GitHub (`gh repo view` could not resolve it). The engine exists, but its primary lives on a self-hosted Gitea: `https://git.opsite.ca/causl/causl-core-rs.git` (private, token required to clone; its README calls GitHub a downstream mirror). I read the `tools/` workspace from a local checkout of the `chore/512-merge-back` branch dated 2026-11-04. Facts:

- Workspace members: `engine-rs-core` (crate name `causl-engine-core`, lib `causl_engine_core`) and `engine-rs-bridge` (`causl-engine-bridge`, the wasm cdylib aibus does not want). `version = "0.0.0"`, `publish = false`, MIT.
- `causl-engine-core` is `#![no_std]` + `alloc`, `#![forbid(unsafe_code)]`, 26k lines, dependencies `serde`, `smol_str`, `smallvec`, `indexmap`, `rustc-hash`, `hashbrown`, plus optional `serde_json` (feature `snapshot`, on by default) and `blake3` (feature `enumerator`, off). Nothing wasm-specific. It compiles into a plain Rust binary as an `rlib`.
- Its public surface is the **state-machine layer**, not the §12 ergonomic layer:
  - `State` with `create_input()`, `create_derived()`, `seed_input_inplace(id, JsonValue)`, `get_value(id)`, `node_version(id)`, `dependencies(id)`, `dependents_transitive(id)`, `register_dep`, `read_at(id, t)`, `register_subscriber(..)`, `register_commit_observer(..)`, `commit_log_value()`, `dispose_node(id)`, `hash()`.
  - `NodeId { slot: u32, gen: u32 }`, dense slots with a generation tag. **No string ids in core**; the string-id map lives in the bridge.
  - `JsonValue` (`Null | Bool | Number(f64) | String(SmolStr) | Array | Object(IndexMap)`), the only value type.
  - `Action::CommitWithValues { intent: String, writes: Vec<(NodeId, JsonValue)> }` plus `Commit`, `Tick`, `Subscribe`, `Unsubscribe`, `Dispose`, `DispatchMsg`, `BeginFetch`, `ResolvePending`.
  - `transition_phased(&State, &Action) -> Result<(State, Vec<(PhaseStep, State)>, Vec<Event>), RaceClass>` and `transition_phased_in_place_with_callbacks<C: ComputeCallback>(..)`.
  - `trait ComputeCallback { fn compute(&self, state: &mut State, derived_id: NodeId, dep_values: &[(NodeId, &JsonValue)]) -> Result<(JsonValue, SmallVec<[NodeId; 4]>), ComputeError>; }`: the host supplies the compute body and returns the **new dependency list** explicitly. Tracked `get` is a host-layer feature.
  - `trait SubscriberCallback { fn fire_batch(&self, firings: &[SubscriberFiring]) -> Vec<ObserverError>; }`.
  - `backend.rs` declares `Transitioner` (implemented), and `Reader`, `Persister` (`snapshot`/`hydrate`) and `Observable` as `unimplemented!()` placeholders with ticket numbers.
- `State` derives `Serialize`/`Deserialize` under the `snapshot` feature, so a snapshot is possible via serde even though `Persister` is a stub.

So: the engine is real and usable from a plain binary, but (a) it is distributed only from a private Gitea at version 0.0.0 with `publish = false`, (b) it does not ship the §12 surface natively (string ids, closures, tracked dependencies, subscribe ergonomics, snapshot/hydrate), and (c) every value crossing the node boundary is a `JsonValue`.

## 3. The decision

The panel's call (DECISIONS.md D-02): **Causl contract behind a trait, two backends, in-house mini engine first, `causl-engine-core` host second.**

- `aibus-graph` defines `trait Graph` (section 4) with exactly the §12 semantics and a conformance test suite (property tests for theorems 1 to 4, diamond glitch test, subscriber-order test, atomic rollback test, hydrate-advances-time test).
- `aibus-graph::mini` is a small in-house engine (target: under 1,500 lines) implementing the trait: topological recompute with Kahn's algorithm, tracked `get`, equality cutoff, commit history ring, snapshot as serde. It exists so aibus builds from crates.io alone and so the contract is pinned by code aibus owns.
- `aibus-graph::causl` (feature `causl-rs`) implements the same trait as a **host layer** over `causl-engine-core`: a `HashMap<NodeKey, NodeId>` for string ids, a `ComputeCallback` impl that runs the registered Rust closures with a tracked-`get` recorder and returns the recorded deps, a `SubscriberCallback` that fans out to aibus subscriptions, and snapshot/hydrate via serde on `State` until `Persister` lands. The dependency is a git dependency on the Gitea remote pinned by commit, or a vendored path; both are documented in `docs/BUILD.md` (M2 deliverable).
- Both backends must pass the same conformance suite, and `cargo test --features causl-rs` runs the differential test: the same recorded `Msg` stream replayed through both engines must produce byte-identical snapshots.
- Default feature set builds `mini`. The switch to `causl-rs` as default is a milestone gate (section 11 of 11-delivery), taken when the crate is publishable or vendorable under a licence check and the differential test has been green for a release.

The Rust engineer's objection (boxing every value into `JsonValue`, the learning curve, a young crate with `unimplemented!()` in its backend traits) is real and is what the two-backend shape answers. The architecture expert's case for Causl (glitch-free derived view model, a structured audit log for free, replay-based testing, snapshot persistence that falls out of the model) is what makes it worth the cost. The performance expert's condition is section 8: heavy values never live in the graph.

## 4. The trait

```rust
pub type NodeKey = SmolStr;             // "model:run:01J...:status"
pub type GraphTime = u64;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {                        // JSON-shaped on purpose: it is what both backends can hold
    Null, Bool(bool), I64(i64), F64(f64), Str(SmolStr), Bytes(Bytes),
    List(Arc<[Value]>), Map(Arc<IndexMap<SmolStr, Value>>),
    Handle(StoreHandle),                // a reference into the side store (section 8)
}

pub struct StoreHandle { pub store: SmolStr, pub id: u64, pub version: u32 }

pub trait Graph {
    fn input(&mut self, key: &NodeKey, initial: Value) -> Node;
    fn derived(&mut self, key: &NodeKey, compute: Compute) -> Node;
    fn commit(&mut self, intent: &str, writes: &[(Node, Value)]) -> Result<Commit, CommitError>;
    fn read(&self, node: Node) -> &Value;
    fn read_key(&self, key: &str) -> Option<&Value>;
    fn subscribe(&mut self, node: Node, observer: Box<dyn FnMut(&Value, &Commit)>) -> Unsubscribe;
    fn subscribe_commits(&mut self, observer: Box<dyn FnMut(&Commit)>) -> Unsubscribe;
    fn explain(&self, node: Node) -> Explanation;
    fn commit_log(&self) -> &[Commit];
    fn snapshot(&self) -> Snapshot;
    fn hydrate(&mut self, snap: Snapshot) -> Result<Commit, HydrateError>;
    fn read_at(&self, node: Node, t: GraphTime) -> RetentionResult<Value>;
    fn snapshot_at(&self, t: GraphTime) -> RetentionResult<Snapshot>;
    fn simulate(&self, intent: &str, writes: &[(Node, Value)]) -> SimulateResult;
    fn dependencies(&self, node: Node) -> Vec<Node>;
    fn dependents(&self, node: Node) -> Vec<Node>;
    fn commit_metadata_derived(&mut self, key: &NodeKey, compute: Compute) -> Node;
    fn now(&self) -> GraphTime;
    fn node(&self, key: &str) -> Option<Node>;
    fn version(&self, node: Node) -> u32;
}

pub type Compute = Box<dyn Fn(&mut Tracker) -> Value + Send>;

pub struct Tracker<'a> { /* records every get */ }
impl Tracker<'_> {
    pub fn get(&mut self, node: Node) -> &Value;
    pub fn get_key(&mut self, key: &str) -> Option<&Value>;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node(u32);                   // dense index inside aibus-graph; the causl backend maps it to NodeId

pub struct Commit {
    pub time: GraphTime,
    pub intent: SmolStr,
    pub originated_at: Option<GraphTime>,   // set on hydrate
    pub changed_nodes: Vec<Node>,
    pub at: SystemTime,
}
```

`Value` is deliberately JSON-shaped: both backends can represent it without a custom codec, and `snapshot()` is then serde. The one addition over Causl's `JsonValue` is `Handle`, which the causl backend stores as a tagged `Object`; the mini backend stores it natively. `Bytes` is base64 on the wire.

Compute closures receive a `Tracker`, not the graph, so the only way to read inside compute is tracked. That is how invariant 4's "dependencies are discovered, not declared" is enforced by the type system.

## 5. Graph layout

Node keys are `<namespace>:<entity>:<id>:<field>`. The namespaces are the layering rule from invariant 8, enforced by a test that walks every registered key.

| Namespace | Owner | Holds |
|---|---|---|
| `model:` | `aibus-core` | facts: definitions, accounts (redacted), runs, tool calls, permissions, router forks, advisor calls, ledger/handoff state, limits |
| `ctrl:` | `aibus-tui` (and the headless driver) | focus, selection, scroll offsets, modal, palette text, filter text |
| `view:` | `aibus-tui` | derived nodes each widget renders from |
| `eng:` | `aibus-core` | substrate: clock, tick counter, persist-due flag, schema version |

Model inputs (abbreviated; the full list is `state/mod.rs`):

```
model:session:id                      Str
model:session:topic                   Str
model:agents:defs                     List<Map>        every AgentDef, serialised (no prompt bodies: Handle)
model:agents:warnings                 List<Str>
model:accounts                        List<Map>        {name, provider, label, default_model, key_ref}  NO SECRETS
model:run:<id>:meta                   Map              agent, role, parent, depth, model, account, label, started_at
model:run:<id>:status                 Str              RunStatus
model:run:<id>:turns                  I64
model:run:<id>:usage                  Map
model:run:<id>:progress               F64 | Null
model:run:<id>:blocks                 Handle           transcript block store (section 8)
model:run:<id>:tail                   Str              the streaming tail text (bounded, section 8)
model:run:<id>:error                  Map | Null
model:runs:order                      List<Str>        RunIds in creation order
model:tools:pending                   List<Map>        approved tool calls waiting for the executor
model:permissions:open                List<Map>        PermissionRequest
model:router:forks                    I64              lifetime counter
model:router:last:<kind>              Map              {choice, p, verdict, at}
model:advisor:state                   Map              {status: on_call|reading|advising, calls, tokens_read, active_checkpoint}
model:advisor:advice                  List<Str>        last N advice lines
model:log                             Handle           session log ring (section 8)
model:ledger                          Map              goal, now, in_progress
model:handoff:latest                  Map | Null
model:limits                          Map
```

Controller inputs:

```
ctrl:focus                            Str              Pane
ctrl:selected:card                    I64
ctrl:selected:log                     I64 | Null
ctrl:scroll:<pane>                    I64
ctrl:modal                            Str | Null       permission | palette | help | outcome | transcript:<run>
ctrl:palette:text                     Str
ctrl:log:filter                       Str
ctrl:size                             Map              {cols, rows}
```

Derived nodes (the interesting ones):

```
model:orchestrator:runnable           List<Map>   SpawnSpec for every Queued run whose parent is alive,
                                                  depth < max, concurrent < limit, account slots free
model:orchestrator:counts             Map         {running, queued, done, failed}  -> status bar "subagents [3/3]"
model:checkpoint:pending              Str | Null  the next checkpoint to consult the advisor on
model:persist:due                     Bool        true when commits since last snapshot >= 64 or 30 s elapsed
view:advisor_rail                     Map         everything the rail draws
view:fork_layer                       Map         forks count, three rows with p/verdict, split highlight
view:cards                            List<Map>   one per worker run: name, model, pct, label, status glyph, border colour
view:session_log:lines                List<Map>   last N log rows after filter, styled spans
view:status_bar                       Map         four segments
view:transcript:<run>:lines           Handle      wrapped lines for the open transcript (section 8)
view:dirty                            List<Str>   which widgets changed this commit (derived from changed_nodes via
                                                  commit_metadata_derived)
```

Every widget is a pure function of exactly one `view:` node. `aibus-tui` has a test that asserts no widget reads a `model:` node directly.

## 6. The loop, restated in graph terms

```rust
pub fn step(app: &mut App, msg: Msg) -> Result<(), AibusError> {
    let writes = reduce(&app.graph, &msg);                 // pure
    let commit = app.graph.commit(msg.name(), &writes)?;   // one commit per Msg
    app.dirty.extend(app.graph.read_key("view:dirty"));    // widgets to redraw
    app.effects.after_commit(&app.graph, &commit);         // IO happens here
    app.persist.after_commit(&app.graph, &commit);         // commit log append, snapshot when due
    Ok(())
}
```

The commit log is the audit trail. Each `Commit` carries the `Msg` name as intent, and `reduce` adds a `model:last_msg` write with a redacted summary of the `Msg` payload (ids, counts, lengths; never text content). `aibus session log <id>` prints it. `explain(view:cards)` answers "why does this card say 98%" with the input chain.

Time travel: `read_at(node, t)` and `snapshot_at(t)` are what the transcript view's `[` / `]` keys use to step the dashboard back through the session ("what did the fork layer say at 23:48:21"). Retention is bounded (section 9), so stepping past the window shows `evicted, oldest retained t=...`.

Replay: `msgs.jsonl` in the session dir records every `Msg` (redacted the same way). `aibus session replay <id> --provider fake` reconstructs the graph by replaying the stream against the fake provider and prints a snapshot hash; tests diff that hash against the recorded one (10 section 5).

## 7. Side effects stay outside

Invariant 4 forbids IO inside compute, so the engine's effects executor (02 section 3.2) is the only reader-with-side-effects. The boundary is explicit:

| Inside the graph | Outside, in `Effects` |
|---|---|
| `model:orchestrator:runnable` says which runs may start | `spawn_run` starts them and sends `RunStarted` |
| `model:tools:pending` lists approved tool calls | `ToolExecutor` runs them and sends `ToolCallFinished` |
| `model:hooks:due` lists hook invocations | `HookRunner` spawns the processes |
| `model:router:queue` lists questions | the router account is called |
| `model:checkpoint:pending` names a checkpoint | the advisor packet is built from the store and sent |
| `model:persist:due` | snapshot and commit-log flush |
| `model:run:<id>:cancel_requested` | `CancellationToken::cancel()` |

Each effect is keyed by an id and the executor tracks what it already started, so re-reading a derived after an unrelated commit does not duplicate work.

## 8. Heavy values and streaming

The performance expert's condition. Transcripts, wrapped lines and the session log would blow the graph up if they were node values (every commit clones changed values; a snapshot would carry megabytes). So:

- **The store.** `aibus_core::state::store::Store` is a side arena, not a graph node: `HashMap<u64, Arc<Blocks>>` with a version counter per entry. The graph holds `Value::Handle { store: "transcript", id, version }`. A derived that needs the content reads the handle through the `Tracker` (so the dependency is tracked and the version bump is the change signal) and then looks the content up in the store, which is passed into compute by reference (`Tracker::store()`), read-only. The store is append-mostly: `Blocks` is `Vec<Arc<Block>>` where every block but the last is immutable.
- **Streaming tokens.** Provider chunks are coalesced per run into one `Msg::Stream` per frame (16 ms) by the run task. `reduce` writes `model:run:<id>:tail` (the in-progress assistant text, capped at 64 KiB; beyond that the oldest 32 KiB are sealed into a block) and bumps the handle version only when a block seals. The transcript derived `view:transcript:<run>:lines` keeps, in the store, a wrapped-line cache per sealed block keyed by `(block_id, width)`; on recompute it re-wraps only the tail string and the blocks whose cache entry is missing for the current width. For a 100k-line transcript, a token arrival re-wraps a few hundred bytes.
- **Session log.** A ring of 2,000 rows in the store; `model:log` is a handle whose version bumps per append; `view:session_log:lines` reads the last `rows` entries after the filter.

The result: a stream commit touches `model:run:<id>:tail`, `model:run:<id>:usage`, maybe `model:log`, and the derived nodes downstream of those (the open transcript, the card, the status bar). Nothing else recomputes.

## 9. History, retention and snapshot bounds

- `commit_history_cap = 4096` commits retained in memory for `read_at`/`snapshot_at`. Older commits are evicted from the graph (the result is `RetentionResult::Evicted { oldest_retained }`) but are already in `commits.jsonl` on disk.
- Retention buffers hold the changed values per commit. Because heavy values are handles, a retained commit costs tens of bytes per changed node, so 4096 commits stay under 4 MB even for busy sessions.
- A snapshot is the input set plus `now`. With handles instead of content it is a few hundred KB for a day-long session; the store's sealed blocks are persisted separately as the transcript JSONL files they already are, and a hydrate rebuilds handles by scanning those files.
- `eng:schema_version` is an input. `hydrate` of a snapshot with an older version runs `state::migrate::chain(from, to)` on the raw `Snapshot` before hydration; each migration is a pure `fn(Snapshot) -> Result<Snapshot, MigrateError>` and the chain is tested by hydrating every fixture in `fixtures/snapshots/v*/`.

## 10. Security note

Credentials are never graph inputs. `model:accounts` holds `{name, provider, label, key_ref}` where `key_ref` is `"keyring:aibus/<name>"` or `"env:AIBUS_ACCOUNT_<NAME>_KEY"`. The `CredentialStore` (04 section 5) resolves refs at request time on the worker thread; the secret lives in a `Secret<String>` that never implements `Serialize` and whose `Debug` prints `***`. Because the secret is not in the graph, `snapshot()`, `commit_log()`, `msgs.jsonl` and `explain()` cannot leak it. The redaction layer (09 section 4) is the second line, not the first.

## 11. Per-commit budget

Stated here, measured in 08: a commit with recompute of every affected derived must finish in under 4 ms at p99 on a session with 100k transcript lines, 8 runs and 2,000 log rows, on a 2020 laptop. That leaves 12 ms of a 60 fps frame for ratatui's diff and the terminal write. The benchmark is `benches/commit_stream.rs` and it is a CI gate.
