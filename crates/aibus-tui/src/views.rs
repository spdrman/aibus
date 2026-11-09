//! The `view:` nodes of the screens beyond the dashboard (05 section 3): the permission modal, the
//! palette, the agents and accounts screens and the transcript. Same rule as `view.rs`: a widget
//! draws exactly one of them and sees nothing but its `Value`.

use aibus_core::state::store::{Row, Store};
use aibus_core::state::value::{as_i64, as_list, as_str, get, list, map, n, s};
use aibus_core::state::{
    run_key, transcript_body, AGENTS_DEFS, AGENTS_WARNINGS, ACCOUNTS, MODAL, PALETTE_TEXT, PERMISSIONS_OPEN, RUNS_ORDER,
    SCREEN_SEL, SIZE, TRANSCRIPT_UI,
};
use aibus_core::RunStatus;
use aibus_graph::{Tracker, Value};

pub const PERMISSION: &str = "view:permission";
pub const PALETTE: &str = "view:palette";
pub const AGENTS: &str = "view:agents";
pub const ACCOUNTS_VIEW: &str = "view:accounts";
pub const TRANSCRIPT: &str = "view:transcript";

/// The commands the palette lists besides agents, workflows and runs: `(id, label, hint)`.
pub const PALETTE_COMMANDS: [(&str, &str, &str); 10] = [
    ("agents", "agents screen", "A"),
    ("accounts", "accounts screen", "K"),
    ("help", "help: the key map", "?"),
    ("lead", "lead transcript", "t"),
    ("permission", "oldest permission prompt", "p"),
    ("handoff", "write a handoff", "/handoff"),
    ("clear", "clear the session log", "/clear"),
    ("rail", "advisor rail", "a"),
    ("compact", "compact the lead", "/compact"),
    ("quit", "quit", "q"),
];

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
    for (id, label, hint) in PALETTE_COMMANDS {
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
