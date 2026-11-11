//! The `view:` nodes of the screens beyond the dashboard (05 section 3): the permission modal, the
//! palette, the agents and accounts screens and the transcript. Same rule as `view.rs`: a widget
//! draws exactly one of them and sees nothing but its `Value`.

use aibus_core::state::store::{Row, Store};
use aibus_core::state::value::{as_i64, as_list, as_str, get, list, map, n, s};
use aibus_core::state::{
    router_last, run_key, transcript_body, ADVISOR_STATE, AGENTS_DEFS, AGENTS_WARNINGS, ACCOUNTS, CLOCK_MS, MODAL,
    PALETTE_TEXT, PERMISSIONS_OPEN, ROUTER_FORKS, RUNS_ORDER, SCREEN_SEL, SESSION_ASSIST, SIZE, TRANSCRIPT_UI, WORKFLOWS,
    WORKFLOW_SHOWN,
};
use aibus_core::{ModelTarget, RunStatus};
use aibus_graph::{Tracker, Value};

pub const PERMISSION: &str = "view:permission";
pub const PALETTE: &str = "view:palette";
pub const AGENTS: &str = "view:agents";
pub const ACCOUNTS_VIEW: &str = "view:accounts";
pub const TRANSCRIPT: &str = "view:transcript";
pub const RUN_STATUS: &str = "view:run_status";
pub const COST: &str = "view:cost";
pub const MODELS: &str = "view:models";
pub const PROVIDERS: &str = "view:providers";
pub const WORKFLOWS_VIEW: &str = "view:workflows";

/// The palette's fuzzy match: every character of `needle` in `hay`, in order. A run of
/// consecutive characters and a match at the start of a word score higher. `None` is no match.
pub fn fuzzy(needle: &str, hay: &str) -> Option<i64> {
    let hay: Vec<char> = hay.to_lowercase().chars().collect();
    let mut at = 0;
    let (mut score, mut last) = (0i64, None::<usize>);
    for c in needle.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (at..hay.len()).find(|&i| hay[i] == c)?;
        score += 10;
        if last.is_some_and(|l| l + 1 == found) {
            score += 15;
        }
        if found == 0 || !hay[found - 1].is_alphanumeric() {
            score += 8;
        }
        score -= i64::try_from(found - at).unwrap_or(0).min(8);
        last = Some(found);
        at = found + 1;
    }
    Some(score)
}

fn text_of(store: &Store, v: &Value) -> String {
    match v {
        Value::Handle(h) => store.text(h).map(|t| t.to_string()).unwrap_or_default(),
        other => as_str(other).unwrap_or("").to_owned(),
    }
}

fn short(id: &str) -> String {
    format!("{}…", id.chars().take(8).collect::<String>())
}

pub fn permission(store: &Store, t: &mut Tracker) -> Value {
    let Some(open) = t.get_key(PERMISSIONS_OPEN).map(as_list).filter(|o| !o.is_empty()) else {
        return Value::Null;
    };
    let first = &open[0];
    let run = get(first, "run").and_then(as_str).unwrap_or("").to_owned();
    let meta = t.get_key(&run_key(&run, "meta")).cloned().unwrap_or(Value::Null);
    let role = get(&meta, "role").and_then(as_str).unwrap_or("");
    map([
        ("id", get(first, "id").cloned().unwrap_or(Value::Null)),
        ("agent", get(first, "agent").cloned().unwrap_or(s(""))),
        ("run", s(&run)),
        ("run_short", s(&short(&run))),
        ("tool", get(first, "tool").cloned().unwrap_or(s(""))),
        // A workflow write asks every time, so the modal does not offer "always" for it.
        ("always", Value::Bool(!get(first, "tool").and_then(as_str).is_some_and(aibus_core::mcp::asks_every_time))),
        ("summary", s(&{
            // The gate's line is `<tool>: <input>`; the modal names the tool on its own line.
            let line = get(first, "summary").map(|v| text_of(store, v)).unwrap_or_default();
            let tool = get(first, "tool").and_then(as_str).unwrap_or("");
            line.strip_prefix(&format!("{tool}: ")).map_or_else(|| line.clone(), str::to_owned)
        })),
        ("index", n(1)),
        ("of", n(open.len())),
        // A run other than the lead's is named in the prompt (Claude Code does the same).
        ("background", Value::Bool(role != "lead")),
    ])
}

/// A palette row before it is ranked: `(id, kind, label, hint)`.
type Entry = (String, &'static str, String, String);

pub fn palette(t: &mut Tracker) -> Value {
    if t.get_key(MODAL).and_then(as_str) != Some("palette") {
        return Value::Null;
    }
    let text = t.get_key(PALETTE_TEXT).and_then(as_str).unwrap_or("").to_owned();
    let mut all: Vec<Entry> = Vec::new();
    for (id, label, hint) in crate::commands::palette_rows() {
        all.push((format!("cmd:{id}"), "command", label.to_owned(), hint.to_owned()));
    }
    for d in t.get_key(AGENTS_DEFS).map(as_list).unwrap_or_default() {
        let name = get(d, "name").and_then(as_str).unwrap_or("");
        all.push((format!("agent:{name}"), "agent", format!("delegate to {name}"), get(d, "role").and_then(as_str).unwrap_or("").to_owned()));
    }
    let order: Vec<String> = t.get_key(RUNS_ORDER).map(as_list).unwrap_or_default().iter().filter_map(as_str).map(str::to_owned).collect();
    for id in order.iter().rev() {
        let meta = t.get_key(&run_key(id, "meta")).cloned().unwrap_or(Value::Null);
        let agent = get(&meta, "agent").and_then(as_str).unwrap_or("");
        let status = t.get_key(&run_key(id, "status")).and_then(as_str).unwrap_or("").to_owned();
        all.push((format!("run:{id}"), "run", format!("run {agent} {}", short(id)), status));
    }
    let mut scored: Vec<(i64, usize, &Entry)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, it)| if text.is_empty() { Some((0, i, it)) } else { fuzzy(&text, &it.2).map(|sc| (sc, i, it)) })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let items: Vec<Value> = scored
        .into_iter()
        .take(60)
        .map(|(_, _, (id, kind, label, hint))| map([("id", s(id)), ("kind", s(kind)), ("label", s(label)), ("hint", s(hint))]))
        .collect();
    let sel = t.get_key(SCREEN_SEL).and_then(as_i64).unwrap_or(0).clamp(0, items.len().saturating_sub(1) as i64);
    map([("text", s(&text)), ("sel", n(sel)), ("items", list(items))])
}

pub fn agents(store: &Store, t: &mut Tracker) -> Value {
    if t.get_key(MODAL).and_then(as_str) != Some("agents") {
        return Value::Null;
    }
    let defs = t.get_key(AGENTS_DEFS).map(as_list).unwrap_or_default().to_vec();
    let sel = t.get_key(SCREEN_SEL).and_then(as_i64).unwrap_or(0).clamp(0, defs.len().saturating_sub(1) as i64);
    let rows: Vec<Value> = defs
        .iter()
        .map(|d| {
            let f = |k: &str| get(d, k).cloned().unwrap_or_else(|| s(""));
            map([
                ("name", f("name")),
                ("scope", f("scope")),
                ("model", f("model")),
                ("tools", f("tools")),
                ("role", f("role")),
                ("description", f("description")),
                ("path", get(d, "path").cloned().unwrap_or(Value::Null)),
            ])
        })
        .collect();
    let preview = defs
        .get(sel as usize)
        .and_then(|d| get(d, "prompt"))
        .map(|p| text_of(store, p))
        .unwrap_or_default();
    let preview: Vec<Value> = preview.lines().take(40).map(s).collect();
    let warnings = t.get_key(AGENTS_WARNINGS).map(as_list).unwrap_or_default();
    map([
        ("rows", list(rows)),
        ("sel", n(sel)),
        ("preview", list(preview)),
        ("warnings", n(warnings.len())),
        ("first_warning", warnings.first().cloned().unwrap_or(Value::Null)),
    ])
}

/// `view:workflows`: the rows of `/workflow list` with the selection, or the text of `/workflow
/// show` as lines with its summary. Null unless one of those two modals is open.
pub fn workflows(store: &Store, t: &mut Tracker) -> Value {
    let modal = t.get_key(MODAL).and_then(as_str).map(str::to_owned);
    match modal.as_deref() {
        Some("workflows") => {
            let rows = t.get_key(WORKFLOWS).map(as_list).unwrap_or_default().to_vec();
            let sel = t.get_key(SCREEN_SEL).and_then(as_i64).unwrap_or(0).clamp(0, rows.len().saturating_sub(1) as i64);
            map([("rows", list(rows)), ("sel", n(sel))])
        }
        Some("workflow_show") => {
            let shown = t.get_key(WORKFLOW_SHOWN).cloned().unwrap_or(Value::Null);
            let body = get(&shown, "text").map(|x| text_of(store, x)).unwrap_or_default();
            let word = |k: &str| get(&shown, k).cloned().unwrap_or_else(|| s(""));
            let lines: Vec<Value> = body.lines().take(400).map(s).collect();
            map([("name", word("name")), ("lines", list(lines)), ("summary", word("summary"))])
        }
        _ => Value::Null,
    }
}

pub fn accounts(t: &mut Tracker) -> Value {
    if t.get_key(MODAL).and_then(as_str) != Some("accounts") {
        return Value::Null;
    }
    let accounts = t.get_key(ACCOUNTS).map(as_list).unwrap_or_default().to_vec();
    let sel = t.get_key(SCREEN_SEL).and_then(as_i64).unwrap_or(0).clamp(0, accounts.len().saturating_sub(1) as i64);
    let rows: Vec<Value> = accounts
        .iter()
        .map(|a| {
            let key = get(a, "key_ref").and_then(as_str).unwrap_or("none");
            let (status, source) = match key.split_once(':') {
                None => ("no key", "none"),
                Some((src, _)) => ("ready", src),
            };
            map([
                ("name", get(a, "name").cloned().unwrap_or_else(|| s(""))),
                ("provider", get(a, "provider").cloned().unwrap_or_else(|| s(""))),
                ("label", get(a, "label").cloned().unwrap_or_else(|| s(""))),
                ("model", get(a, "default_model").cloned().unwrap_or(Value::Null)),
                ("status", s(status)),
                ("key_source", s(source)),
            ])
        })
        .collect();
    map([("rows", list(rows)), ("sel", n(sel))])
}

/// `$3.00`, `$0.25`, `$0.075`: two decimals, a third only when the price has one.
fn usd(price: Option<&Value>) -> String {
    match price {
        Some(Value::F64(x)) => {
            let long = format!("{x:.3}");
            format!("${}", long.strip_suffix('0').unwrap_or(&long))
        }
        _ => "n/a".to_owned(),
    }
}

/// `200k`, `1M`: a context window in the room a column has.
fn window(tokens: i64) -> String {
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        format!("{}M", tokens / 1_000_000)
    } else {
        format!("{}k", tokens / 1000)
    }
}

/// `view:models`: one row per `(account, model)` the connected accounts serve, for the target the
/// screen was opened for. A lead or a worker needs tools, so a model without them is not offered;
/// the advisor has no requirement; the router prefers logprobs and shows the rest dimmed.
/// `{target, rows, sel, text, hidden}`; each row has the `ref` that `SetModel` takes
/// (`account/model`, which pins the account).
pub fn models(t: &mut Tracker) -> Value {
    let Some(target) = t.get_key(MODAL).and_then(as_str).and_then(|m| m.strip_prefix("models:")).and_then(ModelTarget::parse) else {
        return Value::Null;
    };
    let text = t.get_key(PALETTE_TEXT).and_then(as_str).unwrap_or("").to_owned();
    // What the target runs on now, to mark its row.
    let run = match target {
        ModelTarget::Run(r) => Some(r.to_string()),
        ModelTarget::Lead => {
            let order: Vec<String> = t.get_key(RUNS_ORDER).map(as_list).unwrap_or_default().iter().filter_map(as_str).map(str::to_owned).collect();
            order
                .into_iter()
                .find(|id| t.get_key(&run_key(id, "meta")).and_then(|m| get(m, "role")).and_then(as_str) == Some("lead"))
        }
        _ => None,
    };
    let now: Option<(String, Option<String>)> = match (&run, target) {
        (Some(run), _) => {
            let meta = t.get_key(&run_key(run, "meta")).cloned().unwrap_or(Value::Null);
            let model = get(&meta, "model").and_then(as_str).map(str::to_owned);
            model.map(|m| (m, get(&meta, "account").and_then(as_str).map(str::to_owned)))
        }
        (None, ModelTarget::Advisor) => t
            .get_key(SESSION_ASSIST)
            .and_then(|a| get(a, "advisor"))
            .and_then(|x| get(x, "label"))
            .and_then(as_str)
            .map(|m| (m.to_owned(), None)),
        _ => None,
    };
    let accounts = t.get_key(ACCOUNTS).map(as_list).unwrap_or_default().to_vec();
    let mut rows: Vec<(i64, Value)> = Vec::new();
    let mut hidden = 0;
    for a in &accounts {
        let account = get(a, "name").and_then(as_str).unwrap_or("");
        for m in get(a, "models").map(as_list).unwrap_or_default() {
            let flag = |k: &str| matches!(get(m, k), Some(Value::Bool(true)));
            if matches!(target, ModelTarget::Lead | ModelTarget::Run(_)) && !flag("tools") {
                hidden += 1;
                continue;
            }
            let id = get(m, "id").and_then(as_str).unwrap_or("");
            let alias = get(m, "alias").and_then(as_str).unwrap_or("");
            let score = if text.is_empty() { Some(0) } else { fuzzy(&text, &format!("{alias} {id} {account}")) };
            let Some(score) = score else { continue };
            let caps: String = [(flag("tools"), 'T'), (flag("thinking"), 'R'), (flag("images"), 'I'), (flag("cache"), 'C')]
                .iter()
                .map(|(on, c)| if *on { *c } else { '-' })
                .collect();
            let current = now.as_ref().is_some_and(|(model, acct)| model == id && acct.as_deref().is_none_or(|x| x == account));
            rows.push((
                score,
                map([
                    ("ref", s(&format!("{account}/{id}"))),
                    ("alias", s(alias)),
                    ("id", s(id)),
                    ("account", s(account)),
                    ("context", s(&window(get(m, "context").and_then(as_i64).unwrap_or(0)))),
                    ("input", s(&usd(get(m, "input")))),
                    ("output", s(&usd(get(m, "output")))),
                    ("caps", s(&caps)),
                    ("dim", Value::Bool(target == ModelTarget::Router && !flag("logprobs"))),
                    ("current", Value::Bool(current)),
                ]),
            ));
        }
    }
    // Typing a filter ranks; with none, the accounts' own order stands.
    if !text.is_empty() {
        rows.sort_by(|a, b| b.0.cmp(&a.0));
    }
    let sel = t.get_key(SCREEN_SEL).and_then(as_i64).unwrap_or(0).clamp(0, rows.len().saturating_sub(1) as i64);
    map([
        ("target", s(&target.label())),
        ("text", s(&text)),
        ("sel", n(sel)),
        ("hidden", n(hidden)),
        ("rows", list(rows.into_iter().map(|r| r.1).collect())),
    ])
}


/// `view:providers`: the providers an account can be added for, one row each. They are the
/// built-in presets (`aibus_provider::presets`), so a new vendor is a line of data there.
pub fn providers(t: &mut Tracker) -> Value {
    if t.get_key(MODAL).and_then(as_str) != Some("providers") {
        return Value::Null;
    }
    let all = aibus_provider::presets::presets();
    let sel = t.get_key(SCREEN_SEL).and_then(as_i64).unwrap_or(0).clamp(0, all.len().saturating_sub(1) as i64);
    let rows = all.iter().map(|p| {
        let keyless = p.auth_methods.iter().all(|m| m == "none");
        map([
            ("id", s(&p.id)),
            ("api", s(p.api.map_or("", |a| a.as_str()))),
            ("url", s(p.base_url.as_deref().unwrap_or("your own"))),
            ("auth", s(if keyless { "none" } else { "key" })),
        ])
    });
    map([("rows", list(rows.collect())), ("sel", n(sel))])
}

fn row_value(row: &Row, selected: Option<i64>) -> Value {
    match row {
        Row::Text(text) => map([("kind", s("text")), ("text", s(text))]),
        Row::User(text) => map([("kind", s("user")), ("text", s(text))]),
        Row::Detail(text) => map([("kind", s("detail")), ("text", s(text))]),
        Row::Compact(tokens) => map([("kind", s("compact")), ("tokens", n(*tokens))]),
        Row::Note { error, text } => map([("kind", s("note")), ("error", Value::Bool(*error)), ("text", s(text))]),
        Row::Tool { ordinal, tool, expanded } => {
            let done = tool.done.as_ref();
            map([
                ("kind", s("tool")),
                ("ordinal", n(*ordinal)),
                ("name", s(&tool.name)),
                ("summary", s(&tool.summary)),
                ("expanded", Value::Bool(*expanded)),
                ("selected", Value::Bool(selected == Some(*ordinal as i64))),
                // `null` while the call is still running.
                ("ok", done.map_or(Value::Null, |d| Value::Bool(d.ok))),
                ("ms", done.map_or(Value::Null, |d| n(d.ms))),
                ("bytes", done.map_or(Value::Null, |d| n(d.bytes))),
            ])
        }
    }
}

/// The open transcript: its header, and the rows that fit the body at the current scroll.
pub fn transcript(store: &Store, t: &mut Tracker) -> Value {
    let Some(run) = t.get_key(MODAL).and_then(as_str).and_then(|m| m.strip_prefix("transcript:")).map(str::to_owned) else {
        return Value::Null;
    };
    let ui = t.get_key(TRANSCRIPT_UI).cloned().unwrap_or(Value::Null);
    let entry = match (t.get_key(&run_key(&run, "blocks")), t.get_key(&run_key(&run, "tail"))) {
        (Some(Value::Handle(h)), Some(Value::Handle(_))) => h.id,
        _ => 0,
    };
    let meta = t.get_key(&run_key(&run, "meta")).cloned().unwrap_or(Value::Null);
    let status = t.get_key(&run_key(&run, "status")).and_then(as_str).unwrap_or("").to_owned();
    let error = t.get_key(&run_key(&run, "error")).cloned().unwrap_or(Value::Null);
    let size = t.get_key(SIZE).cloned().unwrap_or(Value::Null);
    let dim = |k: &str, d: i64| get(&size, k).and_then(as_i64).unwrap_or(d).clamp(0, 9999) as u16;
    let (width, height) = transcript_body(dim("cols", 80), dim("rows", 24));
    let expanded: Vec<String> = get(&ui, "expanded").map(as_list).unwrap_or_default().iter().filter_map(as_str).map(str::to_owned).collect();
    let ver = get(&ui, "ver").and_then(as_i64).and_then(|v| u32::try_from(v).ok());
    let follow = !matches!(get(&ui, "follow"), Some(Value::Bool(false)));
    let top = get(&ui, "top").and_then(as_i64).unwrap_or(0).max(0) as usize;
    let window = if follow { None } else { Some(top) };
    let rows = store.transcript_rows(entry, width, &expanded, ver, (window, height));
    let sel = get(&ui, "sel").and_then(as_i64);
    let st = RunStatus::parse(&status);
    let retryable = st == Some(RunStatus::Failed) && matches!(get(&error, "retryable"), Some(Value::Bool(true)));
    let live = st.is_some_and(|x| !x.is_terminal());
    let meta_s = |k: &str| get(&meta, k).and_then(as_str).unwrap_or("").to_owned();
    map([
        ("run", s(&run)),
        ("agent", s(&meta_s("agent"))),
        ("role", s(&meta_s("role"))),
        ("model", s(&meta_s("model"))),
        ("account", s(&meta_s("account"))),
        ("fallback_from", s(&meta_s("fallback_from"))),
        ("status", s(&status)),
        ("live", Value::Bool(live)),
        ("retryable", Value::Bool(retryable)),
        ("partial", Value::Bool(st == Some(RunStatus::Partial))),
        ("error", get(&error, "message").cloned().unwrap_or(Value::Null)),
        ("total", n(rows.total)),
        ("first", n(rows.first)),
        ("height", n(height)),
        ("follow", Value::Bool(follow)),
        ("tools", n(rows.tools)),
        ("at", get(&ui, "at").cloned().unwrap_or(Value::Null)),
        ("evicted", get(&ui, "evicted").cloned().unwrap_or(Value::Null)),
        // The finished run's content is on disk and is on its way back.
        ("loading", Value::Bool(entry != 0 && store.is_evicted(entry))),
        ("rows", list(rows.rows.iter().map(|r| row_value(r, sel)).collect())),
    ])
}

/// `view:transcript:<run>:lines`: the run's lines as a store handle, so a stream commit moves a
/// version and not a copy of the text (03 section 8). Registered when a transcript opens.
pub fn lines_key(run: &str) -> String {
    format!("view:transcript:{run}:lines")
}

pub fn lines(t: &mut Tracker, run: &str) -> Value {
    let (blocks, tail) = (t.get_key(&run_key(run, "blocks")), t.get_key(&run_key(run, "tail")));
    let (Some(Value::Handle(b)), Some(Value::Handle(tl))) = (blocks, tail) else { return Value::Null };
    let size = t.get_key(SIZE).cloned().unwrap_or(Value::Null);
    let dim = |k: &str, d: i64| get(&size, k).and_then(as_i64).unwrap_or(d).clamp(0, 9999) as u16;
    let (width, _) = transcript_body(dim("cols", 80), dim("rows", 24));
    map([
        (
            "lines",
            Value::Handle(aibus_graph::StoreHandle {
                store: "lines".into(),
                id: b.id,
                version: b.version.wrapping_mul(31).wrapping_add(tl.version),
            }),
        ),
        ("width", n(width)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_needs_the_letters_in_order_and_prefers_tight_matches() {
        assert!(fuzzy("agt", "agents screen").is_some());
        assert!(fuzzy("tga", "agents screen").is_none());
        let tight = fuzzy("agent", "agents screen").unwrap();
        let loose = fuzzy("agent", "a great enterprise").unwrap();
        assert!(tight > loose, "{tight} vs {loose}");
        assert_eq!(fuzzy("", "anything"), Some(0));
        assert!(fuzzy("ACC", "accounts screen").is_some(), "case does not matter");
        assert!(fuzzy("a c", "accounts screen").is_some(), "spaces in the needle are ignored");
        let word_start = fuzzy("s", "agents screen").unwrap();
        let mid = fuzzy("g", "agents screen").unwrap();
        assert!(word_start != mid);
    }
}

/// `$0.0123`, the one way a cost is written in the status modal, the cost table and the tooltips.
pub fn dollars(x: f64) -> String {
    format!("${x:.4}")
}

/// One run's cost so far.
fn run_cost(t: &mut Tracker, id: &str) -> f64 {
    match t.get_key(&run_key(id, "usage")).and_then(|u| get(u, "cost_usd")) {
        Some(Value::F64(x)) => *x,
        Some(Value::I64(x)) => *x as f64,
        _ => 0.0,
    }
}

/// What the whole session has cost. The cost table's total and the status bar's `cost` field are
/// both this sum, so they cannot disagree.
pub fn session_cost(t: &mut Tracker) -> f64 {
    let ids: Vec<String> = t.get_key(RUNS_ORDER).map(as_list).unwrap_or_default().iter().filter_map(as_str).map(str::to_owned).collect();
    ids.iter().map(|id| run_cost(t, id)).sum()
}

/// `view:run_status`: the rows of the `/status` modal, `{title, rows: [{k, v}]}`. Null unless the
/// modal is `status:<run>`, `status:advisor` or `status:router`.
pub fn run_status(t: &mut Tracker) -> Value {
    let Some(what) = t.get_key(MODAL).and_then(as_str).and_then(|m| m.strip_prefix("status:")).map(str::to_owned) else {
        return Value::Null;
    };
    let row = |k: &str, v: String| map([("k", s(k)), ("v", s(&v))]);
    let text = |v: Option<&Value>| v.and_then(as_str).unwrap_or("").to_owned();
    let count = |v: Option<&Value>| v.and_then(as_i64).unwrap_or(0);
    fn seat(t: &mut Tracker, name: &str) -> String {
        t.get_key(SESSION_ASSIST).and_then(|a| get(a, name)).and_then(|x| get(x, "label")).and_then(as_str).unwrap_or("").to_owned()
    }
    if what == "advisor" {
        let adv = t.get_key(ADVISOR_STATE).cloned().unwrap_or(Value::Null);
        let rows = vec![
            row("model", seat(t, "advisor")),
            row("state", text(get(&adv, "status"))),
            row("checkpoints", count(get(&adv, "calls")).to_string()),
            row("tokens read", count(get(&adv, "tokens_read")).to_string()),
            row("at checkpoint", text(get(&adv, "active_checkpoint"))),
        ];
        return map([("title", s("advisor")), ("rows", list(rows))]);
    }
    if what == "router" {
        let answered = crate::view::FORK_KINDS
            .iter()
            .filter(|(k, _)| t.get_key(&router_last(k)).is_some_and(|v| !matches!(v, Value::Null)))
            .count();
        let rows = vec![
            row("model", seat(t, "router")),
            row("answers so far", count(t.get_key(ROUTER_FORKS)).to_string()),
            row("question kinds answered", answered.to_string()),
        ];
        return map([("title", s("router")), ("rows", list(rows))]);
    }
    let meta = t.get_key(&run_key(&what, "meta")).cloned().unwrap_or(Value::Null);
    let status = text(t.get_key(&run_key(&what, "status")));
    let turns = count(t.get_key(&run_key(&what, "turns")));
    let usage = t.get_key(&run_key(&what, "usage")).cloned().unwrap_or(Value::Null);
    let cost = run_cost(t, &what);
    let clock = count(t.get_key(CLOCK_MS));
    // A run still going is measured to the clock, a finished one to when it ended.
    let elapsed = get(&meta, "started_at").and_then(as_i64).map_or_else(
        || "not started".to_owned(),
        |from| {
            let secs = (get(&meta, "ended_at").and_then(as_i64).unwrap_or(clock) - from).max(0) / 1000;
            if secs >= 60 { format!("{}m {:02}s", secs / 60, secs % 60) } else { format!("{secs}s") }
        },
    );
    let parent = match get(&meta, "parent").and_then(as_str) {
        Some(p) => {
            let name = text(t.get_key(&run_key(p, "meta")).and_then(|m| get(m, "agent")));
            format!("{name} {}", short(p))
        }
        None => "none".to_owned(),
    };
    let rows = vec![
        row("agent", text(get(&meta, "agent"))),
        row("role", text(get(&meta, "role"))),
        row("model", text(get(&meta, "model"))),
        row("account", text(get(&meta, "account"))),
        row("status", status),
        row("parent", parent),
        row("depth", count(get(&meta, "depth")).to_string()),
        row("turns", turns.to_string()),
        row("input tokens", count(get(&usage, "input_tokens")).to_string()),
        row("output tokens", count(get(&usage, "output_tokens")).to_string()),
        row("cache read", count(get(&usage, "cache_read")).to_string()),
        row("cache write", count(get(&usage, "cache_write")).to_string()),
        row("cost", dollars(cost)),
        row("elapsed", elapsed),
    ];
    map([("title", s(&format!("{} {}", text(get(&meta, "agent")), short(&what)))), ("rows", list(rows))])
}

/// `view:cost`: `{rows: [{agent, run, status, cost}], total}` sorted by cost, dearest first.
/// Null unless the modal is `cost`.
pub fn cost(t: &mut Tracker) -> Value {
    if t.get_key(MODAL).and_then(as_str) != Some("cost") {
        return Value::Null;
    }
    let ids: Vec<String> = t.get_key(RUNS_ORDER).map(as_list).unwrap_or_default().iter().filter_map(as_str).map(str::to_owned).collect();
    let mut rows: Vec<(f64, Value)> = ids
        .iter()
        .map(|id| {
            let c = run_cost(t, id);
            let agent = t.get_key(&run_key(id, "meta")).and_then(|m| get(m, "agent")).and_then(as_str).unwrap_or("").to_owned();
            let status = t.get_key(&run_key(id, "status")).and_then(as_str).unwrap_or("").to_owned();
            (c, map([("agent", s(&agent)), ("run", s(&short(id))), ("status", s(&status)), ("cost", s(&dollars(c)))]))
        })
        .collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    map([("rows", list(rows.into_iter().map(|r| r.1).collect())), ("total", s(&dollars(session_cost(t))))])
}
