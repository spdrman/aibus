//! The `view:` nodes (03 section 5). Each widget draws exactly one of them; they are the only
//! place that reads `model:` and `ctrl:` nodes on the way to the screen.

use aibus_core::state::store::Store;
use aibus_core::state::value::{as_i64, as_list, as_str, get, list, map, n, s, with};
use aibus_core::state::{
    router_last, run_key, ADVISOR_ADVICE, ADVISOR_STATE, AGENTS_DEFS, CHECKPOINT_PENDING,
    CLOCK_MS, LEDGER, LOG, LOG_FILTER, MEM, MENU, PERMISSIONS_BYPASS, PERMISSIONS_OPEN,
    ROUTER_FORKS, RUNS_ORDER, SELECTED_CARD, SESSION_ASSIST, SESSION_MODEL_FLAG, TICK,
};
use aibus_core::RunStatus;
use aibus_graph::{Graph, NodeKey, Tracker, Value};

pub const HEADER: &str = "view:header";
pub const RAIL: &str = "view:advisor_rail";
pub const LEAD_BOX: &str = "view:lead_box";
pub const DELEGATE_ROW: &str = "view:delegate_row";
pub const FORK_LAYER: &str = "view:fork_layer";
pub const CARDS: &str = "view:cards";
pub const RETURN_BOX: &str = "view:return_box";
pub const LOG_LINES: &str = "view:session_log:lines";
pub const STATUS_BAR: &str = "view:status_bar";
/// What a tooltip says about each run: the things a card or row cuts off.
pub const RUN_TIPS: &str = "view:run_tips";
/// The context menu: the run it is for and which items apply.
pub const MENU_VIEW: &str = "view:menu";
pub const DIRTY: &str = "view:dirty";

/// Widget name and the one `view:` node it draws. `view:dirty` lists the names.
pub const WIDGETS: [(&str, &str); 21] = [
    ("header", HEADER),
    ("rail", RAIL),
    ("lead_box", LEAD_BOX),
    ("delegate_row", DELEGATE_ROW),
    ("fork_layer", FORK_LAYER),
    ("cards", CARDS),
    ("return_box", RETURN_BOX),
    ("log", LOG_LINES),
    ("status_bar", STATUS_BAR),
    ("tips", RUN_TIPS),
    ("menu", MENU_VIEW),
    ("permission", crate::views::PERMISSION),
    ("palette", crate::views::PALETTE),
    ("agents", crate::views::AGENTS),
    ("accounts", crate::views::ACCOUNTS_VIEW),
    ("models", crate::views::MODELS),
    ("providers", crate::views::PROVIDERS),
    ("transcript", crate::views::TRANSCRIPT),
    ("run_status", crate::views::RUN_STATUS),
    ("cost", crate::views::COST),
    ("workflows", crate::views::WORKFLOWS_VIEW),
];

/// The router questions the fork layer shows, as `(kind, row label)`.
pub(crate) const FORK_KINDS: [(&str, &str); 3] = [
    ("which_file", "which file"),
    ("which_tool", "which tool"),
    ("retry_or_stop", "retry or stop"),
];

/// Log rows kept in `view:session_log:lines`; the widget shows as many as fit.
const LOG_LINES_KEPT: usize = 128;

/// A result that reached the lead this recently (ms) still counts as in flight.
const IN_FLIGHT_MS: i64 = 3000;

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

/// The workers that have a card: not one dismissed with `x`, and not a finished one that a later
/// run of the same agent replaced (05 section 2.6). The reducer's `visible_workers` is the same
/// rule, so the selected index means the same run on both sides.
fn shown_workers<'a>(t: &mut Tracker<'a>) -> Vec<(&'a str, &'a Value)> {
    let all = runs(t, "worker");
    let mut out = Vec::new();
    for (i, (id, m)) in all.iter().enumerate() {
        let dismissed = matches!(get(m, "dismissed"), Some(Value::Bool(true)));
        let finished = status(t, id).is_some_and(RunStatus::is_terminal);
        let agent = get(m, "agent").and_then(as_str);
        let replaced = all[i + 1..].iter().any(|(_, later)| get(later, "agent").and_then(as_str) == agent);
        if !(dismissed || (finished && replaced)) {
            out.push((*id, *m));
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

/// `Haiku 5.5` as `haiku`: the short name the status bar and the log use.
fn short(label: &str) -> String {
    label.split_whitespace().next().unwrap_or("").to_lowercase()
}

/// A side seat as startup named it: its label (null when nobody chose one) and whether it is on.
fn seat<'a>(t: &mut Tracker<'a>, name: &str) -> (Option<&'a str>, bool) {
    let seat = t.get_key(SESSION_ASSIST).and_then(|a| get(a, name));
    let label = seat.and_then(|x| get(x, "label")).and_then(as_str).filter(|l| !l.is_empty());
    let on = !matches!(seat.and_then(|x| get(x, "enabled")), Some(Value::Bool(false)));
    (label, on)
}

fn lead_box(t: &mut Tracker) -> Value {
    let Some((_, m)) = runs(t, "lead").into_iter().next() else {
        return Value::Null;
    };
    map([
        ("model_label", field(m, "model", "")),
        ("label", field(m, "label", "")),
        ("effort", field(m, "effort", "high")),
        ("model_flag", t.get_key(SESSION_MODEL_FLAG).cloned().unwrap_or(Value::Null)),
        ("highlight", Value::Bool(checkpoint(t) == Some("before_plan"))),
    ])
}

/// The worker a `same_error_x2` checkpoint is about: the newest one that has failed.
fn failing_worker(t: &mut Tracker) -> Option<String> {
    if checkpoint(t) != Some("same_error_x2") {
        return None;
    }
    let mut found = None;
    for (id, _) in runs(t, "worker") {
        let errored = !matches!(t.get_key(&run_key(id, "error")), None | Some(Value::Null));
        if errored || status(t, id) == Some(RunStatus::Failed) {
            found = Some(id.to_owned());
        }
    }
    found
}

/// A block of `width` cells bouncing between the ends of a bar `span` cells wide.
fn bounce(x: i64, span: i64) -> i64 {
    let period = (2 * span).max(1);
    let m = x.rem_euclid(period);
    if m <= span {
        m
    } else {
        period - m
    }
}

fn cards(t: &mut Tracker) -> Value {
    let tick = t.get_key(TICK).and_then(as_i64).unwrap_or(0);
    let selected = t.get_key(SELECTED_CARD).and_then(as_i64).unwrap_or(0);
    let failing = failing_worker(t);
    let mut out = Vec::new();
    for (i, (id, m)) in shown_workers(t).into_iter().enumerate() {
        let st = status(t, id);
        let state = match st {
            Some(RunStatus::Completed | RunStatus::Partial) => "done",
            Some(RunStatus::Failed | RunStatus::Cancelled | RunStatus::Denied) => "failed",
            Some(RunStatus::Queued) | None => "queued",
            Some(_) => "running",
        };
        // The phase is per card, seeded by the run id, and only moves while the card runs.
        let seed = id.bytes().fold(0i64, |a, b| a + i64::from(b));
        let progress = match (state, t.get_key(&run_key(id, "progress"))) {
            ("done", _) => map([("kind", s("done")), ("value", Value::F64(1.0))]),
            (_, Some(Value::F64(p))) => map([("kind", s("fraction")), ("value", Value::F64(*p))]),
            _ => map([("kind", s("indeterminate")), ("value", n(bounce(tick + seed, 8)))]),
        };
        let phase = if is_running(st) { (tick + seed) % 4 } else { 0 };
        let border = if failing.as_deref() == Some(id) { "advisor" } else { "swarm" };
        out.push(map([
            ("run_id", s(id)),
            ("name", field(m, "agent", "")),
            ("model_label", field(m, "model", "")),
            ("progress", progress),
            ("label", field(m, "label", "")),
            ("status", s(state)),
            ("spinner_phase", n(phase)),
            ("border", s(border)),
            ("selected", Value::Bool(selected == i as i64)),
        ]));
    }
    list(out)
}

fn delegate_row(t: &mut Tracker) -> Value {
    let tick = t.get_key(TICK).and_then(as_i64).unwrap_or(0);
    let workers = shown_workers(t);
    let label = workers.first().map_or_else(|| s(""), |(_, m)| field(m, "model", ""));
    let mut dots = Vec::new();
    for (id, _) in workers {
        let seed = id.bytes().fold(0i64, |a, b| a + i64::from(b));
        // A dot travels only towards a card that is working; -1 is no dot.
        dots.push(n(if is_running(status(t, id)) { (tick + seed).rem_euclid(1000) } else { -1 }));
    }
    map([("swarm_model_label", label), ("connectors", list(dots))])
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
    let (label, enabled) = seat(t, "router");
    let title = label.map_or_else(|| "ROUTER · fork layer".to_owned(), |l| format!("{} · fork layer", l.to_uppercase()));
    map([
        ("title", s(&title)),
        ("forks", t.get_key(ROUTER_FORKS).cloned().unwrap_or(n(0))),
        ("rows", list(rows)),
        ("any_split", Value::Bool(any_split)),
        ("enabled", Value::Bool(enabled)),
    ])
}

/// The advice the rail shows (the newest two), how many characters of the newest are typed out
/// by now (one per tick since it arrived), and whether it is still being typed.
fn advice(store: &Store, t: &mut Tracker) -> (Vec<String>, usize, bool) {
    let tick = t.get_key(TICK).and_then(as_i64).unwrap_or(0);
    let since = t
        .get_key(ADVISOR_STATE)
        .and_then(|a| get(a, "advice_tick"))
        .and_then(as_i64)
        .unwrap_or(0);
    let lines: Vec<String> = t
        .get_key(ADVISOR_ADVICE)
        .map(as_list)
        .unwrap_or_default()
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(|a| match a {
            Value::Handle(h) => store.text(h).as_deref().unwrap_or("").to_owned(),
            other => as_str(other).unwrap_or("").to_owned(),
        })
        .collect();
    let len = lines.last().map_or(0, |l| l.chars().count());
    let cursor = usize::try_from((tick - since).max(0)).unwrap_or(0).min(len);
    (lines, cursor, cursor < len)
}

/// The advisor's state word: `reading` while a checkpoint's packet is read, `advising` while the
/// advice is typed out, `on_call` otherwise, `off` when the seat is not running.
fn advisor_word(t: &mut Tracker, typing: bool) -> &'static str {
    if !seat(t, "advisor").1 {
        "off"
    } else if checkpoint(t).is_some() {
        "reading"
    } else if typing {
        "advising"
    } else {
        "on_call"
    }
}

/// Whether the rail is still typing: the loop keeps ticking until it is done.
pub fn rail_typing(rail: &Value) -> bool {
    let cursor = get(rail, "typing_cursor").and_then(as_i64).unwrap_or(0);
    let newest = as_list(get(rail, "advice").unwrap_or(&Value::Null))
        .last()
        .and_then(as_str)
        .map_or(0, |a| a.chars().count());
    usize::try_from(cursor).unwrap_or(0) < newest
}

fn status_bar(store: &Store, t: &mut Tracker) -> Value {
    let workers = shown_workers(t);
    let total = workers.len();
    let running = workers
        .iter()
        .filter(|(id, _)| is_running(status(t, id)))
        .count();
    let swarm_label = workers
        .first()
        .and_then(|(_, m)| get(m, "model").and_then(as_str))
        .map_or_else(|| "swarm".to_owned(), short);
    let (_, _, typing) = advice(store, t);
    let adv_word = match advisor_word(t, typing) {
        "on_call" => "on call",
        other => other,
    };
    let adv_label = seat(t, "advisor").0.map_or_else(|| "advisor".to_owned(), short);
    let (router_label, router_on) = seat(t, "router");
    let goal = t.get_key(LEDGER).and_then(|l| get(l, "goal"));
    let now = t.get_key(LEDGER).and_then(|l| get(l, "now"));
    let text = |v: Option<&Value>| match v {
        Some(Value::Handle(h)) => store.text(h).map(|t| t.to_string()).unwrap_or_default(),
        other => other.and_then(as_str).unwrap_or("").to_owned(),
    };
    let (goal, now) = (text(goal), text(now));
    let ledger = if goal.is_empty() {
        Value::Null
    } else {
        map([("goal", s(&goal)), ("now", s(&now))])
    };
    let waiting = t.get_key(PERMISSIONS_OPEN).map_or(0, |p| as_list(p).len());
    let mut alerts = Vec::new();
    if matches!(t.get_key(PERMISSIONS_BYPASS), Some(Value::Bool(true))) {
        alerts.push(map([("kind", s("bypass")), ("text", s("BYPASS"))]));
    }
    if waiting > 0 {
        alerts.push(map([
            ("kind", s("permission")),
            ("text", s(&format!("! {waiting} permission waiting (p)"))),
        ]));
    }
    let mem = match t.get_key(MEM).and_then(as_i64) {
        Some(mib) => s(&format!("mem {mib}M")),
        None => Value::Null,
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
        ("advisor", map([("label", s(&adv_label)), ("state", s(adv_word))])),
        (
            "router",
            map([
                ("label", s(&router_label.map_or_else(|| "router".to_owned(), short))),
                ("forks", t.get_key(ROUTER_FORKS).cloned().unwrap_or(n(0))),
                ("enabled", Value::Bool(router_on)),
            ]),
        ),
        ("ledger", ledger),
        ("alerts", list(alerts)),
        ("mem", mem),
        ("cost", s(&crate::views::dollars(crate::views::session_cost(t)))),
    ])
}

fn rail(store: &Store, t: &mut Tracker) -> Value {
    let adv = t.get_key(ADVISOR_STATE).cloned().unwrap_or(Value::Null);
    let active = checkpoint(t);
    let (advice, cursor, typing) = advice(store, t);
    let status = advisor_word(t, typing);
    let subtitle = match status {
        "off" => "advisor · off",
        "reading" => "advisor · reading",
        "advising" => "advisor · advising",
        _ => "advisor · on call",
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
    let title = seat(t, "advisor").0.map_or_else(|| "ADVISOR".to_owned(), str::to_uppercase);
    map([
        ("title", s(&title)),
        ("subtitle", s(subtitle)),
        ("checkpoints", list(marks.to_vec())),
        ("advice", list(advice.iter().map(|a| s(a)).collect())),
        ("typing_cursor", n(cursor)),
        ("calls", get(&adv, "calls").cloned().unwrap_or(n(0))),
        ("tokens_read", get(&adv, "tokens_read").cloned().unwrap_or(n(0))),
        ("status", s(status)),
    ])
}

fn return_box(t: &mut Tracker) -> Value {
    let clock = t.get_key(CLOCK_MS).and_then(as_i64).unwrap_or(0);
    let lead = runs(t, "lead").into_iter().next();
    let label = lead.map_or_else(|| s(""), |(_, m)| field(m, "model", ""));
    let lead_going = lead.is_some_and(|(id, _)| is_running(status(t, id)));
    // A result is in flight while the lead is still going and a worker has just ended.
    let mut recent = false;
    for (_, m) in runs(t, "worker") {
        let ended = get(m, "ended_at").and_then(as_i64);
        recent |= ended.is_some_and(|e| clock - e <= IN_FLIGHT_MS && clock >= e);
    }
    map([
        ("lead_model_label", label),
        ("highlight", Value::Bool(checkpoint(t) == Some("before_done"))),
        ("pulse", Value::Bool(lead_going && recent)),
    ])
}

fn header(t: &mut Tracker) -> Value {
    let lead = runs(t, "lead")
        .first()
        .and_then(|(_, m)| get(m, "model").and_then(as_str).map(str::to_owned));
    let swarm = runs(t, "worker")
        .first()
        .and_then(|(_, m)| get(m, "model").and_then(as_str).map(str::to_owned));
    let (adv_label, adv_on) = seat(t, "advisor");
    let adv = t.get_key(ADVISOR_STATE).cloned().unwrap_or(Value::Null);
    let advisor_present = adv_on
        && (checkpoint(t).is_some() || get(&adv, "calls").and_then(as_i64).unwrap_or(0) > 0);
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
        legend.push(entry(
            adv_label.map_or_else(|| "advisor".to_owned(), |l| format!("{l} · advisor")),
            "advisor",
        ));
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
    let filter = t.get_key(LOG_FILTER).and_then(as_str).unwrap_or("").to_lowercase();
    let lead_agent = runs(t, "lead").first().and_then(|(_, m)| get(m, "agent").and_then(as_str).map(str::to_owned));
    let router_name = seat(t, "router").0.map_or_else(|| "router".to_owned(), short);
    let advisor_name = seat(t, "advisor").0.map_or_else(|| "advisor".to_owned(), str::to_lowercase);
    let rows = store
        .log_rows(LOG_LINES_KEPT * 4)
        .into_iter()
        .filter_map(|r| {
            let text = get(&r, "text").and_then(as_str)?.to_owned();
            let level = get(&r, "level").and_then(as_str).unwrap_or("info");
            let said = get(&r, "actor").and_then(as_str).unwrap_or("");
            let (actor, colour) = match (said, level) {
                ("router", _) => (router_name.clone(), "router"),
                ("advisor", _) => (advisor_name.clone(), "advisor"),
                (a, _) if !a.is_empty() && Some(a) == lead_agent.as_deref() => (a.to_owned(), "lead"),
                (a, _) if !a.is_empty() => (a.to_owned(), "swarm"),
                (_, "error") => ("error".to_owned(), "error"),
                (_, "warn") => ("warn".to_owned(), "warn"),
                _ => ("aibus".to_owned(), "dim"),
            };
            if !filter.is_empty()
                && !text.to_lowercase().contains(&filter)
                && !actor.to_lowercase().contains(&filter)
            {
                return None;
            }
            let at = get(&r, "at").and_then(as_i64).unwrap_or(0);
            let row = map([
                ("time", s(&clock(at))),
                ("actor", s(&actor)),
                ("actor_colour", s(colour)),
                ("text", s(&text)),
                ("p", get(&r, "p").cloned().unwrap_or(Value::Null)),
                ("verdict", get(&r, "verdict").cloned().unwrap_or(Value::Null)),
                ("run", get(&r, "run").cloned().unwrap_or(Value::Null)),
            ]);
            // A row a client of `aibus mcp` caused (#274) carries its badge; the rest keep their shape.
            Some(match get(&r, "via") {
                Some(via) => with(&row, "via", via.clone()),
                None => row,
            })
        })
        .collect::<Vec<_>>();
    let skip = rows.len().saturating_sub(LOG_LINES_KEPT);
    list(rows.into_iter().skip(skip).collect())
}

/// What a tooltip says about each run: the full agent name and description, the model and
/// account, the cost so far, the id.
fn run_tips(t: &mut Tracker) -> Value {
    let Some(order) = t.get_key(RUNS_ORDER) else {
        return list(Vec::new());
    };
    let defs = t.get_key(AGENTS_DEFS).map(as_list).unwrap_or_default();
    let mut out = Vec::new();
    for id in as_list(order).iter().filter_map(as_str) {
        let Some(meta) = t.get_key(&run_key(id, "meta")) else { continue };
        let agent = get(meta, "agent").and_then(as_str).unwrap_or("");
        let description = defs
            .iter()
            .find(|d| get(d, "name").and_then(as_str) == Some(agent))
            .and_then(|d| get(d, "description"))
            .and_then(as_str)
            .unwrap_or("");
        let cost = t
            .get_key(&run_key(id, "usage"))
            .and_then(|u| get(u, "cost_usd"))
            .and_then(|c| match c {
                Value::F64(x) => Some(*x),
                Value::I64(x) => Some(*x as f64),
                _ => None,
            })
            .unwrap_or(0.0);
        out.push(map([
            ("run_id", s(id)),
            ("name", s(agent)),
            ("description", s(description)),
            ("model", field(meta, "model", "")),
            ("account", field(meta, "account", "")),
            ("cost", s(&format!("${cost:.4}"))),
        ]));
    }
    list(out)
}

/// The open context menu with each item marked as applying or not. Null when closed.
fn menu(t: &mut Tracker) -> Value {
    let Some(m) = t.get_key(MENU).filter(|m| !matches!(m, Value::Null)).cloned() else {
        return Value::Null;
    };
    let id = get(&m, "run").and_then(as_str).unwrap_or("").to_owned();
    let st = status(t, &id);
    let name = t
        .get_key(&run_key(&id, "meta"))
        .and_then(|meta| get(meta, "agent"))
        .and_then(as_str)
        .unwrap_or("")
        .to_owned();
    let live = st.is_some_and(|x| x.is_active() || x == RunStatus::Queued);
    let items: Vec<Value> = crate::commands::menu_items().into_iter().map(|(item, label)| {
        let enabled = match item {
            "cancel" => live,
            "resume" => st == Some(RunStatus::Partial),
            "copy_result" => matches!(st, Some(RunStatus::Completed | RunStatus::Partial)),
            "fork" => st.is_some(),
            _ => true,
        };
        map([("id", s(item)), ("label", s(label)), ("enabled", Value::Bool(enabled))])
    }).collect();
    map([
        ("run_id", s(&id)),
        ("title", s(&name)),
        ("col", get(&m, "col").cloned().unwrap_or(n(0))),
        ("row", get(&m, "row").cloned().unwrap_or(n(0))),
        ("selected", get(&m, "selected").cloned().unwrap_or(n(0))),
        ("items", list(items)),
    ])
}

/// Registers `view:transcript:<run>:lines` for the transcript that is open, once. The node is a
/// store handle that moves a version when the text does (03 section 8); the screen draws from
/// `view:transcript`, which reads the same two run handles.
pub fn ensure_lines<G: Graph>(g: &mut G) -> Option<NodeKey> {
    let run = g.read_key(aibus_core::state::MODAL).and_then(as_str)?.strip_prefix("transcript:")?.to_owned();
    let key = NodeKey::from(crate::views::lines_key(&run));
    if g.node(&key).is_none() {
        g.derived(&key, Box::new(move |t| crate::views::lines(t, &run)));
        return Some(key);
    }
    None
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
            "delegate_row" => Box::new(delegate_row),
            "fork_layer" => Box::new(fork_layer),
            "cards" => Box::new(cards),
            "return_box" => Box::new(return_box),
            "log" => Box::new(move |t| session_log(&st, t)),
            "tips" => Box::new(run_tips),
            "menu" => Box::new(menu),
            "permission" => Box::new(move |t| crate::views::permission(&st, t)),
            "palette" => Box::new(crate::views::palette),
            "agents" => Box::new(move |t| crate::views::agents(&st, t)),
            "accounts" => Box::new(crate::views::accounts),
            "models" => Box::new(crate::views::models),
            "providers" => Box::new(crate::views::providers),
            "transcript" => Box::new(move |t| crate::views::transcript(&st, t)),
            "run_status" => Box::new(crate::views::run_status),
            "cost" => Box::new(crate::views::cost),
            "workflows" => Box::new(move |t| crate::views::workflows(&st, t)),
            _ => Box::new(move |t| status_bar(&st, t)),
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
