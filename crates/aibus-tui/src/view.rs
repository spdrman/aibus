//! The `view:` nodes (03 section 5). Each widget draws exactly one of them; they are the only
//! place that reads `model:` and `ctrl:` nodes on the way to the screen.

use aibus_core::state::store::Store;
use aibus_core::state::value::{as_i64, as_list, as_str, get, list, map, n, s};
use aibus_core::state::{
    router_last, run_key, ADVISOR_ADVICE, ADVISOR_STATE, CHECKPOINT_PENDING, LEDGER, LOG,
    LOG_FILTER, PERMISSIONS_OPEN, ROUTER_FORKS, RUNS_ORDER, SELECTED_CARD, TICK,
};
use aibus_core::RunStatus;
use aibus_graph::{Graph, NodeKey, Tracker, Value};

pub const HEADER: &str = "view:header";
pub const RAIL: &str = "view:advisor_rail";
pub const LEAD_BOX: &str = "view:lead_box";
pub const FORK_LAYER: &str = "view:fork_layer";
pub const CARDS: &str = "view:cards";
pub const RETURN_BOX: &str = "view:return_box";
pub const LOG_LINES: &str = "view:session_log:lines";
pub const STATUS_BAR: &str = "view:status_bar";
pub const DIRTY: &str = "view:dirty";

/// Widget name and the one `view:` node it draws. `view:dirty` lists the names.
pub const WIDGETS: [(&str, &str); 8] = [
    ("header", HEADER),
    ("rail", RAIL),
    ("lead_box", LEAD_BOX),
    ("fork_layer", FORK_LAYER),
    ("cards", CARDS),
    ("return_box", RETURN_BOX),
    ("log", LOG_LINES),
    ("status_bar", STATUS_BAR),
];

/// The router questions the fork layer shows, as `(kind, row label)`.
const FORK_KINDS: [(&str, &str); 3] = [
    ("which_file", "which file"),
    ("which_tool", "which tool"),
    ("retry_or_stop", "retry or stop"),
];

/// Log rows kept in `view:session_log:lines`; the widget shows as many as fit.
const LOG_LINES_KEPT: usize = 24;

fn runs<'a>(t: &mut Tracker<'a>, role: &str) -> Vec<(&'a str, &'a Value)> {
    let Some(order) = t.get_key(RUNS_ORDER) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in as_list(order).iter().filter_map(as_str) {
        if let Some(meta) = t.get_key(&run_key(id, "meta")) {
            if get(meta, "role").and_then(as_str) == Some(role) {
                out.push((id, meta));
            }
        }
    }
    out
}

fn status<'a>(t: &mut Tracker<'a>, id: &str) -> Option<RunStatus> {
    as_str(t.get_key(&run_key(id, "status"))?).and_then(RunStatus::parse)
}

fn checkpoint<'a>(t: &mut Tracker<'a>) -> Option<&'a str> {
    t.get_key(CHECKPOINT_PENDING).and_then(as_str)
}

fn field(v: &Value, key: &str, default: &str) -> Value {
    get(v, key).cloned().unwrap_or_else(|| s(default))
}

fn is_running(st: Option<RunStatus>) -> bool {
    st.is_some_and(RunStatus::is_active)
        || matches!(st, Some(RunStatus::Compacting | RunStatus::Starting))
}

fn lead_box(t: &mut Tracker) -> Value {
    let Some((_, m)) = runs(t, "lead").into_iter().next() else {
        return Value::Null;
    };
    map([
        ("model_label", field(m, "model", "")),
        ("label", field(m, "label", "")),
        ("effort", field(m, "effort", "high")),
        ("model_flag", get(m, "model_flag").cloned().unwrap_or(Value::Null)),
        ("highlight", Value::Bool(checkpoint(t) == Some("before_plan"))),
    ])
}

fn cards(t: &mut Tracker) -> Value {
    let tick = t.get_key(TICK).and_then(as_i64).unwrap_or(0);
    let selected = t.get_key(SELECTED_CARD).and_then(as_i64).unwrap_or(0);
    let mut out = Vec::new();
    for (i, (id, m)) in runs(t, "worker").into_iter().enumerate() {
        let st = status(t, id);
        let state = match st {
            Some(RunStatus::Completed | RunStatus::Partial) => "done",
            Some(RunStatus::Failed | RunStatus::Cancelled | RunStatus::Denied) => "failed",
            Some(RunStatus::Queued) | None => "queued",
            Some(_) => "running",
        };
        let progress = match (state, t.get_key(&run_key(id, "progress"))) {
            ("done", _) => Value::F64(1.0),
            (_, Some(p @ Value::F64(_))) => p.clone(),
            _ => Value::Null,
        };
        // The phase is per card, seeded by the run id, and only moves while the card runs.
        let seed = id.bytes().fold(0i64, |a, b| a + i64::from(b));
        let phase = if is_running(st) { (tick + seed) % 4 } else { 0 };
        out.push(map([
            ("run_id", s(id)),
            ("name", field(m, "agent", "")),
            ("model_label", field(m, "model", "")),
            ("progress", progress),
            ("label", field(m, "label", "")),
            ("status", s(state)),
            ("spinner_phase", n(phase)),
            ("border", s("swarm")),
            ("selected", Value::Bool(selected == i as i64)),
        ]));
    }
    list(out)
}

fn fork_layer(t: &mut Tracker) -> Value {
    let mut any_split = false;
    let mut rows = Vec::new();
    for (kind, label) in FORK_KINDS {
        let last = t.get_key(&router_last(kind)).cloned().unwrap_or(Value::Null);
        let verdict = get(&last, "verdict").and_then(as_str).unwrap_or("");
        any_split |= verdict == "split";
        rows.push(map([
            ("label", s(label)),
            ("p", get(&last, "p").cloned().unwrap_or(Value::Null)),
            ("verdict", s(verdict)),
        ]));
    }
    map([
        ("title", s("ROUTER · fork layer")),
        ("forks", t.get_key(ROUTER_FORKS).cloned().unwrap_or(n(0))),
        ("rows", list(rows)),
        ("any_split", Value::Bool(any_split)),
        ("enabled", Value::Bool(true)),
    ])
}

fn advisor_state<'a>(t: &mut Tracker<'a>) -> Value {
    t.get_key(ADVISOR_STATE).cloned().unwrap_or(Value::Null)
}

fn status_bar(t: &mut Tracker) -> Value {
    let workers = runs(t, "worker");
    let total = workers.len();
    let running = workers
        .iter()
        .filter(|(id, _)| is_running(status(t, id)))
        .count();
    let swarm_label = workers
        .first()
        .and_then(|(_, m)| get(m, "model").and_then(as_str))
        .unwrap_or("swarm")
        .to_owned();
    let adv = advisor_state(t);
    let adv_state = get(&adv, "status").and_then(as_str).unwrap_or("on_call");
    let adv_word = match adv_state {
        "on_call" => "on call",
        "reading" => "reading",
        _ => "advising",
    };
    let goal = t.get_key(LEDGER).and_then(|l| get(l, "goal"));
    let now = t.get_key(LEDGER).and_then(|l| get(l, "now"));
    let ledger = match goal.and_then(as_str) {
        Some(g) if !g.is_empty() => map([
            ("goal", s(g)),
            ("now", now.cloned().unwrap_or_else(|| s(""))),
        ]),
        _ => Value::Null,
    };
    let waiting = t.get_key(PERMISSIONS_OPEN).map_or(0, |p| as_list(p).len());
    let alerts = if waiting > 0 {
        vec![s(&format!("! {waiting} permission waiting (p)"))]
    } else {
        vec![]
    };
    map([
        (
            "swarm",
            map([
                ("label", s(&swarm_label)),
                ("state", s(if running > 0 { "swarming" } else { "idle" })),
            ]),
        ),
        ("subagents", map([("running", n(running)), ("total", n(total))])),
        (
            "advisor",
            map([("label", s("advisor")), ("state", s(adv_word))]),
        ),
        (
            "router",
            map([
                ("label", s("router")),
                ("forks", t.get_key(ROUTER_FORKS).cloned().unwrap_or(n(0))),
            ]),
        ),
        ("ledger", ledger),
        ("alerts", list(alerts)),
    ])
}

fn rail(store: &Store, t: &mut Tracker) -> Value {
    let adv = advisor_state(t);
    let active = checkpoint(t);
    let status = get(&adv, "status").and_then(as_str).unwrap_or("on_call");
    let subtitle = match status {
        "on_call" => "advisor · on call",
        "reading" => "advisor · reading",
        _ => "advisor · advising",
    };
    let marks = [
        ("before a plan", "before_plan"),
        ("same error x2", "same_error_x2"),
        ("before done", "before_done"),
    ]
    .map(|(name, key)| {
        map([
            ("name", s(name)),
            ("active", Value::Bool(active == Some(key))),
        ])
    });
    let advice: Vec<Value> = t
        .get_key(ADVISOR_ADVICE)
        .map(as_list)
        .unwrap_or_default()
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(|a| match a {
            Value::Handle(h) => s(store.text(h).as_deref().unwrap_or("")),
            other => other.clone(),
        })
        .collect();
    map([
        ("title", s("ADVISOR")),
        ("subtitle", s(subtitle)),
        ("checkpoints", list(marks.to_vec())),
        ("advice", list(advice)),
        ("calls", get(&adv, "calls").cloned().unwrap_or(n(0))),
        ("tokens_read", get(&adv, "tokens_read").cloned().unwrap_or(n(0))),
        ("status", s(status)),
    ])
}

fn return_box(t: &mut Tracker) -> Value {
    let label = runs(t, "lead")
        .first()
        .map_or_else(|| s(""), |(_, m)| field(m, "model", ""));
    map([
        ("lead_model_label", label),
        ("highlight", Value::Bool(checkpoint(t) == Some("before_done"))),
        ("pulse", Value::Bool(false)),
    ])
}

fn header(t: &mut Tracker) -> Value {
    let lead = runs(t, "lead")
        .first()
        .and_then(|(_, m)| get(m, "model").and_then(as_str).map(str::to_owned));
    let swarm = runs(t, "worker")
        .first()
        .and_then(|(_, m)| get(m, "model").and_then(as_str).map(str::to_owned));
    let adv = advisor_state(t);
    let advisor_present = checkpoint(t).is_some()
        || get(&adv, "calls").and_then(as_i64).unwrap_or(0) > 0;
    let forks = t.get_key(ROUTER_FORKS).and_then(as_i64).unwrap_or(0);
    let entry = |text: String, role: &str| map([("text", s(&text)), ("role", s(role))]);
    let mut legend = Vec::new();
    if let Some(l) = &lead {
        legend.push(entry(format!("{l} · lead"), "lead"));
    }
    if let Some(w) = &swarm {
        legend.push(entry(format!("{w} · swarm"), "swarm"));
    }
    if forks > 0 {
        legend.push(entry("router · forks".to_owned(), "router"));
    }
    if advisor_present {
        legend.push(entry("advisor".to_owned(), "advisor"));
    }
    map([
        ("lead_label", lead.map_or(Value::Null, |l| s(&l))),
        ("swarm_label", swarm.map_or(Value::Null, |w| s(&w))),
        ("legend", list(legend)),
    ])
}

fn clock(ms: i64) -> String {
    let day = (ms / 1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", day / 3600, day / 60 % 60, day % 60)
}

fn session_log(store: &Store, t: &mut Tracker) -> Value {
    // The handle's version is the change signal; the rows live in the store.
    t.get_key(LOG);
    let filter = t.get_key(LOG_FILTER).and_then(as_str).unwrap_or("");
    let rows = store
        .log_rows(LOG_LINES_KEPT * 4)
        .into_iter()
        .filter_map(|r| {
            let text = get(&r, "text").and_then(as_str)?.to_owned();
            let actor = get(&r, "level").and_then(as_str).unwrap_or("info").to_owned();
            if !filter.is_empty() && !text.contains(filter) && !actor.contains(filter) {
                return None;
            }
            let at = get(&r, "at").and_then(as_i64).unwrap_or(0);
            Some(map([
                ("time", s(&clock(at))),
                ("actor", s(&actor)),
                ("text", s(&text)),
            ]))
        })
        .collect::<Vec<_>>();
    let skip = rows.len().saturating_sub(LOG_LINES_KEPT);
    list(rows.into_iter().skip(skip).collect())
}

/// Registers the `view:` nodes and `view:dirty`, and returns their keys.
///
/// The router rows are inputs the core creates on the first answer; registering them here
/// first means the fork layer is already subscribed when that answer arrives.
pub fn register<G: Graph>(g: &mut G, store: Store) -> Vec<NodeKey> {
    let mut keys = Vec::new();
    for (kind, _) in FORK_KINDS {
        let key = router_last(kind);
        g.input(&key, Value::Null);
        keys.push(key);
    }
    let mut nodes = Vec::new();
    for (name, key) in WIDGETS {
        let st = store.clone();
        let compute: aibus_graph::Compute = match name {
            "header" => Box::new(header),
            "rail" => Box::new(move |t| rail(&st, t)),
            "lead_box" => Box::new(lead_box),
            "fork_layer" => Box::new(fork_layer),
            "cards" => Box::new(cards),
            "return_box" => Box::new(return_box),
            "log" => Box::new(move |t| session_log(&st, t)),
            _ => Box::new(status_bar),
        };
        let key = NodeKey::from(key);
        nodes.push((g.derived(&key, compute), name));
        keys.push(key);
    }
    let dirty = NodeKey::from(DIRTY);
    g.commit_metadata_derived(
        &dirty,
        Box::new(move |t| {
            let changed = t.commit().map(|c| c.changed_nodes.as_slice()).unwrap_or(&[]);
            list(
                nodes
                    .iter()
                    .filter(|(node, _)| changed.contains(node))
                    .map(|(_, name)| s(name))
                    .collect(),
            )
        }),
    );
    keys.push(dirty);
    keys
}
