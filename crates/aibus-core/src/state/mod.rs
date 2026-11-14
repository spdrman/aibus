//! Node keys, namespaces and registration (03 section 5).
//!
//! Keys are `<namespace>:<entity>:<id>:<field>`. The namespaces are the layering rule from
//! invariant 8: `model:` holds facts, `ctrl:` holds UI state, `view:` holds what widgets draw,
//! `eng:` holds substrate. A `model:` or `eng:` node never depends on `ctrl:` or `view:`.

pub mod derive;
pub mod migrate;
pub mod reduce;
pub mod store;
pub mod value;

use std::fmt::Display;

use aibus_graph::{Graph, NodeKey, StoreHandle, Value};

use self::value::{list, map, n, s};

pub const NAMESPACES: [&str; 4] = ["model", "ctrl", "view", "eng"];

// model: inputs
pub const SESSION_ID: &str = "model:session:id";
pub const SESSION_TOPIC: &str = "model:session:topic";
/// What picked the lead's model, as the lead box shows it (`--model sonnet`); null on the default.
pub const SESSION_MODEL_FLAG: &str = "model:session:model_flag";
/// The side seats as the panels name them: `{ advisor: {label, enabled}, router: {label, enabled} }`.
/// The labels are null until startup says otherwise.
pub const SESSION_ASSIST: &str = "model:session:assist";
pub const AGENTS_DEFS: &str = "model:agents:defs";
pub const AGENTS_WARNINGS: &str = "model:agents:warnings";
/// `{name, provider, label, default_model, key_ref}`, no secrets.
pub const ACCOUNTS: &str = "model:accounts";
/// The workflow files as `/workflow list` shows them (and the prompt's `/fix` family reads).
pub const WORKFLOWS: &str = "model:workflows";
/// The text `/workflow show` opens: `{name, text, summary}`.
pub const WORKFLOW_SHOWN: &str = "model:workflow:shown";
/// The latest workflow the author drafted: `{toml, summary}`, or null. `/workflow save` writes it.
pub const WORKFLOW_DRAFT: &str = "model:workflow:draft";
pub const RUNS_ORDER: &str = "model:runs:order";
pub const TOOLS_PENDING: &str = "model:tools:pending";
pub const PERMISSIONS_OPEN: &str = "model:permissions:open";
/// True for the whole session when `bypass` is on, so the status bar can show `BYPASS`.
pub const PERMISSIONS_BYPASS: &str = "model:permissions:bypass";
pub const HOOKS_DUE: &str = "model:hooks:due";
pub const ROUTER_QUEUE: &str = "model:router:queue";
pub const ROUTER_FORKS: &str = "model:router:forks";
pub const ADVISOR_STATE: &str = "model:advisor:state";
pub const ADVISOR_ADVICE: &str = "model:advisor:advice";
pub const LOG: &str = "model:log";
pub const LEDGER: &str = "model:ledger";
pub const HANDOFF_LATEST: &str = "model:handoff:latest";
pub const LIMITS: &str = "model:limits";
pub const LAST_MSG: &str = "model:last_msg";
// model: deriveds
pub const ORCH_RUNNABLE: &str = "model:orchestrator:runnable";
pub const ORCH_COUNTS: &str = "model:orchestrator:counts";
pub const CHECKPOINT_PENDING: &str = "model:checkpoint:pending";
pub const PERSIST_DUE: &str = "model:persist:due";
// ctrl:
pub const FOCUS: &str = "ctrl:focus";
pub const SELECTED_CARD: &str = "ctrl:selected:card";
pub const SELECTED_LOG: &str = "ctrl:selected:log";
pub const MODAL: &str = "ctrl:modal";
pub const PALETTE_TEXT: &str = "ctrl:palette:text";
/// The modal that a permission prompt took the place of, to come back to after the answer.
pub const MODAL_UNDER: &str = "ctrl:modal:under";
/// The selected row of the open palette, agents screen or accounts screen.
pub const SCREEN_SEL: &str = "ctrl:screen:sel";
/// The open transcript's view state: `{ run, follow, top, sel, expanded, at, ver, evicted }`.
/// `top` is the first row shown while not following; `at` and `ver` are the graph time and the
/// tail version the view is stepped back to (null while live); `evicted` is the oldest time still
/// retained when a step went past it.
pub const TRANSCRIPT_UI: &str = "ctrl:transcript";
pub const LOG_FILTER: &str = "ctrl:log:filter";
/// Whether the log follows its newest row (`f`); scrolling away turns it off.
pub const LOG_FOLLOW: &str = "ctrl:log:follow";
/// The open context menu: `{ run, col, row, selected }`, null when closed.
pub const MENU: &str = "ctrl:menu";
/// Resident memory in MiB for the status bar's `mem` segment; null unless `-v` asked for it.
pub const MEM: &str = "ctrl:mem";
pub const SIZE: &str = "ctrl:size";
/// Whether the 3-column rail strip is opened to the full rail (`a`, a click on the strip).
pub const RAIL_EXPANDED: &str = "ctrl:rail:expanded";
/// Dragged sizes: `{ rail, log }`, each null until the user drags that border.
pub const LAYOUT: &str = "ctrl:layout";
// eng:
pub const TICK: &str = "eng:tick";
pub const TICKS_DROPPED: &str = "eng:ticks_dropped";
pub const MSG_SEQ: &str = "eng:msg_seq";
pub const CLOCK_MS: &str = "eng:clock_ms";
pub const SNAPSHOT_SEQ: &str = "eng:snapshot_seq";
pub const SNAPSHOT_MS: &str = "eng:snapshot_ms";
pub const RUN_SEQ: &str = "eng:run_seq";
pub const SHUTDOWN: &str = "eng:shutdown";
pub const SCHEMA_VERSION: &str = "eng:schema_version";
// view: the core only owns the one the tick timer needs; `aibus-tui` registers the rest.
pub const ANIMATING: &str = "view:animating";

/// Where the transcript screen puts its text for a terminal of this size: `(width, height)` of
/// the body. The reducer scrolls by it and `aibus-tui` draws into it, so both must agree.
pub fn transcript_body(cols: u16, rows: u16) -> (usize, usize) {
    (usize::from(cols).saturating_sub(5).max(1), usize::from(rows).saturating_sub(5).max(1))
}

/// Current value of `eng:schema_version`.
pub const SCHEMA: i64 = 1;
/// A snapshot is due after this many messages, or `PERSIST_EVERY_MS` since the last one.
pub const PERSIST_EVERY_MSGS: i64 = 64;
pub const PERSIST_EVERY_MS: i64 = 30_000;

pub const RUN_FIELDS: [&str; 9] = [
    "meta",
    "status",
    "turns",
    "usage",
    "progress",
    "blocks",
    "tail",
    "error",
    "cancel_requested",
];

/// `model:run:<id>:<field>`.
pub fn run_key(id: impl Display, field: &str) -> NodeKey {
    NodeKey::from(format!("model:run:{id}:{field}"))
}

/// `model:router:last:<kind>`.
pub fn router_last(kind: &str) -> NodeKey {
    NodeKey::from(format!("model:router:last:{kind}"))
}

/// `ctrl:scroll:<pane>`.
pub fn scroll_key(pane: &str) -> NodeKey {
    NodeKey::from(format!("ctrl:scroll:{pane}"))
}

pub fn namespace(key: &str) -> Option<&str> {
    let (ns, _) = key.split_once(':')?;
    NAMESPACES.contains(&ns).then_some(ns)
}

pub fn zero_usage() -> Value {
    map([
        ("input_tokens", n(0)),
        ("output_tokens", n(0)),
        ("cache_read", n(0)),
        ("cache_write", n(0)),
        ("cost_usd", Value::F64(0.0)),
    ])
}

fn empty_handle(store: &str) -> Value {
    Value::Handle(StoreHandle {
        store: store.into(),
        id: 0,
        version: 0,
    })
}

/// Registers the inputs of one run. Idempotent: the first registration of a key wins.
pub fn register_run<G: Graph>(g: &mut G, id: impl Display + Copy) -> Vec<NodeKey> {
    let initial = [
        Value::Null,
        s("queued"),
        n(0),
        zero_usage(),
        Value::Null,
        empty_handle("transcript"),
        empty_handle("tail"),
        Value::Null,
        Value::Bool(false),
    ];
    RUN_FIELDS
        .iter()
        .zip(initial)
        .map(|(field, v)| {
            let key = run_key(id, field);
            g.input(&key, v);
            key
        })
        .collect()
}

/// Registers every static input and derived node, and returns their keys.
pub fn register<G: Graph>(g: &mut G) -> Vec<NodeKey> {
    let inputs: Vec<(&str, Value)> = vec![
        (SESSION_ID, s("")),
        (SESSION_TOPIC, s("")),
        (SESSION_MODEL_FLAG, Value::Null),
        (
            SESSION_ASSIST,
            map([
                ("advisor", map([("label", Value::Null), ("enabled", Value::Bool(true))])),
                ("router", map([("label", Value::Null), ("enabled", Value::Bool(true))])),
            ]),
        ),
        (AGENTS_DEFS, list(vec![])),
        (AGENTS_WARNINGS, list(vec![])),
        (ACCOUNTS, list(vec![])),
        (WORKFLOWS, list(vec![])),
        (WORKFLOW_SHOWN, Value::Null),
        (WORKFLOW_DRAFT, Value::Null),
        (RUNS_ORDER, list(vec![])),
        (TOOLS_PENDING, list(vec![])),
        (PERMISSIONS_OPEN, list(vec![])),
        (PERMISSIONS_BYPASS, Value::Bool(false)),
        (HOOKS_DUE, list(vec![])),
        (ROUTER_QUEUE, list(vec![])),
        (ROUTER_FORKS, n(0)),
        (
            ADVISOR_STATE,
            map([
                ("status", s("on_call")),
                ("calls", n(0)),
                ("tokens_read", n(0)),
                ("active_checkpoint", Value::Null),
                // The tick the newest advice arrived on; the rail types it out from there.
                ("advice_tick", n(0)),
            ]),
        ),
        (ADVISOR_ADVICE, list(vec![])),
        (LOG, empty_handle("log")),
        (
            LEDGER,
            map([("goal", s("")), ("now", s("")), ("in_progress", s(""))]),
        ),
        (HANDOFF_LATEST, Value::Null),
        (
            LIMITS,
            map([
                ("max_spawn_depth", n(3)),
                ("max_concurrent_runs", n(8)),
                ("max_per_account", n(4)),
                ("max_runs_per_session", n(500)),
            ]),
        ),
        (LAST_MSG, Value::Null),
        (FOCUS, s("input")),
        (SELECTED_CARD, n(0)),
        (SELECTED_LOG, Value::Null),
        (MODAL, Value::Null),
        (PALETTE_TEXT, s("")),
        (MODAL_UNDER, Value::Null),
        (SCREEN_SEL, n(0)),
        (TRANSCRIPT_UI, Value::Null),
        (LOG_FILTER, s("")),
        (LOG_FOLLOW, Value::Bool(true)),
        (MENU, Value::Null),
        (MEM, Value::Null),
        (SIZE, map([("cols", n(80)), ("rows", n(24))])),
        (RAIL_EXPANDED, Value::Bool(false)),
        (LAYOUT, map([("rail", Value::Null), ("log", Value::Null)])),
        (TICK, n(0)),
        (TICKS_DROPPED, n(0)),
        (MSG_SEQ, n(0)),
        (CLOCK_MS, n(0)),
        (SNAPSHOT_SEQ, n(0)),
        (SNAPSHOT_MS, n(0)),
        (RUN_SEQ, n(0)),
        (SHUTDOWN, Value::Null),
        (SCHEMA_VERSION, n(SCHEMA)),
    ];
    let mut keys = Vec::new();
    for (key, v) in inputs {
        g.input(&NodeKey::from(key), v);
        keys.push(NodeKey::from(key));
    }
    for pane in ["advisor", "forks", "cards", "log", "input", "cards:x"] {
        let key = scroll_key(pane);
        g.input(&key, n(0));
        keys.push(key);
    }
    keys.extend(derive::register(g));
    keys
}
