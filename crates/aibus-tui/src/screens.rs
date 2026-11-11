//! The modals and screens beyond the dashboard (05 sections 3 and 5): the permission modal, the
//! palette, the agents and accounts screens, the outcome question and the `/clear` confirmation.
//! Each is a function of one `view:` value and a theme, and says where its buttons and rows went
//! so the hit map can be built from it.

use aibus_core::state::value::{as_list, get};
use aibus_core::RunId;
use aibus_graph::Value;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Clear, Widget};

use aibus_provider::Secret;

use crate::hit::HitId;
use crate::theme::{Interact, Theme};
use crate::ui::Pointer;
use crate::widgets::{clip, flag, int, panel, put, text};

static NULL: Value = Value::Null;

/// What a modal puts on the hit map: its box, and each button or row inside it.
pub struct Placed {
    pub area: Rect,
    pub hits: Vec<(HitId, Rect)>,
}

/// A text field open over a modal: an edited tool input, a message to a run, an account being
/// added. `text` is what the field shows. A secret field (the key of an account) keeps what was
/// typed in `key` from the first keystroke, as a `Secret`, and shows one dot per character in
/// `text`; Enter hands the `Secret` on and nothing else ever reads it.
pub struct Entry {
    pub kind: EntryKind,
    pub label: String,
    pub text: String,
    pub key: Option<Secret<String>>,
}

impl Entry {
    /// A plain field.
    pub fn plain(kind: EntryKind, label: &str, text: &str) -> Self {
        Self { kind, label: label.to_owned(), text: text.to_owned(), key: None }
    }

    /// A field whose content is a secret: it starts empty.
    pub fn secret(kind: EntryKind, label: &str) -> Self {
        Self { kind, label: label.to_owned(), text: String::new(), key: Some(Secret::new(String::new())) }
    }

    /// The typed key with `edit` applied, which may add up to `room` bytes. The result is a new buffer sized so that it never grows
    /// (a regrown `String` would leave the old allocation behind, un-zeroed), and the old
    /// `Secret` is zeroised as it drops.
    pub fn edit_key(&mut self, room: usize, edit: impl FnOnce(&mut String)) {
        let Some(old) = self.key.take() else { return };
        let mut next = String::with_capacity(old.expose().len() + room);
        next.push_str(old.expose());
        edit(&mut next);
        self.text = next.chars().map(|_| '•').collect();
        self.key = Some(Secret::new(next));
    }
}

// What a frame depends on: the key is not part of it, and its dots are in `text`.
impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.label == other.label && self.text == other.text
    }
}

// A copy is for drawing: it has the dots and never the key.
impl Clone for Entry {
    fn clone(&self) -> Self {
        Self { kind: self.kind.clone(), label: self.label.clone(), text: self.text.clone(), key: None }
    }
}

// A key typed here must not reach a log through Debug.
impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = if self.key.is_some() { "<hidden>" } else { self.text.as_str() };
        write!(f, "Entry({:?}, {:?}, {shown})", self.kind, self.label)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// `e` in the permission modal: the input to run instead.
    EditInput,
    /// `m` in a transcript.
    Message(RunId),
    /// `R` on a partial run.
    Resume(RunId),
    /// The two steps of adding an account: its name (for the provider that was picked), its key.
    AccountName(String),
    AccountKey(String, String),
    /// `n` on the agents screen: the new agent's name.
    AgentName,
    /// `/` in the model picker: the text mirrors into the graph as it is typed.
    ModelFilter,
}

pub fn centred(full: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(full.width), h.min(full.height));
    Rect { x: full.x + (full.width - w) / 2, y: full.y + (full.height - h) / 2, width: w, height: h }
}

/// A button: `[key] label`, lit under the pointer, darker while pressed, dim when it does not apply.
pub fn button(buf: &mut Buffer, at: Rect, key: &str, label: &str, th: &Theme, state: Option<Interact>, enabled: bool) {
    let style = match (enabled, state) {
        (false, _) if th.mono => Style::new().add_modifier(Modifier::DIM),
        (false, _) => Style::new().fg(th.dim_text).bg(th.highlight),
        (true, Some(s)) => Style::new().fg(th.fg).patch(th.interact(s)),
        (true, None) if th.mono => Style::new().add_modifier(Modifier::BOLD),
        (true, None) => Style::new().fg(th.fg).bg(th.highlight),
    };
    buf.set_style(at, style);
    let line = if key.is_empty() { format!(" {label} ") } else { format!(" [{key}] {label} ") };
    // A mono button shows its edge with brackets of its own, since there is no fill to see.
    put(buf, at, 0, 0, &clip(&line, usize::from(at.width)), style);
    if th.mono {
        buf.set_style(at, style.add_modifier(Modifier::UNDERLINED));
    }
}

/// Lays `items` out from the left of `row`, wrapping to the next row; returns each button's rect
/// and draws it. `items` are `(id, key, label, enabled)`.
pub fn buttons(
    buf: &mut Buffer,
    area: Rect,
    y0: u16,
    items: &[(&'static str, &str, &str, bool)],
    th: &Theme,
    pointer: &Pointer,
) -> (Vec<(HitId, Rect)>, u16) {
    let mut out = Vec::new();
    let (mut x, mut y) = (0u16, 0u16);
    for (id, key, label, enabled) in items {
        let w = (label.chars().count() + key.chars().count() + if key.is_empty() { 2 } else { 5 }) as u16;
        if x > 0 && x + w > area.width {
            x = 0;
            y += 1;
        }
        let r = Rect { x: area.x + x, y: area.y + y0 + y, width: w.min(area.width), height: 1 };
        if r.bottom() > area.bottom() {
            break;
        }
        let hit = HitId::Button(id);
        let state = if pointer.pressed == Some(hit) && *enabled {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(hit) && *enabled {
            Some(Interact::Hover)
        } else {
            None
        };
        button(buf, r, key, label, th, state, *enabled);
        out.push((hit, r));
        x += w + 2;
    }
    (out, y + 1)
}

/// The text field open over a modal: its label, then the text (dots for a secret) and a cursor.
pub fn entry_line(buf: &mut Buffer, area: Rect, y: u16, e: &Entry, th: &Theme) {
    let shown = e.text.clone();
    let x = put(buf, area, 0, y, &format!("{}: ", e.label), Style::new().fg(th.dim_text));
    let room = usize::from(area.width.saturating_sub(x + 2));
    let skip = shown.chars().count().saturating_sub(room);
    let tail: String = shown.chars().skip(skip).collect();
    let x = put(buf, area, x, y, &tail, Style::new().fg(th.fg));
    put(buf, area, x, y, th.g("▌"), Style::new().fg(th.router));
}

/// The permission modal (05 section 5): 60 columns, yellow, the tool, what it would do, who asked
/// and four buttons. `1 of N` in the title when several wait; a run other than the lead is named.
pub fn permission(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer, entry: Option<&Entry>, cwd: &str) -> Placed {
    let agent = text(v, "agent");
    let run = text(v, "run_short");
    let of = int(v, "of");
    let mut title = format!("permission · {agent} (run {run})");
    if of > 1 {
        title.push_str(&format!(" · {} of {of}", int(v, "index")));
    }
    let w = 60.min(full.width);
    let inner_w = usize::from(w.saturating_sub(4)).max(1);
    let summary: Vec<String> = aibus_core::state::store::wrap_text(text(v, "summary"), inner_w.saturating_sub(2)).into_iter().take(4).collect();
    let h = (summary.len() as u16 + 8).min(full.height);
    let area = centred(full, w, h);
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, &title, th.warn, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    put(buf, inner, 0, 0, text(v, "tool"), Style::new().fg(th.fg).add_modifier(Modifier::BOLD));
    for (i, l) in summary.iter().enumerate() {
        put(buf, inner, 0, 1 + i as u16, &format!("{} {l}", if i == 0 { "$" } else { " " }), Style::new().fg(th.fg));
    }
    let y = 1 + summary.len() as u16;
    let who = if flag(v, "background") { format!("background run {agent} {run}  ·  cwd {cwd}") } else { format!("cwd {cwd}") };
    put(buf, inner, 0, y, &who, Style::new().fg(th.dim_text));
    let mut hits = Vec::new();
    if let Some(e) = entry {
        entry_line(buf, inner, y + 2, e, th);
        let (b, _) = buttons(buf, inner, y + 4, &[("send_edit", "Enter", "Allow edited", true), ("cancel", "Esc", "Back", true)], th, pointer);
        hits.extend(b);
    } else {
        let items = [
            ("allow", "y", "Allow once", true),
            ("always", "a", "Allow always", true),
            ("deny", "n", "Deny", true),
            ("edit", "e", "Edit", true),
            ("cancel", "Esc", "Cancel", true),
        ];
        let (b, _) = buttons(buf, inner, y + 2, &items, th, pointer);
        hits.extend(b);
    }
    Placed { area, hits }
}

/// `1 of 3` style title aside, the palette: a field, the ranked rows, the buttons.
pub fn palette(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer) -> Placed {
    let items = as_list(get(v, "items").unwrap_or(&NULL));
    let w = 70.min(full.width.saturating_sub(2)).max(30.min(full.width));
    let rows = items.len().clamp(1, usize::from(full.height.saturating_sub(8)).clamp(1, 14));
    let area = centred(full, w, rows as u16 + 6);
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, "palette", th.focus, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let x = put(buf, inner, 0, 0, ": ", Style::new().fg(th.dim_text));
    let x = put(buf, inner, x, 0, text(v, "text"), Style::new().fg(th.fg));
    put(buf, inner, x, 0, th.g("▌"), Style::new().fg(th.router));
    let sel = int(v, "sel") as usize;
    let first = sel.saturating_sub(rows - 1);
    let mut hits = Vec::new();
    if items.is_empty() {
        put(buf, inner, 0, 2, "nothing matches", Style::new().fg(th.dim_text));
    }
    for (k, item) in items.iter().skip(first).take(rows).enumerate() {
        let r = Rect { y: inner.y + 2 + k as u16, height: 1, ..inner };
        let i = first + k;
        let hit = HitId::ListRow(i);
        let state = if pointer.pressed == Some(hit) {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(hit) {
            Some(Interact::Hover)
        } else if i == sel {
            Some(Interact::Selected)
        } else {
            None
        };
        if let Some(s) = state {
            buf.set_style(r, th.interact(s));
        }
        let kind = text(item, "kind");
        put(buf, r, 0, 0, &format!("{kind:<8}"), Style::new().fg(th.dim_text));
        put(buf, r, 9, 0, &clip(text(item, "label"), usize::from(r.width).saturating_sub(24)), Style::new().fg(th.fg));
        let hint = text(item, "hint");
        let hx = r.width.saturating_sub(hint.chars().count() as u16 + 1);
        put(buf, r, hx, 0, hint, Style::new().fg(th.dim_text));
        hits.push((hit, r));
    }
    let (b, _) = buttons(buf, inner, 2 + rows as u16 + 1, &[("run", "Enter", "Run", !items.is_empty()), ("cancel", "Esc", "Cancel", true)], th, pointer);
    hits.extend(b);
    Placed { area, hits }
}

fn column(buf: &mut Buffer, r: Rect, x: u16, w: u16, s: &str, style: Style) {
    put(buf, r, x, 0, &clip(s, usize::from(w)), style);
}

/// The agents screen: every definition with scope, model and tools, and a preview of the body.
pub fn agents(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer, entry: Option<&Entry>, note: &str) -> Placed {
    let area = full;
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, "agents", th.swarm, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let sel = int(v, "sel") as usize;
    let dim = Style::new().fg(th.dim_text);
    let list_h = (inner.height.saturating_sub(5) * 55 / 100).max(3);
    let name_w = 18u16;
    let head = Rect { height: 1, ..inner };
    column(buf, head, 0, name_w, "name", dim);
    column(buf, head, name_w + 1, 9, "scope", dim);
    column(buf, head, name_w + 11, 10, "role", dim);
    column(buf, head, name_w + 22, 18, "model", dim);
    column(buf, head, name_w + 41, inner.width.saturating_sub(name_w + 41), "tools", dim);
    let shown = usize::from(list_h - 1);
    let first = sel.saturating_sub(shown.saturating_sub(1));
    let mut hits = Vec::new();
    if rows.is_empty() {
        put(buf, inner, 0, 1, "no agent definitions found", dim);
    }
    for (k, row) in rows.iter().skip(first).take(shown).enumerate() {
        let r = Rect { y: inner.y + 1 + k as u16, height: 1, ..inner };
        let i = first + k;
        let hit = HitId::ListRow(i);
        let state = if pointer.pressed == Some(hit) {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(hit) {
            Some(Interact::Hover)
        } else if i == sel {
            Some(Interact::Selected)
        } else {
            None
        };
        if let Some(s) = state {
            buf.set_style(r, th.interact(s));
        }
        let fg = Style::new().fg(th.fg);
        column(buf, r, 0, name_w, text(row, "name"), fg);
        column(buf, r, name_w + 1, 9, text(row, "scope"), dim);
        column(buf, r, name_w + 11, 10, text(row, "role"), dim);
        column(buf, r, name_w + 22, 18, text(row, "model"), fg);
        column(buf, r, name_w + 41, inner.width.saturating_sub(name_w + 41), text(row, "tools"), dim);
        hits.push((hit, r));
    }
    // The preview of the selected definition's body.
    let preview_top = inner.y + list_h;
    let rule = Rect { y: preview_top, height: 1, ..inner };
    let name = rows.get(sel).map_or("", |r| text(r, "name"));
    put(buf, rule, 0, 0, &format!("── {name} {}", "─".repeat(usize::from(inner.width).saturating_sub(name.chars().count() + 4))), dim);
    let preview = as_list(get(v, "preview").unwrap_or(&NULL));
    let room = usize::from(inner.bottom().saturating_sub(preview_top + 1 + 3));
    for (k, l) in preview.iter().take(room).enumerate() {
        let r = Rect { y: preview_top + 1 + k as u16, height: 1, ..inner };
        put(buf, r, 0, 0, &clip(l.as_str_or(""), usize::from(r.width)), Style::new().fg(th.fg));
    }
    let bottom = Rect { y: inner.bottom().saturating_sub(3), height: 3, ..inner };
    let warn = int(v, "warnings");
    let status = if !note.is_empty() {
        note.to_owned()
    } else if warn > 0 {
        format!("{warn} warning(s): {}", get(v, "first_warning").and_then(|w| w.as_text()).unwrap_or(""))
    } else {
        String::new()
    };
    put(buf, bottom, 0, 0, &clip(&status, usize::from(bottom.width)), Style::new().fg(if note.starts_with("invalid") { th.error } else { th.dim_text }));
    if let Some(e) = entry {
        entry_line(buf, bottom, 1, e, th);
    } else {
        let (b, _) = buttons(
            buf,
            bottom,
            1,
            &[("new", "n", "New", true), ("edit", "e", "Edit", !rows.is_empty()), ("validate", "v", "Validate", !rows.is_empty()), ("delegate", "d", "Delegate now", !rows.is_empty()), ("cancel", "Esc", "Close", true)],
            th,
            pointer,
        );
        hits.extend(b);
    }
    Placed { area, hits }
}

/// The accounts screen: name, provider, model and whether a key is set.
pub fn accounts(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer, entry: Option<&Entry>, note: &str) -> Placed {
    let area = full;
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, "accounts", th.router, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let sel = int(v, "sel") as usize;
    let dim = Style::new().fg(th.dim_text);
    let head = Rect { height: 1, ..inner };
    column(buf, head, 0, 18, "name", dim);
    column(buf, head, 19, 12, "provider", dim);
    column(buf, head, 32, 28, "default model", dim);
    column(buf, head, 61, 10, "key", dim);
    column(buf, head, 72, 10, "status", dim);
    let shown = usize::from(inner.height.saturating_sub(5)).max(1);
    let first = sel.saturating_sub(shown.saturating_sub(1));
    let mut hits = Vec::new();
    if rows.is_empty() {
        put(buf, inner, 0, 1, "no accounts yet: add one with a", dim);
    }
    for (k, row) in rows.iter().skip(first).take(shown).enumerate() {
        let r = Rect { y: inner.y + 1 + k as u16, height: 1, ..inner };
        let i = first + k;
        let hit = HitId::ListRow(i);
        let state = if pointer.pressed == Some(hit) {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(hit) {
            Some(Interact::Hover)
        } else if i == sel {
            Some(Interact::Selected)
        } else {
            None
        };
        if let Some(s) = state {
            buf.set_style(r, th.interact(s));
        }
        let fg = Style::new().fg(th.fg);
        column(buf, r, 0, 18, text(row, "name"), fg);
        column(buf, r, 19, 12, text(row, "provider"), dim);
        column(buf, r, 32, 28, row_model(row), fg);
        column(buf, r, 61, 10, text(row, "key_source"), dim);
        let ready = text(row, "status") == "ready";
        column(buf, r, 72, 10, text(row, "status"), Style::new().fg(if ready { th.ok } else { th.warn }));
        hits.push((hit, r));
    }
    let bottom = Rect { y: inner.bottom().saturating_sub(3), height: 3, ..inner };
    put(buf, bottom, 0, 0, &clip(note, usize::from(bottom.width)), dim);
    if let Some(e) = entry {
        entry_line(buf, bottom, 1, e, th);
    } else {
        let (b, _) = buttons(
            buf,
            bottom,
            1,
            &[("test", "t", "Test", !rows.is_empty()), ("add", "a", "Add", true), ("disable", "d", "Disable", !rows.is_empty()), ("cancel", "Esc", "Close", true)],
            th,
            pointer,
        );
        hits.extend(b);
    }
    Placed { area, hits }
}

/// The `/model` picker: one row per `(account, model)` with the alias that names it, the context
/// window, what a million tokens cost in and out (`n/a` when the table has no price) and four
/// capability letters (T tools, R reasoning, I images, C cache). The row the target runs on now
/// is marked; the columns share what the width leaves after the fixed ones.
pub fn models(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer, entry: Option<&Entry>, note: &str) -> Placed {
    let area = full;
    Clear.render(area, buf);
    let target = get(v, "target").and_then(|t| t.as_text()).unwrap_or("lead").to_owned();
    let for_run = target.starts_with("run ");
    let title = if for_run { "models for this run".to_owned() } else { format!("models for the {target}") };
    let inner = {
        let i = panel(buf, area, &title, th.router, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let sel = int(v, "sel") as usize;
    let dim = Style::new().fg(th.dim_text);
    let fg = Style::new().fg(th.fg);
    let text = get(v, "text").and_then(|t| t.as_text()).unwrap_or("");
    match entry {
        Some(e) => entry_line(buf, inner, 0, e, th),
        None if text.is_empty() => {
            put(buf, inner, 0, 0, "/ to filter", dim);
        }
        None => {
            put(buf, inner, 0, 0, &format!("filter: {text}"), fg);
        }
    }
    // alias, model, account, context, in, out, caps: the model and account columns take the slack.
    let slack = inner.width.saturating_sub(67);
    let (w_model, w_acct) = (20 + slack * 2 / 3, 10 + slack / 3);
    let x_model = 11;
    let x_acct = x_model + w_model + 1;
    let x_ctx = x_acct + w_acct + 1;
    let (x_in, x_out, x_caps) = (x_ctx + 6, x_ctx + 13, x_ctx + 20);
    let head = Rect { y: inner.y + 1, height: 1, ..inner };
    column(buf, head, 2, 8, "alias", dim);
    column(buf, head, x_model, w_model, "model", dim);
    column(buf, head, x_acct, w_acct, "account", dim);
    column(buf, head, x_ctx, 5, "ctx", dim);
    column(buf, head, x_in, 6, "in $/M", dim);
    column(buf, head, x_out, 6, "out $/M", dim);
    column(buf, head, x_caps, 4, "TRIC", dim);
    let shown = usize::from(inner.height.saturating_sub(6)).max(1);
    let first = sel.saturating_sub(shown.saturating_sub(1));
    let mut hits = Vec::new();
    if rows.is_empty() {
        let hidden = int(v, "hidden");
        let why = if !text.is_empty() {
            "no model matches the filter".to_owned()
        } else if hidden > 0 {
            format!("{hidden} models have no tool calling, which this target needs")
        } else {
            "no account serves a model yet: /login adds one".to_owned()
        };
        put(buf, inner, 0, 2, &why, dim);
    }
    for (k, row) in rows.iter().skip(first).take(shown).enumerate() {
        let r = Rect { y: inner.y + 2 + k as u16, height: 1, ..inner };
        let i = first + k;
        let hit = HitId::ListRow(i);
        let state = if pointer.pressed == Some(hit) {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(hit) {
            Some(Interact::Hover)
        } else if i == sel {
            Some(Interact::Selected)
        } else {
            None
        };
        if let Some(s) = state {
            buf.set_style(r, th.interact(s));
        }
        let word = |k: &str| get(row, k).and_then(|x| x.as_text()).unwrap_or("").to_owned();
        let faded = matches!(get(row, "dim"), Some(Value::Bool(true)));
        let main = if faded { dim } else { fg };
        let current = matches!(get(row, "current"), Some(Value::Bool(true)));
        column(buf, r, 0, 2, if current { th.g("●") } else { "" }, Style::new().fg(th.ok));
        column(buf, r, 2, 8, &word("alias"), dim);
        column(buf, r, x_model, w_model, &word("id"), main);
        column(buf, r, x_acct, w_acct, &word("account"), dim);
        column(buf, r, x_ctx, 5, &word("context"), dim);
        column(buf, r, x_in, 6, &word("input"), main);
        column(buf, r, x_out, 6, &word("output"), main);
        column(buf, r, x_caps, 4, &word("caps"), dim);
        hits.push((hit, r));
    }
    let bottom = Rect { y: inner.bottom().saturating_sub(3), height: 3, ..inner };
    put(buf, bottom, 0, 0, &clip(note, usize::from(bottom.width)), dim);
    let any = !rows.is_empty();
    let (b, _) = buttons(
        buf,
        bottom,
        1,
        &[
            ("model_use", "Enter", "Use", any),
            ("seat_advisor", "a", "Advisor", any),
            ("seat_router", "r", "Router", any),
            ("this_run", "s", "This run only", any && for_run),
            ("filter", "/", "Filter", true),
            ("cancel", "Esc", "Close", true),
        ],
        th,
        pointer,
    );
    hits.extend(b);
    Placed { area, hits }
}


/// The `/login` provider picker: the presets, one row each.
pub fn providers(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer) -> Placed {
    let area = centred(full, 78, full.height.min(22));
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, "log in to a provider", th.router, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let sel = int(v, "sel") as usize;
    let dim = Style::new().fg(th.dim_text);
    let fg = Style::new().fg(th.fg);
    let head = Rect { height: 1, ..inner };
    column(buf, head, 0, 18, "provider", dim);
    column(buf, head, 19, 24, "wire", dim);
    column(buf, head, 44, 5, "auth", dim);
    column(buf, head, 50, 24, "address", dim);
    let shown = usize::from(inner.height.saturating_sub(4)).max(1);
    let first = sel.saturating_sub(shown.saturating_sub(1));
    let mut hits = Vec::new();
    for (k, row) in rows.iter().skip(first).take(shown).enumerate() {
        let r = Rect { y: inner.y + 1 + k as u16, height: 1, ..inner };
        let i = first + k;
        let hit = HitId::ListRow(i);
        let state = if pointer.pressed == Some(hit) {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(hit) {
            Some(Interact::Hover)
        } else if i == sel {
            Some(Interact::Selected)
        } else {
            None
        };
        if let Some(s) = state {
            buf.set_style(r, th.interact(s));
        }
        let word = |k: &str| get(row, k).and_then(|x| x.as_text()).unwrap_or("").to_owned();
        column(buf, r, 0, 18, &word("id"), fg);
        column(buf, r, 19, 24, &word("api"), dim);
        column(buf, r, 44, 5, &word("auth"), dim);
        column(buf, r, 50, 24, &word("url"), dim);
        hits.push((hit, r));
    }
    let bottom = Rect { y: inner.bottom().saturating_sub(2), height: 2, ..inner };
    let (b, _) = buttons(buf, bottom, 0, &[("provider_pick", "Enter", "Choose", !rows.is_empty()), ("cancel", "Esc", "Close", true)], th, pointer);
    hits.extend(b);
    Placed { area, hits }
}

fn row_model(row: &Value) -> &str {
    get(row, "model").and_then(|m| m.as_text()).unwrap_or("")
}

/// How did this session go? (01 section 8.2): four outcomes, and a way out that records nothing.
pub fn outcome(buf: &mut Buffer, full: Rect, th: &Theme, pointer: &Pointer) -> Placed {
    let area = centred(full, 54, 12);
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, "how did this session go?", th.advisor, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    put(buf, inner, 0, 0, "The answer goes into the handoff.", Style::new().fg(th.dim_text));
    let items = [
        ("outcome:1", "1", "Succeeded", true),
        ("outcome:2", "2", "Partial, mostly there", true),
        ("outcome:3", "3", "Partial, not much", true),
        ("outcome:4", "4", "Failed", true),
    ];
    let mut hits = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let (b, _) = buttons(buf, Rect { y: inner.y + 2 + i as u16, height: 1, ..inner }, 0, std::slice::from_ref(item), th, pointer);
        hits.extend(b);
    }
    let (b, _) = buttons(buf, inner, 7, &[("quit_now", "!", "Quit without recording", true), ("cancel", "Esc", "Cancel", true)], th, pointer);
    hits.extend(b);
    Placed { area, hits }
}

/// `/clear` and `/cancel @lead` ask first: a title, the question and the id of the yes button.
pub fn confirm(buf: &mut Buffer, full: Rect, th: &Theme, pointer: &Pointer, title: &str, question: &str, yes: (&'static str, &str)) -> Placed {
    let area = centred(full, 44, 6);
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, title, th.warn, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    put(buf, inner, 0, 0, question, Style::new().fg(th.fg));
    let (hits, _) = buttons(buf, inner, 2, &[(yes.0, "y", yes.1, true), ("cancel", "n", "Cancel", true)], th, pointer);
    Placed { area, hits }
}

/// The `/status` modal: label and value rows from `view:run_status`.
pub fn run_status(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer) -> Placed {
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let area = centred(full, 46, rows.len() as u16 + 5);
    Clear.render(area, buf);
    let title = get(v, "title").and_then(|t| t.as_text()).unwrap_or("status");
    let inner = {
        let i = panel(buf, area, title, th.router, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    for (i, row) in rows.iter().enumerate() {
        let word = |k: &str| get(row, k).and_then(|x| x.as_text()).unwrap_or("").to_owned();
        let x = put(buf, inner, 0, i as u16, &format!("{:<14}", word("k")), Style::new().fg(th.dim_text));
        put(buf, inner, x, i as u16, &clip(&word("v"), usize::from(inner.width.saturating_sub(x))), Style::new().fg(th.fg));
    }
    let (hits, _) = buttons(buf, inner, rows.len() as u16 + 1, &[("close", "Esc", "Close", true)], th, pointer);
    Placed { area, hits }
}

/// The `/cost` modal: one row per run, dearest first, and the session total under them.
pub fn cost(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer) -> Placed {
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let shown = rows.len().min(usize::from(full.height.saturating_sub(8)).max(1));
    let area = centred(full, 56, shown as u16 + 7);
    Clear.render(area, buf);
    let inner = {
        let i = panel(buf, area, "cost", th.router, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let dim = Style::new().fg(th.dim_text);
    let fg = Style::new().fg(th.fg);
    let head = Rect { height: 1, ..inner };
    column(buf, head, 0, 16, "agent", dim);
    column(buf, head, 17, 10, "run", dim);
    column(buf, head, 28, 14, "status", dim);
    column(buf, head, 43, 10, "cost", dim);
    for (i, row) in rows.iter().take(shown).enumerate() {
        let r = Rect { y: inner.y + 1 + i as u16, height: 1, ..inner };
        let word = |k: &str| get(row, k).and_then(|x| x.as_text()).unwrap_or("").to_owned();
        column(buf, r, 0, 16, &word("agent"), fg);
        column(buf, r, 17, 10, &word("run"), fg);
        column(buf, r, 28, 14, &word("status"), dim);
        column(buf, r, 43, 10, &word("cost"), fg);
    }
    let total = get(v, "total").and_then(|x| x.as_text()).unwrap_or("").to_owned();
    put(buf, inner, 0, shown as u16 + 1, &format!("{:<43}{total}", "session total"), fg.add_modifier(Modifier::BOLD));
    let (hits, _) = buttons(buf, inner, shown as u16 + 3, &[("close", "Esc", "Close", true)], th, pointer);
    Placed { area, hits }
}

/// Small conveniences on `Value` that the screens use.
trait ValueExt {
    fn as_text(&self) -> Option<&str>;
    fn as_str_or<'a>(&'a self, d: &'a str) -> &'a str;
}

impl ValueExt for Value {
    fn as_text(&self) -> Option<&str> {
        aibus_core::state::value::as_str(self)
    }

    fn as_str_or<'a>(&'a self, d: &'a str) -> &'a str {
        self.as_text().unwrap_or(d)
    }
}
