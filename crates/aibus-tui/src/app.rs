//! Terminal setup, the panic hook and the event loop (the only file that touches a real tty).

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aibus_core::state::ANIMATING;
use aibus_core::{
    AibusError, App, Command, KeyCode, KeyEvent, Msg, Ticker, MSG_CAPACITY,
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

use crate::theme::Theme;
use crate::ui::Ui;
use crate::view;

/// How many times `restore` has run. The panic-hook test reads it.
pub static RESTORES: AtomicUsize = AtomicUsize::new(0);

/// The second `Ctrl+c` within this window quits (05 section 4).
const QUIT_WINDOW: Duration = Duration::from_secs(2);

/// Leaves the alternate screen, shows the cursor and drops raw mode. Safe to call twice.
pub fn restore() {
    RESTORES.fetch_add(1, Ordering::SeqCst);
    let mut out = io::stdout();
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
    let code = match k.code {
        K::Char(c) => KeyCode::Char(c),
        K::Enter => KeyCode::Enter,
        K::Esc => KeyCode::Esc,
        K::Tab => KeyCode::Tab,
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
        shift: k.modifiers.contains(M::SHIFT),
    })
}

/// Runs the dashboard until quit. Registers the `view:` nodes, draws once, then draws only when
/// `view:dirty` names a widget or the terminal is resized. Ticks run only while
/// `view:animating` is true.
pub async fn run<G: Graph>(mut app: App<G>, hz: u32) -> Result<(), AibusError> {
    install_panic_hook();
    let keys = view::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    let mut session = Session::enter()?;
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let ticker = Ticker::spawn(tx.clone(), hz);
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let msg = match ev {
                Event::Key(k) => key_from_crossterm(k).map(Msg::Key),
                Event::Resize(cols, rows) => Some(Msg::Resize { cols, rows }),
                _ => None,
            };
            if let Some(m) = msg {
                if tx.blocking_send(m).is_err() {
                    return;
                }
            }
        }
    });

    let mut ui = Ui::new(Theme::recording());
    let mut ctrl_c = CtrlC::default();
    ui.note_commit(&app.graph);
    ui.draw(&mut session.term, &app.graph)?;
    while let Some(mut msg) = rx.recv().await {
        if let Msg::Key(k) = &msg {
            if k.ctrl && k.code == KeyCode::Char('c') && ctrl_c.press(Instant::now()) {
                msg = Msg::Command(Command::Quit { force: false });
            }
        }
        app.step(msg)?;
        ui.note_commit(&app.graph);
        let animating = matches!(app.graph.read_key(ANIMATING), Some(Value::Bool(true)));
        ticker.set_animating(animating);
        ui.draw(&mut session.term, &app.graph)?;
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
}
