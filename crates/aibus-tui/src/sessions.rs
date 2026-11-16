//! The file side of `/sessions` and `/tap` (#355), the way `runs` is for `/runs`. The reducer
//! cannot read files or start `claude agents --json`, so a `Command::Sessions` reaches the
//! terminal loop, which calls `apply` and steps what comes back: the text (`Msg::RunsLoaded`,
//! the same screen `/runs` uses) and the screen to open. The text is `aibus_tap::show`'s, the
//! same `aibus sessions` prints, cleaned of anything a terminal would act on. Read-only: nothing
//! here sends to a session.

use std::path::Path;

use aibus_core::{Command, Msg, Screen, SessionsQuery};
use aibus_tap::{show, Env};

/// What `query` shows. `state` is aibus's state directory, `config` the CLI's; `lister` is the
/// `claude agents --json` call, passed in so a test never needs the CLI.
pub fn apply(
    query: &SessionsQuery,
    state: &Path,
    config: &Path,
    lister: &dyn Fn() -> Result<Vec<u8>, String>,
) -> Vec<Msg> {
    let env = Env {
        state: state.to_owned(),
        config: config.to_owned(),
    };
    let shown = match query {
        SessionsQuery::List => show::list(&env, lister).map(|t| ("sessions".to_owned(), t)),
        SessionsQuery::Tap(s) => show::tap(&env, lister, s)
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
