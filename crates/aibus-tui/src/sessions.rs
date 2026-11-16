//! The file side of `/sessions` and `/tap` (#355), the way `runs` is for `/runs`. The reducer
//! cannot read files or start `claude agents --json`, so a `Command::Sessions` reaches the
//! terminal loop, which calls `apply` and steps what comes back: the text (`Msg::RunsLoaded`,
//! the same screen `/runs` uses) and the screen to open. The text is `aibus_tap::show`'s, the
//! same `aibus sessions` prints, cleaned of anything a terminal would act on. Read-only: nothing
//! here sends to a session.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aibus_core::Redactor;
use tokio::sync::mpsc;

use aibus_core::{Command, Msg, Screen, SessionsQuery};
use aibus_tap::{show, Env};

/// What `query` shows. `state` is aibus's state directory, `config` the CLI's; `lister` is the
/// `claude agents --json` call, passed in so a test never needs the CLI.
pub fn apply(
    query: &SessionsQuery,
    state: &Path,
    config: &Path,
    lister: &dyn Fn() -> Result<Vec<u8>, String>,
    redactor: &Redactor,
) -> Vec<Msg> {
    if config.as_os_str().is_empty() {
        return vec![Msg::IoError {
            context: "/sessions: cannot tell where Claude Code keeps its config: set CLAUDE_CONFIG_DIR or HOME".into(),
            error: String::new(),
        }];
    }
    let env = Env {
        state: state.to_owned(),
        config: config.to_owned(),
    };
    let shown = match query {
        SessionsQuery::List => {
            show::list(&env, lister, redactor).map(|t| ("sessions".to_owned(), t))
        }
        SessionsQuery::Tap(s) => show::tap(&env, lister, redactor, s)
            .map(|t| (format!("tap {}", aibus_trace::view::clean(s, 40)), t)),
    };
    match shown {
        Ok((title, text)) => vec![
            Msg::RunsLoaded { title, text },
            Msg::Command(Command::OpenScreen(Screen::Runs)),
        ],
        Err(why) => vec![Msg::IoError {
            context: format!("/sessions: {}", aibus_trace::view::clean(&why, 200)),
            error: String::new(),
        }],
    }
}

/// What the loop knows about where things are: aibus's state directory, the CLI's config
/// directory (`None` when neither `CLAUDE_CONFIG_DIR` nor `HOME` is set) and the session's redactor.
#[derive(Clone)]
pub struct Setup {
    pub state: PathBuf,
    pub config: Option<PathBuf>,
    pub redactor: Redactor,
}

/// Runs `apply` on a thread of its own and sends what it made to `tx`, so `claude agents --json`
/// never holds the screen. Returns at once.
pub fn spawn(
    query: &SessionsQuery,
    setup: &Setup,
    lister: Arc<dyn Fn() -> Result<Vec<u8>, String> + Send + Sync>,
    tx: mpsc::Sender<Msg>,
) {
    let (query, setup) = (query.clone(), setup.clone());
    std::thread::spawn(move || {
        let config = setup.config.clone().unwrap_or_default();
        for m in apply(&query, &setup.state, &config, &*lister, &setup.redactor) {
            if tx.blocking_send(m).is_err() {
                return;
            }
        }
    });
}
