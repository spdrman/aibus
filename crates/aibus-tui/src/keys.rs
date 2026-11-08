//! The key map (05 section 4): every default binding, the `[keys]` and `keys.toml` overrides by
//! action name, conflict warnings, and the rows the `?` screen is built from. The help screen
//! reads the live `Keymap`, so a rebound key shows up there.

use std::collections::BTreeMap;

use aibus_core::{KeyCode, KeyEvent};

macro_rules! actions {
    ($($variant:ident $name:literal $help:literal;)*) => {
        /// One thing a key can ask for. The snake_case name is what `[keys]` uses.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Action { $($variant),* }

        /// Every action in help-screen order: `(action, name, one-line help)`.
        pub const ACTIONS: &[(Action, &str, &str)] = &[$((Action::$variant, $name, $help)),*];
    };
}

actions! {
    FocusNext "focus_next" "next pane";
    FocusPrev "focus_prev" "previous pane";
    Submit "submit" "send the prompt";
    OpenTranscript "open_transcript" "open run transcript";
    MoveUp "move_up" "up / scroll";
    MoveDown "move_down" "down / scroll";
    CardPrev "card_prev" "previous card";
    CardNext "card_next" "next card";
    StopRun "stop_run" "stop or dismiss run";
    RetryRun "retry_run" "retry failed run";
    MessageRun "message_run" "message the run";
    ResumeRun "resume_run" "resume partial run";
    ToggleBackground "toggle_background" "background / foreground";
    BackgroundWait "background_wait" "wait in background";
    OpenLeadTranscript "open_lead_transcript" "lead transcript";
    FocusRail "focus_rail" "advisor rail";
    ShowAdvice "show_advice" "show last advice";
    JumpPermission "jump_permission" "oldest permission";
    Allow "allow" "allow this call";
    Deny "deny" "deny this call";
    AllowAlways "allow_always" "always allow this tool";
    EditInput "edit_input" "edit input, then allow";
    OpenPalette "open_palette" "command palette";
    Filter "filter" "filter / search";
    ToggleFollow "toggle_follow" "follow newest";
    TimeBack "time_back" "step time back";
    TimeForward "time_forward" "step time forward";
    OpenAgents "open_agents" "agents screen";
    OpenAccounts "open_accounts" "accounts screen";
    OpenHelp "open_help" "this help";
    CancelRun "cancel_run" "cancel run, twice quits";
    Quit "quit" "quit";
    CloseModal "close_modal" "close";
    ClearPrompt "clear_prompt" "clear the prompt";
}

impl Action {
    pub fn name(self) -> &'static str {
        ACTIONS.iter().find(|(a, ..)| *a == self).map_or("", |(_, n, _)| n)
    }

    pub fn parse(name: &str) -> Option<Self> {
        ACTIONS.iter().find(|(_, n, _)| *n == name).map(|(a, ..)| *a)
    }

    pub fn help(self) -> &'static str {
        ACTIONS.iter().find(|(a, ..)| *a == self).map_or("", |(_, _, h)| h)
    }
}

/// Where the user is, as far as the key map cares. A modal takes the keys away from the
/// dashboard behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctx {
    Prompt,
    Cards,
    Log,
    Rail,
    Transcript,
    Permission,
    /// Any other modal (palette, advice, a screen).
    Modal,
}

/// Where a binding applies (the "Context" column of the table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Every pane of the dashboard. Plain printable keys are typing while the prompt has focus.
    Global,
    /// Cards, log and rail, not the prompt.
    Dashboard,
    Prompt,
    Cards,
    Log,
    Rail,
    Transcript,
    Permission,
    Modal,
}

impl Scope {
    fn covers(self, ctx: Ctx) -> bool {
        use Ctx::*;
        match self {
            Scope::Global => matches!(ctx, Prompt | Cards | Log | Rail),
            Scope::Dashboard => matches!(ctx, Cards | Log | Rail),
            Scope::Prompt => ctx == Prompt,
            Scope::Cards => ctx == Cards,
            Scope::Log => ctx == Log,
            Scope::Rail => ctx == Rail,
            Scope::Transcript => ctx == Transcript,
            Scope::Permission => ctx == Permission,
            Scope::Modal => ctx == Modal,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Dashboard => "dashboard",
            Scope::Prompt => "prompt",
            Scope::Cards => "cards",
            Scope::Log => "log",
            Scope::Rail => "rail",
            Scope::Transcript => "transcript",
            Scope::Permission => "permission",
            Scope::Modal => "modal",
        }
    }
}

const ALL_CTX: [Ctx; 7] = [
    Ctx::Prompt,
    Ctx::Cards,
    Ctx::Log,
    Ctx::Rail,
    Ctx::Transcript,
    Ctx::Permission,
    Ctx::Modal,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub key: KeyEvent,
    pub scopes: Vec<Scope>,
}

impl Binding {
    fn covers(&self, ctx: Ctx) -> bool {
        self.scopes.iter().any(|s| s.covers(ctx))
    }
}

use Action::*;
use Scope::*;

/// The table of 05 section 4, one row per key: `(action, key, scopes)`.
const DEFAULTS: &[(Action, &str, &[Scope])] = &[
    (FocusNext, "tab", &[Global]),
    (FocusPrev, "shift+tab", &[Global]),
    (Submit, "enter", &[Prompt]),
    (OpenTranscript, "enter", &[Cards, Log]),
    (MoveUp, "up", &[Cards, Log, Rail]),
    (MoveUp, "k", &[Cards, Log, Rail]),
    (MoveDown, "down", &[Cards, Log, Rail]),
    (MoveDown, "j", &[Cards, Log, Rail]),
    (CardPrev, "left", &[Cards]),
    (CardPrev, "h", &[Cards]),
    (CardNext, "right", &[Cards]),
    (CardNext, "l", &[Cards]),
    (StopRun, "x", &[Cards, Transcript]),
    (RetryRun, "r", &[Cards, Transcript]),
    (MessageRun, "m", &[Cards, Transcript]),
    (ResumeRun, "R", &[Cards]),
    (ToggleBackground, "b", &[Cards]),
    (BackgroundWait, "ctrl+b", &[Global]),
    (OpenLeadTranscript, "t", &[Global]),
    (FocusRail, "a", &[Global]),
    (ShowAdvice, "enter", &[Rail]),
    (JumpPermission, "p", &[Global]),
    (Allow, "y", &[Permission]),
    (Deny, "n", &[Permission]),
    (AllowAlways, "a", &[Permission]),
    (EditInput, "e", &[Permission]),
    (OpenPalette, ":", &[Global]),
    (Filter, "/", &[Log, Transcript]),
    (ToggleFollow, "f", &[Log, Transcript]),
    (TimeBack, "[", &[Transcript, Dashboard]),
    (TimeForward, "]", &[Transcript, Dashboard]),
    (OpenAgents, "A", &[Global]),
    (OpenAccounts, "K", &[Global]),
    (OpenHelp, "?", &[Global]),
    (CancelRun, "ctrl+c", &[Global]),
    (Quit, "q", &[Dashboard]),
    (CloseModal, "esc", &[Modal, Permission, Transcript]),
    (CloseModal, "q", &[Transcript]),
    (ClearPrompt, "esc", &[Prompt]),
];

/// `ctrl+b`, `shift+tab`, `esc`, `R`, `f5`: modifiers joined by `+`, then a key name or one
/// character. A character with `shift` is the upper-case character.
pub fn parse_key(text: &str) -> Result<KeyEvent, String> {
    let bad = || format!("cannot read key `{text}`");
    let (mods, name) = match text.strip_suffix("++") {
        Some(m) => (m, "+"),
        None if text == "+" => ("", "+"),
        None => text.rsplit_once('+').unwrap_or(("", text)),
    };
    let mut ev = KeyEvent { code: KeyCode::Esc, ctrl: false, alt: false, shift: false };
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ev.ctrl = true,
            "alt" => ev.alt = true,
            "shift" => ev.shift = true,
            _ => return Err(bad()),
        }
    }
    let lower = name.to_ascii_lowercase();
    ev.code = match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        f if f.len() > 1 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            KeyCode::F(f[1..].parse().map_err(|_| bad())?)
        }
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return Err(bad()),
            }
        }
    };
    Ok(normalize(ev))
}

/// A character key ignores `shift` (the character already says it); everything else keeps it.
pub fn normalize(mut ev: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(c) = ev.code {
        if ev.ctrl {
            // Ctrl+B and Ctrl+b are one key to a terminal.
            ev.code = KeyCode::Char(c.to_ascii_lowercase());
        } else if ev.shift && c.is_lowercase() {
            ev.code = KeyCode::Char(c.to_uppercase().next().unwrap_or(c));
        }
        ev.shift = false;
    }
    ev
}

/// How a key is written in the help screen.
pub fn key_name(ev: &KeyEvent) -> String {
    let mut out = String::new();
    for (on, m) in [(ev.ctrl, "Ctrl+"), (ev.alt, "Alt+"), (ev.shift, "Shift+")] {
        if on {
            out.push_str(m);
        }
    }
    match ev.code {
        KeyCode::Char(' ') => out.push_str("Space"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Esc => out.push_str("Esc"),
        KeyCode::Tab => out.push_str("Tab"),
        KeyCode::Backspace => out.push_str("Backspace"),
        KeyCode::Up => out.push_str("Up"),
        KeyCode::Down => out.push_str("Down"),
        KeyCode::Left => out.push_str("Left"),
        KeyCode::Right => out.push_str("Right"),
        KeyCode::PageUp => out.push_str("PgUp"),
        KeyCode::PageDown => out.push_str("PgDn"),
        KeyCode::F(n) => out.push_str(&format!("F{n}")),
    }
    out
}

/// One line of the help screen: an action with all its keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpRow {
    pub keys: String,
    pub text: &'static str,
    pub scope: String,
}

#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        let bindings = DEFAULTS
            .iter()
            .map(|(action, key, scopes)| Binding {
                action: *action,
                key: parse_key(key).unwrap_or(KeyEvent {
                    code: KeyCode::Esc,
                    ctrl: false,
                    alt: false,
                    shift: false,
                }),
                scopes: scopes.to_vec(),
            })
            .collect();
        Self { bindings }
    }
}

/// The overrides of a `keys.toml`: `action = "key"` or `action = ["key", "key"]`, at the top
/// level or under a `[keys]` table.
pub fn parse_keys_toml(text: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let inner = match table.get("keys") {
        Some(toml::Value::Table(t)) => Some(t.clone()),
        _ => None,
    };
    let table = inner.unwrap_or(table);
    let mut out = Vec::new();
    for (action, v) in table {
        let keys = match v {
            toml::Value::String(s) => vec![s],
            toml::Value::Array(a) => a
                .into_iter()
                .map(|k| match k {
                    toml::Value::String(s) => Ok(s),
                    other => Err(format!("`{action}`: a key must be a string, not {other}")),
                })
                .collect::<Result<_, _>>()?,
            other => return Err(format!("`{action}`: a key must be a string, not {other}")),
        };
        out.push((action, keys));
    }
    Ok(out)
}

impl Keymap {
    /// The defaults with the overrides applied by action name, in order (a later one replaces an
    /// earlier one for the same action). An override replaces every key of its action. Unknown
    /// actions, unreadable keys and keys that now clash with another action come back as warnings;
    /// a clash is kept and the overriding action wins.
    pub fn with_overrides(overrides: &[(String, Vec<String>)]) -> (Self, Vec<String>) {
        let mut map = Self::default();
        let mut warnings = Vec::new();
        for (name, keys) in overrides {
            let Some(action) = Action::parse(name) else {
                warnings.push(format!("keys: unknown action `{name}`"));
                continue;
            };
            let mut parsed = Vec::new();
            for k in keys {
                match parse_key(k) {
                    Ok(ev) => parsed.push(ev),
                    Err(e) => warnings.push(format!("keys: `{name}`: {e}")),
                }
            }
            if parsed.is_empty() {
                continue;
            }
            let mut scopes: Vec<Scope> = Vec::new();
            for b in map.bindings.iter().filter(|b| b.action == action) {
                for s in &b.scopes {
                    if !scopes.contains(s) {
                        scopes.push(*s);
                    }
                }
            }
            map.bindings.retain(|b| b.action != action);
            // In front, so that on a clash the user's choice is the one found first.
            for (i, key) in parsed.into_iter().enumerate() {
                map.bindings.insert(i, Binding { action, key, scopes: scopes.clone() });
            }
        }
        warnings.extend(map.conflicts());
        (map, warnings)
    }

    /// The `[keys]` table of the config, then `keys.toml` on top of it.
    pub fn load(config: &BTreeMap<String, String>, keys_toml: Option<&str>) -> (Self, Vec<String>) {
        let mut overrides: Vec<(String, Vec<String>)> =
            config.iter().map(|(a, k)| (a.clone(), vec![k.clone()])).collect();
        let mut warnings = Vec::new();
        if let Some(text) = keys_toml {
            match parse_keys_toml(text) {
                Ok(o) => overrides.extend(o),
                Err(e) => warnings.push(format!("keys.toml: {e}")),
            }
        }
        let (map, more) = Self::with_overrides(&overrides);
        warnings.extend(more);
        (map, warnings)
    }

    /// Two actions on one key where both can be active at once.
    pub fn conflicts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, a) in self.bindings.iter().enumerate() {
            for b in &self.bindings[i + 1..] {
                if a.key == b.key && a.action != b.action {
                    let shared: Vec<&str> = ALL_CTX
                        .iter()
                        .filter(|c| a.covers(**c) && b.covers(**c))
                        .map(|c| ctx_name(*c))
                        .collect();
                    if !shared.is_empty() {
                        out.push(format!(
                            "keys: `{}` is bound to both {} and {} ({})",
                            key_name(&a.key),
                            a.action.name(),
                            b.action.name(),
                            shared.join(", ")
                        ));
                    }
                }
            }
        }
        out
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The action a key press means where the user is. While the prompt has focus a plain
    /// printable key is text, so only bindings aimed at the prompt (or non-printable keys) match.
    pub fn lookup(&self, ev: KeyEvent, ctx: Ctx) -> Option<Action> {
        let ev = normalize(ev);
        let typing = ctx == Ctx::Prompt
            && matches!(ev.code, KeyCode::Char(_))
            && !ev.ctrl
            && !ev.alt;
        self.bindings
            .iter()
            .find(|b| {
                b.key == ev
                    && b.covers(ctx)
                    && !(typing && !b.scopes.contains(&Scope::Prompt))
            })
            .map(|b| b.action)
    }

    /// One row per action that has a key, in help order, for the `?` screen.
    pub fn help_rows(&self) -> Vec<HelpRow> {
        ACTIONS
            .iter()
            .filter_map(|(action, _, text)| {
                let bound: Vec<&Binding> =
                    self.bindings.iter().filter(|b| b.action == *action).collect();
                if bound.is_empty() {
                    return None;
                }
                let mut keys: Vec<String> = Vec::new();
                let mut scopes: Vec<&str> = Vec::new();
                for b in &bound {
                    let k = key_name(&b.key);
                    if !keys.contains(&k) {
                        keys.push(k);
                    }
                    for s in &b.scopes {
                        if !scopes.contains(&s.name()) {
                            scopes.push(s.name());
                        }
                    }
                }
                Some(HelpRow { keys: keys.join(" "), text, scope: scopes.join(", ") })
            })
            .collect()
    }
}

fn ctx_name(c: Ctx) -> &'static str {
    match c {
        Ctx::Prompt => "prompt",
        Ctx::Cards => "cards",
        Ctx::Log => "log",
        Ctx::Rail => "rail",
        Ctx::Transcript => "transcript",
        Ctx::Permission => "permission",
        Ctx::Modal => "modal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(s: &str) -> KeyEvent {
        parse_key(s).unwrap()
    }

    #[test]
    fn every_action_has_a_default_key_and_the_defaults_do_not_clash() {
        let map = Keymap::default();
        for (action, name, _) in ACTIONS {
            assert!(
                map.bindings().iter().any(|b| b.action == *action),
                "{name} has no default key"
            );
        }
        assert_eq!(map.conflicts(), Vec::<String>::new());
        assert_eq!(ACTIONS.len(), 34);
    }

    #[test]
    fn the_section_4_table_row_by_row() {
        let m = Keymap::default();
        let at = |k: &str, c: Ctx| m.lookup(press(k), c);
        assert_eq!(at("tab", Ctx::Cards), Some(FocusNext));
        assert_eq!(at("shift+tab", Ctx::Prompt), Some(FocusPrev));
        assert_eq!(at("enter", Ctx::Prompt), Some(Submit));
        assert_eq!(at("enter", Ctx::Cards), Some(OpenTranscript));
        assert_eq!(at("enter", Ctx::Log), Some(OpenTranscript));
        assert_eq!(at("enter", Ctx::Rail), Some(ShowAdvice));
        for c in [Ctx::Cards, Ctx::Log, Ctx::Rail] {
            assert_eq!(at("up", c), Some(MoveUp));
            assert_eq!(at("k", c), Some(MoveUp));
            assert_eq!(at("down", c), Some(MoveDown));
            assert_eq!(at("j", c), Some(MoveDown));
        }
        for k in ["h", "left"] {
            assert_eq!(at(k, Ctx::Cards), Some(CardPrev));
        }
        for k in ["l", "right"] {
            assert_eq!(at(k, Ctx::Cards), Some(CardNext));
        }
        for c in [Ctx::Cards, Ctx::Transcript] {
            assert_eq!(at("x", c), Some(StopRun));
            assert_eq!(at("r", c), Some(RetryRun));
            assert_eq!(at("m", c), Some(MessageRun));
        }
        assert_eq!(at("R", Ctx::Cards), Some(ResumeRun));
        assert_eq!(at("b", Ctx::Cards), Some(ToggleBackground));
        assert_eq!(at("ctrl+b", Ctx::Log), Some(BackgroundWait));
        assert_eq!(at("t", Ctx::Log), Some(OpenLeadTranscript));
        assert_eq!(at("a", Ctx::Cards), Some(FocusRail));
        assert_eq!(at("p", Ctx::Cards), Some(JumpPermission));
        assert_eq!(at("y", Ctx::Permission), Some(Allow));
        assert_eq!(at("n", Ctx::Permission), Some(Deny));
        assert_eq!(at("a", Ctx::Permission), Some(AllowAlways));
        assert_eq!(at("e", Ctx::Permission), Some(EditInput));
        assert_eq!(at(":", Ctx::Cards), Some(OpenPalette));
        for c in [Ctx::Log, Ctx::Transcript] {
            assert_eq!(at("/", c), Some(Filter));
            assert_eq!(at("f", c), Some(ToggleFollow));
        }
        for c in [Ctx::Transcript, Ctx::Cards] {
            assert_eq!(at("[", c), Some(TimeBack));
            assert_eq!(at("]", c), Some(TimeForward));
        }
        assert_eq!(at("A", Ctx::Cards), Some(OpenAgents));
        assert_eq!(at("K", Ctx::Cards), Some(OpenAccounts));
        assert_eq!(at("?", Ctx::Cards), Some(OpenHelp));
        assert_eq!(at("ctrl+c", Ctx::Prompt), Some(CancelRun));
        assert_eq!(at("q", Ctx::Cards), Some(Quit));
        assert_eq!(at("esc", Ctx::Modal), Some(CloseModal));
        assert_eq!(at("esc", Ctx::Permission), Some(CloseModal));
        assert_eq!(at("q", Ctx::Transcript), Some(CloseModal));
        assert_eq!(at("esc", Ctx::Prompt), Some(ClearPrompt));
    }

    #[test]
    fn a_modal_takes_the_dashboard_keys_away() {
        let m = Keymap::default();
        assert_eq!(m.lookup(press("q"), Ctx::Modal), None);
        assert_eq!(m.lookup(press("tab"), Ctx::Modal), None);
        assert_eq!(m.lookup(press("a"), Ctx::Permission), Some(AllowAlways));
    }

    #[test]
    fn plain_printable_keys_are_typing_while_the_prompt_has_focus() {
        let m = Keymap::default();
        for k in ["t", "a", "p", "q", "?", ":", "A", "j", "x"] {
            assert_eq!(m.lookup(press(k), Ctx::Prompt), None, "{k}");
        }
        // Keys that cannot be typed still work from the prompt.
        assert_eq!(m.lookup(press("tab"), Ctx::Prompt), Some(FocusNext));
        assert_eq!(m.lookup(press("ctrl+b"), Ctx::Prompt), Some(BackgroundWait));
    }

    #[test]
    fn shifted_letters_match_either_way_a_terminal_reports_them() {
        let m = Keymap::default();
        let upper = KeyEvent { code: KeyCode::Char('R'), ctrl: false, alt: false, shift: true };
        let lower = KeyEvent { code: KeyCode::Char('r'), ctrl: false, alt: false, shift: true };
        assert_eq!(m.lookup(upper, Ctx::Cards), Some(ResumeRun));
        assert_eq!(m.lookup(lower, Ctx::Cards), Some(ResumeRun));
        assert_eq!(m.lookup(press("r"), Ctx::Cards), Some(RetryRun));
    }

    #[test]
    fn key_strings_parse() {
        assert_eq!(press("Ctrl+B"), press("ctrl+b"));
        assert_eq!(press("shift+r"), press("R"));
        assert_eq!(press("+").code, KeyCode::Char('+'));
        assert_eq!(press("ctrl++").code, KeyCode::Char('+'));
        assert_eq!(press("space").code, KeyCode::Char(' '));
        assert_eq!(press("f5").code, KeyCode::F(5));
        assert!(press("shift+tab").shift);
        assert_eq!(press("pgdn").code, KeyCode::PageDown);
        for bad in ["", "ctrl+", "hyper+x", "xx", "f99999"] {
            assert!(parse_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_override_replaces_every_key_of_its_action() {
        let (m, w) = Keymap::with_overrides(&[("move_down".into(), vec!["d".into()])]);
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(m.lookup(press("d"), Ctx::Log), Some(MoveDown));
        assert_eq!(m.lookup(press("j"), Ctx::Log), None);
        assert_eq!(m.lookup(press("down"), Ctx::Log), None);
        // The scopes stay those of the action.
        assert_eq!(m.lookup(press("d"), Ctx::Permission), None);
    }

    #[test]
    fn the_spec_examples_rebind() {
        let cfg = BTreeMap::from([
            ("stop_run".to_owned(), "X".to_owned()),
            ("open_palette".to_owned(), "ctrl+p".to_owned()),
        ]);
        let (m, w) = Keymap::load(&cfg, None);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(m.lookup(press("X"), Ctx::Cards), Some(StopRun));
        assert_eq!(m.lookup(press("x"), Ctx::Cards), None);
        assert_eq!(m.lookup(press("ctrl+p"), Ctx::Log), Some(OpenPalette));
    }

    #[test]
    fn keys_toml_sits_on_top_of_the_config_table() {
        let cfg = BTreeMap::from([("quit".to_owned(), "Q".to_owned())]);
        let toml = "quit = \"ctrl+q\"\nmove_up = [\"w\", \"up\"]\n";
        let (m, w) = Keymap::load(&cfg, Some(toml));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(m.lookup(press("ctrl+q"), Ctx::Cards), Some(Quit));
        assert_eq!(m.lookup(press("Q"), Ctx::Cards), None);
        assert_eq!(m.lookup(press("w"), Ctx::Cards), Some(MoveUp));
        assert_eq!(m.lookup(press("up"), Ctx::Cards), Some(MoveUp));
        // A `[keys]` table inside keys.toml works as well.
        let (m, _) = Keymap::load(&BTreeMap::new(), Some("[keys]\nquit = \"z\"\n"));
        assert_eq!(m.lookup(press("z"), Ctx::Cards), Some(Quit));
    }

    #[test]
    fn mistakes_and_clashes_warn_and_the_override_wins() {
        let ov = [
            ("nonsense".to_owned(), vec!["x".to_owned()]),
            ("quit".to_owned(), vec!["hyper+q".to_owned()]),
            ("open_help".to_owned(), vec!["x".to_owned()]),
        ];
        let (m, w) = Keymap::with_overrides(&ov);
        assert!(w.iter().any(|s| s.contains("unknown action `nonsense`")), "{w:?}");
        assert!(w.iter().any(|s| s.contains("cannot read key `hyper+q`")), "{w:?}");
        assert!(
            w.iter().any(|s| s.contains("`x` is bound to both open_help and stop_run")),
            "{w:?}"
        );
        assert_eq!(m.lookup(press("x"), Ctx::Cards), Some(OpenHelp));
        // The unreadable override left the default in place.
        assert_eq!(m.lookup(press("q"), Ctx::Cards), Some(Quit));
        let (_, w) = Keymap::load(&BTreeMap::new(), Some("this is not toml"));
        assert!(w[0].starts_with("keys.toml:"), "{w:?}");
    }

    #[test]
    fn help_rows_follow_the_live_map() {
        let rows = Keymap::default().help_rows();
        assert_eq!(rows.len(), ACTIONS.len());
        let up = rows.iter().find(|r| r.text == "up / scroll").unwrap();
        assert_eq!(up.keys, "Up k");
        let (m, _) = Keymap::with_overrides(&[("open_help".into(), vec!["F1".into()])]);
        let help = m.help_rows().into_iter().find(|r| r.text == "this help").unwrap();
        assert_eq!(help.keys, "F1");
    }
}
