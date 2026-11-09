//! Terminal setup, the panic hook and the event loop (the only file that touches a real tty).

use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aibus_core::state::{run_key, ANIMATING};
use aibus_core::{
    AibusError, App, Command, KeyCode, KeyEvent, Msg, Ticker, UiIntent, MSG_CAPACITY,
};
use aibus_graph::{Graph, Value};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::input::{self, Clip, Context, Input, MouseMode};
use crate::keys::Keymap;
use crate::layout::Layout;
use crate::theme::Theme;
use crate::ui::{Chrome, Prompt, Ui};
use crate::view;

/// How many times `restore` has run. The panic-hook test reads it.
pub static RESTORES: AtomicUsize = AtomicUsize::new(0);

/// The second `Ctrl+c` within this window quits (05 section 4).
const QUIT_WINDOW: Duration = Duration::from_secs(2);

/// What the terminal loop is told by whoever launched it.
pub struct Options {
    pub hz: u32,
    pub theme: Theme,
    pub keys: Keymap,
    pub prompt: Prompt,
    /// `[ui] mouse` and `--no-mouse`.
    pub mouse: bool,
    /// `$TERM`, to turn the mouse off where the terminal cannot report it.
    pub term: Option<String>,
    /// `state/ui.toml`, where dragged sizes are kept.
    pub layout_file: Option<PathBuf>,
    /// `[ui] log_rows`: the log's height until the user drags its border.
    pub log_rows: u16,
    /// Messages from runs started outside the loop (the orchestrator's channel).
    pub msgs: Option<mpsc::Receiver<Msg>>,
    /// Sees every message before the loop steps it, for what must follow `AgentsReloaded` and
    /// the like outside the graph.
    #[allow(clippy::type_complexity)]
    pub watch: Option<Box<dyn FnMut(&Msg)>>,
    /// Samples resident memory in MiB for the status bar's `mem` segment (`-v`).
    pub mem: Option<fn() -> Option<u32>>,
}

/// What a batch of terminal events asks of the loop besides the messages it stepped.
#[derive(Debug, Default)]
pub struct Handled {
    /// A border drag ended: the sizes are final and worth saving.
    pub layout_done: bool,
    /// Text a menu pick wants on the clipboard.
    pub copies: Vec<Clip>,
}

/// `text` for the terminal to put on the clipboard: an OSC 52 sequence, base64 inside.
pub fn osc52(text: &str) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::from("\x1b]52;c;");
    for chunk in text.as_bytes().chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |a, (i, b)| a | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out.push('\x07');
    out
}

/// Leaves the alternate screen, shows the cursor, stops mouse reporting and drops raw mode. Safe
/// to call twice.
pub fn restore() {
    RESTORES.fetch_add(1, Ordering::SeqCst);
    let mut out = io::stdout();
    let _ = out.write_all(input::mouse_switch(MouseMode::Hover, MouseMode::Off).as_bytes());
    let _ = execute!(out, LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Restores the terminal before the previous hook prints the panic, so the message is readable.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

/// Raw mode plus the alternate screen, undone on drop.
pub struct Session {
    pub term: Terminal<CrosstermBackend<Stdout>>,
}

impl Session {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(e) = execute!(out, EnterAlternateScreen, cursor::Hide) {
            restore();
            return Err(e);
        }
        Ok(Self {
            term: Terminal::new(CrosstermBackend::new(out))?,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        restore();
    }
}

/// Counts `Ctrl+c` presses: the second inside the window means quit.
#[derive(Default)]
pub struct CtrlC {
    last: Option<Instant>,
}

impl CtrlC {
    pub fn press(&mut self, now: Instant) -> bool {
        let quit = self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) <= QUIT_WINDOW);
        self.last = if quit { None } else { Some(now) };
        quit
    }
}

pub fn key_from_crossterm(k: event::KeyEvent) -> Option<KeyEvent> {
    use crossterm::event::{KeyCode as K, KeyModifiers as M};
    if k.kind == KeyEventKind::Release {
        return None;
    }
    let mut shift = k.modifiers.contains(M::SHIFT);
    let code = match k.code {
        K::Char(c) => KeyCode::Char(c),
        K::Enter => KeyCode::Enter,
        K::Esc => KeyCode::Esc,
        K::Tab => KeyCode::Tab,
        // Shift+Tab arrives as its own key code.
        K::BackTab => {
            shift = true;
            KeyCode::Tab
        }
        K::Backspace => KeyCode::Backspace,
        K::Up => KeyCode::Up,
        K::Down => KeyCode::Down,
        K::Left => KeyCode::Left,
        K::Right => KeyCode::Right,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::F(n) => KeyCode::F(n),
        _ => return None,
    };
    Some(KeyEvent {
        code,
        ctrl: k.modifiers.contains(M::CONTROL),
        alt: k.modifiers.contains(M::ALT),
        shift,
    })
}

/// Handles a batch of terminal events: each one becomes messages that are stepped before the
/// next event is looked at, so a click that follows a click sees what the first one did. Pointer
/// and prompt state change on `input` and `ui`.
pub fn terminal_events<G: Graph>(
    events: Vec<Event>,
    app: &mut App<G>,
    ui: &mut Ui,
    input: &mut Input,
    ctrl_c: &mut CtrlC,
    mouse: bool,
    now: Instant,
) -> Result<Handled, AibusError> {
    let mut done = Handled::default();
    for ev in input::coalesce(events) {
        let cx = Context::read(&app.graph);
        let out = match ev {
            Event::Key(k) => {
                let Some(key) = key_from_crossterm(k) else { continue };
                if key.ctrl && key.code == KeyCode::Char('c') && ctrl_c.press(now) {
                    app.step(Msg::Command(Command::Quit { force: false }))?;
                    continue;
                }
                input.key(key, &cx, &ui.keys, &ui.drawn, &mut ui.prompt.text)
            }
            Event::Mouse(m) if mouse => input.mouse(m, now, &ui.drawn, &cx),
            Event::Resize(cols, rows) => {
                app.step(Msg::Resize { cols, rows })?;
                continue;
            }
            _ => continue,
        };
        for msg in out.msgs {
            app.step(msg)?;
        }
        done.layout_done |= out.layout_done;
        done.copies.extend(out.copy);
    }
    ui.pointer = input.pointer;
    ui.filter_edit = input.filter_edit;
    Ok(done)
}

/// What a menu pick wants copied, as text. A run's result is the last block of its transcript
/// (or what is still being written), which is what the run produced.
fn clip_text<G: Graph>(app: &App<G>, clip: &Clip) -> Option<String> {
    match clip {
        Clip::Text(t) => Some(t.clone()),
        Clip::Result(run) => {
            let Some(Value::Handle(h)) = app.graph.read_key(&run_key(run, "blocks")) else {
                return None;
            };
            let tail = app.store.tail(h.id);
            if !tail.is_empty() {
                return Some(tail);
            }
            app.store.blocks(h.id).last().map(|b| b.text.to_string())
        }
    }
}

/// Runs the dashboard until quit. Registers the `view:` nodes, draws once, then draws only when
/// `view:dirty` names a widget, the UI state or the pointer changed, or the terminal is resized.
/// Ticks run only while `view:animating` is true, or while the rail is typing out advice.
pub async fn run<G: Graph>(mut app: App<G>, mut opts: Options) -> Result<(), AibusError> {
    install_panic_hook();
    let keys = view::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    let mut session = Session::enter()?;
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(256);
    let ticker = Ticker::spawn(tx.clone(), opts.hz);
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if ev_tx.blocking_send(ev).is_err() {
                return;
            }
        }
    });

    let mut ui = Ui::new(opts.theme);
    ui.keys = opts.keys;
    ui.prompt = opts.prompt;
    let mut input = Input::default();
    let mut ctrl_c = CtrlC::default();
    let supported = input::mouse_supported(opts.term.as_deref());
    let mut mode = MouseMode::Off;
    let mut runs = opts.msgs.take();
    let mut watch = opts.watch.take();
    let mut mem = tokio::time::interval(Duration::from_secs(2));

    let size = session.term.size()?;
    app.step(Msg::Resize { cols: size.width, rows: size.height })?;
    let saved = opts.layout_file.as_deref().map(Layout::load).unwrap_or_default();
    if let Some(w) = saved.rail {
        app.step(Msg::Ui(UiIntent::SetRailWidth(w)))?;
    }
    // Dragged sizes win; else the configured number of log rows (and its border).
    match saved.log {
        Some(h) => app.step(Msg::Ui(UiIntent::SetLogRows(h)))?,
        None if opts.log_rows != 4 => {
            app.step(Msg::Ui(UiIntent::SetLogRows(opts.log_rows.max(3) + 2)))?;
        }
        None => {}
    }
    ui.note_commit(&app.graph);
    ui.draw(&mut session.term, &app.graph)?;
    loop {
        let mut done = Handled::default();
        let tip_due = input.tip_due(Instant::now());
        let tip_timer = async {
            match tip_due {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };
        let next_run = async {
            match runs.as_mut() {
                Some(r) => r.recv().await,
                None => std::future::pending().await,
            }
        };
        let sample = async {
            match opts.mem {
                Some(_) => mem.tick().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(m) = rx.recv() => app.step(m)?,
            Some(m) = next_run => {
                if let Some(f) = watch.as_mut() {
                    f(&m);
                }
                app.step(m)?;
            }
            Some(first) = ev_rx.recv() => {
                // Everything that piled up while the last frame drew is one batch.
                let mut batch = vec![first];
                while batch.len() < 256 {
                    let Ok(e) = ev_rx.try_recv() else { break };
                    batch.push(e);
                }
                done = terminal_events(
                    batch, &mut app, &mut ui, &mut input, &mut ctrl_c, opts.mouse, Instant::now(),
                )?;
            }
            () = tip_timer => {}
            _ = sample => {
                if let Some(mib) = opts.mem.and_then(|f| f()) {
                    app.step(Msg::Ui(UiIntent::Mem(mib)))?;
                }
            }
            else => break,
        }
        // A tooltip comes up once the pointer has rested long enough.
        input.settle(Instant::now());
        ui.pointer = input.pointer;
        ui.filter_edit = input.filter_edit;
        for clip in &done.copies {
            if let Some(text) = clip_text(&app, clip) {
                let mut out = io::stdout();
                out.write_all(osc52(&text).as_bytes())?;
                out.flush()?;
            }
        }
        ui.note_commit(&app.graph);
        let typing = app.graph.read_key(view::RAIL).is_some_and(view::rail_typing);
        let animating = typing || matches!(app.graph.read_key(ANIMATING), Some(Value::Bool(true)));
        ticker.set_animating(animating);
        ui.draw(&mut session.term, &app.graph)?;
        // Move events are only asked for while something on this frame reacts to hover.
        let want = input::mouse_mode(opts.mouse, supported, ui.drawn.hits.hoverable());
        if want != mode {
            let mut out = io::stdout();
            out.write_all(input::mouse_switch(mode, want).as_bytes())?;
            out.flush()?;
            mode = want;
        }
        if done.layout_done {
            if let Some(file) = &opts.layout_file {
                // A failed save is not worth interrupting the session for.
                let _ = Chrome::read(&app.graph).layout.save(file);
            }
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clipboard_sequence_is_osc_52_with_standard_base64() {
        // Every remainder of the 3-byte groups: none, two bytes short, one byte short.
        for (text, b64) in [("", ""), ("a", "YQ=="), ("ab", "YWI="), ("abc", "YWJj"), ("hello", "aGVsbG8="), ("01HF7YAT", "MDFIRjdZQVQ=")] {
            assert_eq!(osc52(text), format!("\x1b]52;c;{b64}\x07"), "{text:?}");
        }
    }

    #[test]
    fn second_ctrl_c_inside_two_seconds_quits() {
        let mut c = CtrlC::default();
        let t = Instant::now();
        assert!(!c.press(t));
        assert!(c.press(t + Duration::from_millis(1900)));
        // The pair is spent: the next press starts a new window.
        assert!(!c.press(t + Duration::from_millis(2000)));
    }

    #[test]
    fn a_slow_second_ctrl_c_does_not_quit() {
        let mut c = CtrlC::default();
        let t = Instant::now();
        assert!(!c.press(t));
        assert!(!c.press(t + Duration::from_millis(2100)));
        assert!(c.press(t + Duration::from_millis(3000)));
    }

    #[test]
    fn the_panic_hook_restores_the_terminal() {
        install_panic_hook();
        let before = RESTORES.load(Ordering::SeqCst);
        let r = std::panic::catch_unwind(|| panic!("boom"));
        assert!(r.is_err());
        assert!(RESTORES.load(Ordering::SeqCst) > before);
    }

    #[test]
    fn keys_convert_and_releases_are_dropped() {
        use crossterm::event::{KeyCode as K, KeyEventState, KeyModifiers};
        let mut k = event::KeyEvent::new(K::Char('c'), KeyModifiers::CONTROL);
        let got = key_from_crossterm(k).unwrap();
        assert!(got.ctrl && got.code == KeyCode::Char('c'));
        k.kind = KeyEventKind::Release;
        k.state = KeyEventState::NONE;
        assert!(key_from_crossterm(k).is_none());
    }

    #[test]
    fn back_tab_is_shift_tab() {
        use crossterm::event::{KeyCode as K, KeyModifiers};
        let got = key_from_crossterm(event::KeyEvent::new(K::BackTab, KeyModifiers::SHIFT)).unwrap();
        assert_eq!((got.code, got.shift), (KeyCode::Tab, true));
        let plain = key_from_crossterm(event::KeyEvent::new(K::Tab, KeyModifiers::NONE)).unwrap();
        assert!(!plain.shift);
    }
}
