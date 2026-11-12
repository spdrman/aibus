//! The prompt line (05 section 2.8): text with a cursor and a selection, up to 5 rows, history,
//! `Tab` completion and the slash commands. It is plain data and plain functions, so the key
//! handling can be tested without a terminal. The prompt is not in the graph: it changes with
//! every key press.

use aibus_core::Msg;

use crate::commands::{slash_offered, Env};

/// Rows the prompt grows to before it scrolls (`Alt+Enter`).
pub const MAX_ROWS: usize = 5;
/// The cursor is on for this many ticks, then off for as many: a 2 s period at 30 Hz.
pub const BLINK_TICKS: i64 = 30;
/// A prompt line shows a hint row under it for this many sessions.
pub const HINT_SESSIONS: u32 = 5;

/// The completion popup: candidates for the word that ends at the cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Popup {
    pub items: Vec<String>,
    pub sel: usize,
    /// Char index where the completed word starts.
    pub from: usize,
}

/// The prompt line: `<cwd> $ ` and what the user has typed, or the dim placeholder until the
/// first submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub cwd: String,
    pub text: String,
    pub placeholder: String,
    /// Char index of the cursor in `text`. A value past the end counts as the end.
    pub cursor: usize,
    /// The other end of the selection, when there is one.
    pub anchor: Option<usize>,
    pub history: Vec<String>,
    hist_at: Option<usize>,
    draft: String,
    /// Something was submitted: the placeholder is gone for good.
    pub submitted: bool,
    /// Show the hint row (`[ui] hints`, the first five sessions).
    pub hints: bool,
    pub popup: Option<Popup>,
    /// Rows scrolled off the top of a prompt taller than `MAX_ROWS`.
    pub scroll: usize,
    /// What the last Enter could not make sense of; it shows under the prompt until the next key.
    pub notice: Option<String>,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            cwd: "~/app".into(),
            text: String::new(),
            placeholder: "aibus --model sonnet --advisor opus".into(),
            cursor: 0,
            anchor: None,
            history: Vec::new(),
            hist_at: None,
            draft: String::new(),
            submitted: false,
            hints: false,
            popup: None,
            scroll: 0,
            notice: None,
        }
    }
}

/// What the Enter key means once the prompt is read.
#[derive(Clone, Debug)]
pub enum Sent {
    /// Goes to the reducer as `SubmitPrompt` (or is a slash command the reducer already knows).
    Msgs(Vec<Msg>),
    /// `/login <provider>`: the messages, and the provider whose add flow starts in the terminal.
    Login(String, Vec<Msg>),
    /// A command the dashboard cannot make sense of; the line says why.
    Bad(String),
}

fn char_at(s: &str, i: usize) -> usize {
    s.char_indices().nth(i).map_or(s.len(), |(b, _)| b)
}

impl Prompt {
    /// A prompt that already holds `text` (the one given on the command line), cursor at the end.
    pub fn with_text(mut self, text: &str) -> Self {
        self.set(text);
        self
    }

    /// Replaces the text and puts the cursor at its end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.len();
        self.anchor = None;
        self.popup = None;
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The cursor, clamped (tests and startup assign `text` directly).
    pub fn at(&self) -> usize {
        self.cursor.min(self.len())
    }

    /// Lines of the text.
    pub fn lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }

    /// Rows the prompt takes: one per line, up to `MAX_ROWS`.
    pub fn rows(&self) -> u16 {
        self.lines().len().clamp(1, MAX_ROWS) as u16
    }

    /// `(line, column)` of a char index.
    pub fn pos(&self, at: usize) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for (i, c) in self.text.chars().enumerate() {
            if i == at {
                break;
            }
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// The char index of `(line, column)`, the column clamped to the line.
    pub fn index(&self, line: usize, col: usize) -> usize {
        let mut start = 0;
        for (i, l) in self.text.split('\n').enumerate() {
            let n = l.chars().count();
            if i == line {
                return start + col.min(n);
            }
            start += n + 1;
        }
        self.len()
    }

    /// The selected text, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?.min(self.len());
        let c = self.at();
        (a != c).then_some((a.min(c), a.max(c)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.text.chars().skip(a).take(b - a).collect())
    }

    fn drop_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let (from, to) = (char_at(&self.text, a), char_at(&self.text, b));
        self.text.replace_range(from..to, "");
        self.cursor = a;
        self.anchor = None;
        true
    }

    fn edited(&mut self) {
        self.hist_at = None;
        self.popup = None;
    }

    pub fn insert(&mut self, c: char) {
        self.drop_selection();
        let at = char_at(&self.text, self.at());
        self.text.insert(at, c);
        self.cursor = self.at() + 1;
        self.edited();
    }

    /// Pasted text: a dropped file path stays a path (quoted when it has spaces) and is never
    /// run as a command; newlines stay newlines.
    pub fn paste(&mut self, pasted: &str) {
        // A terminal sends CRLF, LF or (xterm style) a bare CR for each newline of a paste.
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let trimmed = pasted.trim_end_matches('\n');
        let one_line = !trimmed.contains('\n');
        let insert = if one_line && looks_like_path(trimmed) { quote_path(trimmed) } else { trimmed.to_owned() };
        self.drop_selection();
        let at = char_at(&self.text, self.at());
        self.text.insert_str(at, &insert);
        self.cursor = self.at() + insert.chars().count();
        self.edited();
    }

    pub fn newline(&mut self) {
        self.insert('\n');
    }

    pub fn backspace(&mut self) {
        if self.drop_selection() {
            self.edited();
            return;
        }
        let at = self.at();
        if at > 0 {
            let (a, b) = (char_at(&self.text, at - 1), char_at(&self.text, at));
            self.text.replace_range(a..b, "");
            self.cursor = at - 1;
        }
        self.edited();
    }

    pub fn delete(&mut self) {
        if self.drop_selection() {
            self.edited();
            return;
        }
        let at = self.at();
        if at < self.len() {
            let (a, b) = (char_at(&self.text, at), char_at(&self.text, at + 1));
            self.text.replace_range(a..b, "");
        }
        self.edited();
    }

    /// Moves the cursor; `select` keeps or starts a selection, else the selection is dropped.
    pub fn go(&mut self, to: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.at());
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(self.len());
        self.popup = None;
    }

    pub fn left(&mut self, select: bool) {
        let at = self.at();
        // Without shift, a selection collapses to its near edge.
        let to = match (self.selection(), select) {
            (Some((a, _)), false) => a,
            _ => at.saturating_sub(1),
        };
        self.go(to, select);
    }

    pub fn right(&mut self, select: bool) {
        let at = self.at();
        let to = match (self.selection(), select) {
            (Some((_, b)), false) => b,
            _ => at + 1,
        };
        self.go(to, select);
    }

    pub fn home(&mut self, select: bool) {
        let (line, _) = self.pos(self.at());
        self.go(self.index(line, 0), select);
    }

    pub fn end(&mut self, select: bool) {
        let (line, _) = self.pos(self.at());
        self.go(self.index(line, usize::MAX), select);
    }

    /// Up inside a multi-line prompt moves the cursor; on the first line it walks the history.
    pub fn up(&mut self) {
        let (line, col) = self.pos(self.at());
        if line > 0 {
            self.go(self.index(line - 1, col), false);
            return;
        }
        if self.history.is_empty() {
            return;
        }
        let at = match self.hist_at {
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.hist_at = Some(at);
        let text = self.history[at].clone();
        self.set(&text);
    }

    pub fn down(&mut self) {
        let (line, col) = self.pos(self.at());
        if line + 1 < self.lines().len() {
            self.go(self.index(line + 1, col), false);
            return;
        }
        let Some(i) = self.hist_at else { return };
        if i + 1 < self.history.len() {
            self.hist_at = Some(i + 1);
            let text = self.history[i + 1].clone();
            self.set(&text);
        } else {
            self.hist_at = None;
            let draft = std::mem::take(&mut self.draft);
            self.set(&draft);
        }
    }

    /// `Esc`: closes the popup if one is open, else clears the line.
    pub fn escape(&mut self) -> bool {
        if self.popup.take().is_some() {
            return true;
        }
        let had = !self.text.is_empty();
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.hist_at = None;
        had
    }

    /// The word before the cursor and where it starts.
    fn word(&self) -> (usize, String) {
        let before: Vec<char> = self.text.chars().take(self.at()).collect();
        let start = before.iter().rposition(|c| c.is_whitespace()).map_or(0, |i| i + 1);
        (start, before[start..].iter().collect())
    }

    /// Candidates for the word at the cursor: `@agent-` names, and slash commands for the first
    /// word of the line. `workflows` are the shorthands that exist.
    pub fn candidates(&self, env: &Env) -> (usize, Vec<String>) {
        let (from, word) = self.word();
        if word.starts_with('@') {
            let typed = word.trim_start_matches('@').trim_start_matches("agent-");
            let items = env
                .agents
                .iter()
                .filter(|a| a.starts_with(typed))
                .map(|a| format!("@agent-{a}"))
                .collect();
            return (from, items);
        }
        if word.starts_with('/') && self.text.chars().take(from).all(char::is_whitespace) {
            let all = slash_offered(env).into_iter().map(|(name, _)| name);
            return (from, all.filter(|c| c.starts_with(&word)).collect());
        }
        (from, Vec::new())
    }

    /// `Tab`: a single candidate is inserted; several open the popup, and `Tab` again walks it.
    pub fn tab(&mut self, env: &Env) {
        if let Some(p) = &mut self.popup {
            p.sel = (p.sel + 1) % p.items.len().max(1);
            return;
        }
        let (from, items) = self.candidates(env);
        match items.len() {
            0 => {}
            1 => self.accept(from, &items[0]),
            _ => self.popup = Some(Popup { items, sel: 0, from }),
        }
    }

    pub fn shift_tab(&mut self) {
        if let Some(p) = &mut self.popup {
            p.sel = (p.sel + p.items.len().max(1) - 1) % p.items.len().max(1);
        }
    }

    fn accept(&mut self, from: usize, item: &str) {
        let (a, b) = (char_at(&self.text, from), char_at(&self.text, self.at()));
        self.text.replace_range(a..b, &format!("{item} "));
        self.cursor = from + item.chars().count() + 1;
        self.popup = None;
        self.anchor = None;
    }

    /// `Enter` while the popup is open takes the highlighted candidate.
    pub fn accept_popup(&mut self) -> bool {
        let Some(p) = self.popup.take() else { return false };
        if let Some(item) = p.items.get(p.sel) {
            self.accept(p.from, item);
        }
        true
    }

    /// Takes the line for sending: trims it, remembers it in the history, empties the prompt and
    /// drops the placeholder. `None` for a blank line.
    pub fn take(&mut self) -> Option<String> {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        // A key typed after `/login <provider>` must not be recalled with the up arrow.
        if self.history.last() != Some(&text) && !crate::commands::carries_key(&text) {
            self.history.push(text.clone());
        }
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.hist_at = None;
        self.popup = None;
        self.scroll = 0;
        self.submitted = true;
        Some(text)
    }

    /// Whether the placeholder shows: empty, and nothing sent yet.
    pub fn shows_placeholder(&self) -> bool {
        self.text.is_empty() && !self.submitted
    }

    /// The cursor shows for 30 ticks, then not for 30.
    pub fn blink_on(tick: i64) -> bool {
        (tick / BLINK_TICKS) % 2 == 0
    }
}

fn looks_like_path(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && (s.starts_with('/') || s.starts_with("~/") || s.starts_with("./") || s.starts_with("file://"))
        && !s.contains(['|', ';', '&', '$', '`', '>', '<'])
}

/// A path as it goes in the line: `file://` dropped, escaped spaces undone, quoted if it still
/// has a space.
fn quote_path(s: &str) -> String {
    let s = s.trim().strip_prefix("file://").unwrap_or(s.trim()).replace("\\ ", " ");
    if s.contains(' ') {
        format!("'{s}'")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(p: &mut Prompt, s: &str) {
        for c in s.chars() {
            p.insert(c);
        }
    }

    #[test]
    fn edits_land_at_the_cursor_and_the_cursor_moves_with_them() {
        let mut p = Prompt::default();
        typed(&mut p, "helo");
        p.left(false);
        p.insert('l');
        assert_eq!((p.text.as_str(), p.at()), ("hello", 4));
        p.home(false);
        p.delete();
        assert_eq!(p.text, "ello");
        p.end(false);
        p.backspace();
        assert_eq!(p.text, "ell");
    }

    #[test]
    fn a_selection_is_replaced_by_the_next_character() {
        let mut p = Prompt::default().with_text("hello world");
        p.go(5, false);
        p.go(0, true);
        assert_eq!(p.selected_text().as_deref(), Some("hello"));
        p.insert('J');
        assert_eq!(p.text, "J world");
        // Moving without shift drops it, to the near edge.
        p.go(0, false);
        p.right(true);
        p.right(true);
        p.left(false);
        assert_eq!((p.selection(), p.at()), (None, 0));
    }

    #[test]
    fn alt_enter_grows_the_prompt_to_five_rows_and_no_further() {
        let mut p = Prompt::default();
        assert_eq!(p.rows(), 1);
        for i in 0..7 {
            typed(&mut p, &format!("l{i}"));
            p.newline();
        }
        assert_eq!(p.rows(), 5);
        assert_eq!(p.lines().len(), 8);
        // Up and down move between lines before they touch the history.
        p.history.push("old".into());
        p.up();
        assert_eq!(p.pos(p.at()).0, 6);
    }

    #[test]
    fn history_walks_back_and_forward_and_keeps_the_draft() {
        let mut p = Prompt::default();
        for line in ["one", "two", "three"] {
            p.set(line);
            assert_eq!(p.take().as_deref(), Some(line));
        }
        typed(&mut p, "dra");
        p.up();
        assert_eq!(p.text, "three");
        p.up();
        p.up();
        p.up();
        assert_eq!(p.text, "one", "stops at the oldest");
        p.down();
        assert_eq!(p.text, "two");
        p.down();
        p.down();
        assert_eq!(p.text, "dra", "the draft comes back");
        // The same line twice in a row is one history entry.
        p.set("three");
        p.take();
        assert_eq!(p.history.len(), 3);
    }

    #[test]
    fn escape_closes_the_popup_first_then_clears_the_line() {
        let mut p = Prompt::default().with_text("/a");
        p.tab(&Env::bare(&[], &[]));
        assert!(p.popup.is_some());
        assert!(p.escape());
        assert!(p.popup.is_none() && p.text == "/a");
        assert!(p.escape());
        assert!(p.text.is_empty());
    }

    #[test]
    fn tab_completes_slash_commands_and_agent_names() {
        let agents = vec!["search".to_owned(), "grep".to_owned(), "audit".to_owned()];
        let mut p = Prompt::default().with_text("/comp");
        p.tab(&Env::bare(&agents, &[]));
        assert_eq!(p.text, "/compact ", "one candidate is taken at once");
        let mut p = Prompt::default().with_text("/");
        p.tab(&Env::bare(&agents, &[]));
        let popup = p.popup.clone().unwrap();
        assert_eq!(popup.items.len(), 15);
        p.tab(&Env::bare(&agents, &[]));
        assert_eq!(p.popup.as_ref().unwrap().sel, 1);
        assert!(p.accept_popup());
        assert_eq!(p.text, "/delegate ");
        let mut p = Prompt::default().with_text("ask @agent-gr");
        p.tab(&Env::bare(&agents, &[]));
        assert_eq!(p.text, "ask @agent-grep ");
        let mut p = Prompt::default().with_text("@");
        p.tab(&Env::bare(&agents, &[]));
        assert_eq!(p.popup.as_ref().unwrap().items, ["@agent-search", "@agent-grep", "@agent-audit"]);
        // A slash in the middle of a sentence is not a command.
        let mut p = Prompt::default().with_text("see /com");
        p.tab(&Env::bare(&agents, &[]));
        assert!(p.popup.is_none() && p.text == "see /com");
    }

    #[test]
    fn workflow_shorthands_complete_only_when_the_workflow_exists() {
        let mut p = Prompt::default().with_text("/fi");
        p.tab(&Env::bare(&[], &[]));
        assert_eq!(p.text, "/fi");
        p.tab(&Env::bare(&[], &["fix".to_owned()]));
        assert_eq!(p.text, "/fix ");
    }

    #[test]
    fn the_placeholder_stays_until_the_first_submit() {
        let mut p = Prompt::default();
        assert!(p.shows_placeholder());
        typed(&mut p, "x");
        assert!(!p.shows_placeholder());
        p.backspace();
        assert!(p.shows_placeholder(), "still nothing sent");
        typed(&mut p, "go");
        p.take();
        assert!(!p.shows_placeholder(), "after the first submit the line is just empty");
        assert!(p.take().is_none(), "a blank line is not sent");
    }

    #[test]
    fn the_cursor_blinks_on_a_two_second_period_from_the_tick_counter() {
        let phases: Vec<bool> = [0, 29, 30, 59, 60, 89, 90].map(Prompt::blink_on).to_vec();
        assert_eq!(phases, [true, true, false, false, true, true, false]);
    }

    #[test]
    fn a_paste_keeps_its_line_breaks_whatever_newline_the_terminal_sent() {
        // Issue #197: xterm style terminals put a bare CR for each newline inside a bracketed
        // paste. LF and CRLF always worked; the CR was deleted and the lines ran together.
        for (name, pasted) in [("LF", "one\ntwo\nthree"), ("CRLF", "one\r\ntwo\r\nthree"), ("CR", "one\rtwo\rthree")] {
            let mut p = Prompt::default();
            p.paste(pasted);
            assert_eq!(p.text, "one\ntwo\nthree", "{name} newlines");
        }
    }

    #[test]
    fn a_pasted_path_is_inserted_as_a_path_and_not_run() {
        let mut p = Prompt::default().with_text("read ");
        p.paste("/home/me/my file.txt\n");
        assert_eq!(p.text, "read '/home/me/my file.txt'");
        let mut p = Prompt::default();
        p.paste("file:///tmp/a.png");
        assert_eq!(p.text, "/tmp/a.png");
        // Text with a newline in it is pasted as text, newlines kept.
        let mut p = Prompt::default();
        p.paste("ls /tmp\nrm -rf x");
        assert_eq!(p.text, "ls /tmp\nrm -rf x");
        assert!(p.take().is_some_and(|t| t.starts_with("ls")), "nothing was submitted by the paste itself");
        // A path-looking command is not rewritten.
        let mut p = Prompt::default();
        p.paste("/bin/sh -c 'x'; y");
        assert_eq!(p.text, "/bin/sh -c 'x'; y");
    }
}
