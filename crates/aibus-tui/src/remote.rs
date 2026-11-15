//! A client of `aibus mcp` driving this session (#274). Whatever it sends becomes the messages
//! the keyboard would have produced for the same line, through the same parser (`commands::send`),
//! and is stepped through the same `step` a key's messages take, wrapped in `Msg::Via` so the log,
//! the run's meta and the transcript say who sent it.
//!
//! What stays the keyboard's is decided by what a line would do (`effect`), never by the
//! command's name, so an alias or a later command is covered: ending the session, any
//! confirmation (`/logout`, `/clear`, `/cancel` on the lead), writing files, opening any screen with
//! live letter keys (`/accounts`, `/model`, `/agents`, `/workflow list`), and opening or closing an
//! inert one (help, cost, status) while the user has a screen open or is typing (`busy`). A permission
//! prompt is answered by a client only when the session was started with `--control-permission`,
//! and `/login` opens a masked key entry, which is the keyboard's too.

use aibus_core::remote::{client_name, Ask, Remote};
use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::state::{run_key, PERMISSIONS_OPEN};
use aibus_core::{
    AibusError, App, ClientEvent, Command, Decision, ModelTarget, Msg, PermissionId, RunId, Screen,
    UiIntent, WorkflowFile,
};
use aibus_graph::{Graph, Value};
use serde_json::{json, Value as Json};

use crate::app::StepFn;
use crate::commands;
use crate::input::Context;
use crate::prompt::Sent;
use crate::ui::Ui;

/// The most of a transcript one read returns: its last 32 KiB.
const TRANSCRIPT_MAX: usize = 32 * 1024;
/// Session log rows one read returns, newest last.
const LOG_ROWS: usize = 200;

/// What a message would do that a client may not do on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Effect {
    None,
    /// Ends the session (or opens the outcome prompt on the way to ending it).
    Quits,
    /// Opens a confirmation, or is the destructive step one stands for.
    Confirms,
    /// Writes files: a workflow, a handoff, the ledger.
    Writes,
    /// Opens a screen that takes no key but the one that closes it (help, cost, a status card),
    /// or closes one: refused only while the user is in the middle of something.
    Screen,
    /// Opens a screen or menu with live letter keys, or moves the focus to a pane that has them.
    /// The user's next keystroke would answer it, so it is refused from a client at all.
    Keys,
}

/// What `m` would do. The matches name every variant on purpose: a new command or intent does
/// not compile until somebody decides what it means for a client.
fn effect(m: &Msg) -> Effect {
    match m {
        Msg::Command(c) => match c {
            Command::Quit { .. } => Effect::Quits,
            Command::OpenScreen(screen) => match screen {
                Screen::ConfirmLogout(_) | Screen::ConfirmClear | Screen::ConfirmCancel(_) => {
                    Effect::Confirms
                }
                Screen::RunStatus(_)
                | Screen::AdvisorStatus
                | Screen::RouterStatus
                | Screen::Cost => Effect::Screen,
                Screen::Agents
                | Screen::Accounts
                | Screen::Models(_)
                | Screen::Providers
                | Screen::Workflows
                | Screen::WorkflowShow => Effect::Keys,
            },
            Command::Clear => Effect::Confirms,
            Command::WorkflowFile(WorkflowFile::Save { .. })
            | Command::LedgerUpdate { .. }
            | Command::CreateHandoff { .. } => Effect::Writes,
            Command::WorkflowAuthor { write, .. } => {
                if *write {
                    Effect::Writes
                } else {
                    Effect::None
                }
            }
            // The loop answers these with a screen.
            Command::WorkflowFile(WorkflowFile::List | WorkflowFile::Show(_)) => Effect::Keys,
            Command::CloseModal | Command::OpenHelp => Effect::Screen,
            Command::OpenTranscript(_)
            | Command::OpenPalette
            | Command::Permission { .. }
            | Command::Focus(_) => Effect::Keys,
            Command::Delegate { .. }
            | Command::Background { .. }
            | Command::Fork { .. }
            | Command::Cancel { .. }
            | Command::Resume { .. }
            | Command::SendMessage { .. }
            | Command::Retry { .. }
            | Command::Scroll(..)
            | Command::Select(..)
            | Command::PaletteText(_)
            | Command::Workflow { .. }
            | Command::Compact { .. }
            | Command::LogFilter(_)
            | Command::SetModel { .. } => Effect::None,
        },
        Msg::Ui(intent) => match intent {
            UiIntent::CloseMenu => Effect::Screen,
            UiIntent::Open(_)
            | UiIntent::ShowPermission
            | UiIntent::OpenMenu { .. }
            | UiIntent::Transcript(_)
            | UiIntent::Focus(_)
            | UiIntent::FocusNext
            | UiIntent::FocusPrev => Effect::Keys,
            UiIntent::Select(..)
            | UiIntent::Scroll { .. }
            | UiIntent::ToggleRail
            | UiIntent::SetRailWidth(_)
            | UiIntent::SetLogRows(_)
            | UiIntent::ToggleFollow
            | UiIntent::MenuSelect(_)
            | UiIntent::Mem(_)
            | UiIntent::Loaded(_)
            | UiIntent::Pick(_) => Effect::None,
        },
        Msg::Via { msg, .. } => effect(msg),
        _ => Effect::None,
    }
}

/// What the user is in the middle of, if anything: a screen or a menu open, a field being edited,
/// the log filter or the prompt line being typed. A client may not open a screen under that.
pub fn busy(cx: &Context, ui: &Ui) -> Option<&'static str> {
    if cx.chrome.modal.is_some() {
        Some("the user has a screen open")
    } else if cx.chrome.menu_open {
        Some("the user has a menu open")
    } else if ui.entry.is_some() {
        Some("the user is editing a field")
    } else if ui.filter_edit {
        Some("the user is typing the log filter")
    } else if !ui.prompt.is_empty() {
        Some("the user is typing on the prompt line")
    } else {
        None
    }
}

/// The messages of a line a client sent, unless one of them would do what only the keyboard may.
fn checked(line: &str, msgs: Vec<Msg>, busy: Option<&str>) -> Result<Vec<Msg>, String> {
    let word = line.split_whitespace().next().unwrap_or(line);
    let effects: Vec<Effect> = msgs.iter().map(effect).collect();
    if effects.contains(&Effect::Quits) {
        return Err(format!(
            "{word} would end the session, and only the keyboard can do that"
        ));
    }
    if effects.contains(&Effect::Confirms) {
        return Err(format!(
            "{word} asks for a confirmation, and only the keyboard can give one"
        ));
    }
    if effects.contains(&Effect::Writes) {
        return Err(format!(
            "{word} writes files, which only the keyboard can do from a session"
        ));
    }
    if effects.contains(&Effect::Keys) {
        return Err(format!(
            "{word} would open a screen or menu the user's next key answers, so only the keyboard can do that"
        ));
    }
    if let (true, Some(busy)) = (effects.contains(&Effect::Screen), busy) {
        return Err(format!(
            "{word} would open or change a screen while {busy}, so only the keyboard can do that now"
        ));
    }
    Ok(msgs)
}

fn sent(s: Sent) -> Result<Vec<Msg>, String> {
    match s {
        Sent::Msgs(m) => Ok(m),
        Sent::Login(..) => {
            Err("/login opens a masked key entry, which is for the keyboard only".into())
        }
        Sent::Bad(why) => Err(why),
    }
}

/// One word naming a run the way `@target` does: an id or a unique prefix, an agent, `lead`.
fn run_word(run: &str) -> Result<&str, String> {
    let word = run.trim().trim_start_matches('@');
    if word.is_empty() || word.contains(char::is_whitespace) {
        return Err("run is one word: an id, a prefix of one, an agent name or lead".into());
    }
    Ok(word)
}

/// The run `run` names, or the lead without one.
fn resolve(run: Option<&str>, cx: &Context) -> Result<RunId, String> {
    let env = cx.env(&cx.workflows);
    let target = match run {
        None => ModelTarget::Lead,
        Some(r) => commands::parse_target(&format!("@{}", run_word(r)?), &env)?.0,
    };
    match target {
        ModelTarget::Run(r) => Ok(r),
        ModelTarget::Lead => env
            .lead
            .ok_or_else(|| "the session has no lead yet".to_owned()),
        _ => Err("the advisor and the router are seats, not runs".into()),
    }
}

/// The messages `ask` stands for: what Enter on the prompt line gives for the same text, or what
/// the permission modal's key gives. A read asks for none. `busy` is what the user is in the
/// middle of (`busy()`). `Err` is the sentence for the client.
pub fn msgs(
    ask: &Ask,
    cx: &Context,
    permission: bool,
    busy: Option<&str>,
) -> Result<Vec<Msg>, String> {
    let env = cx.env(&cx.workflows);
    match ask {
        Ask::Status | Ask::Transcript { .. } => Ok(Vec::new()),
        Ask::Send { text } => {
            let text = text.trim();
            if text.is_empty() {
                return Err("the prompt is empty".into());
            }
            if text.starts_with('/') {
                return Err("a slash line is a command: send it with session_command".into());
            }
            checked(text, sent(commands::send(text, &env))?, busy)
        }
        Ask::Command { line } => {
            let line = line.trim();
            if !line.starts_with('/') {
                return Err("a command is a slash line such as /model haiku; a prompt goes through session_send".into());
            }
            checked(line, sent(commands::send(line, &env))?, busy)
        }
        // As typed; on the lead that is a confirmation, so the keyboard's.
        Ask::Cancel { run } => {
            let line = format!("/cancel @{}", run_word(run)?);
            checked(&line, sent(commands::send(&line, &env))?, busy)
        }
        Ask::Permission { id, decision } => {
            if !permission {
                return Err("a permission prompt is answered at the keyboard: this session was not started with --control-permission".into());
            }
            let id: PermissionId = id
                .parse()
                .map_err(|_| "id is not a permission id".to_owned())?;
            // The keyboard answers the prompt the modal shows, and so does a client.
            if cx.permission != Some(id) {
                return Err(format!("no open permission prompt {id} on screen"));
            }
            let decision = match decision.as_str() {
                "allow" => Decision::Allow,
                "allow_session" => Decision::AllowSession,
                "deny" => Decision::Deny,
                other => {
                    return Err(format!(
                        "decision {:?}: want allow, allow_session or deny",
                        client_name(other)
                    ))
                }
            };
            Ok(vec![Msg::Command(aibus_core::Command::Permission {
                id,
                decision,
            })])
        }
    }
}

/// One run as `session_status` shows it.
fn run_row<G: Graph>(g: &G, run: RunId) -> Json {
    let meta = g.read_key(&run_key(run, "meta"));
    let field = |k: &str| {
        meta.and_then(|m| get(m, k))
            .and_then(as_str)
            .unwrap_or("")
            .to_owned()
    };
    let status = g
        .read_key(&run_key(run, "status"))
        .and_then(as_str)
        .unwrap_or("");
    let mut row = json!({"run": run.to_string(), "agent": field("agent"), "model": field("model"), "status": status});
    if !field("via").is_empty() {
        row["via"] = json!(field("via"));
        row["client"] = json!(field("client"));
    }
    row
}

fn text_of(app: &App<impl Graph>, v: Option<&Value>) -> String {
    match v {
        Some(Value::Handle(h)) => app.store.text(h).map(|t| t.to_string()).unwrap_or_default(),
        Some(v) => as_str(v).unwrap_or("").to_owned(),
        None => String::new(),
    }
}

/// What the screen holds: the lead, the cards, the open permission prompts and the client.
pub fn status<G: Graph>(app: &App<G>, client: Option<&str>) -> Json {
    let cx = Context::read(&app.graph);
    let cards: Vec<Json> = cx
        .runs
        .iter()
        .filter(|(r, _)| Some(*r) != cx.lead_run)
        .map(|(r, _)| run_row(&app.graph, *r))
        .collect();
    let permissions: Vec<Json> = app
        .graph
        .read_key(PERMISSIONS_OPEN)
        .map_or(&[][..], as_list)
        .iter()
        .map(|p| {
            let f = |k: &str| get(p, k).and_then(as_str).unwrap_or("").to_owned();
            json!({"id": f("id"), "run": f("run"), "agent": f("agent"), "tool": f("tool"), "summary": text_of(app, get(p, "summary"))})
        })
        .collect();
    json!({
        "lead": cx.lead_run.map(|r| run_row(&app.graph, r)),
        "cards": cards,
        "permissions": permissions,
        "client": client,
    })
}

/// A run's transcript as text (its last 32 KiB) and the session log's last rows.
pub fn transcript<G: Graph>(app: &App<G>, run: Option<&str>) -> Result<Json, String> {
    let cx = Context::read(&app.graph);
    let id = resolve(run, &cx)?;
    let mut text = String::new();
    if let Some(Value::Handle(h)) = app.graph.read_key(&run_key(id, "blocks")) {
        for b in app.store.blocks(h.id) {
            text.push_str(&b.text);
            text.push('\n');
        }
        text.push_str(&app.store.tail(h.id));
    }
    let total = text.len();
    if total > TRANSCRIPT_MAX {
        let mut start = total - TRANSCRIPT_MAX;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text = text[start..].to_owned();
    }
    let log: Vec<Json> = app
        .store
        .log_rows(LOG_ROWS)
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or(Json::Null))
        .collect();
    Ok(
        json!({"run": id.to_string(), "transcript": text, "bytes": total, "cut": total > TRANSCRIPT_MAX, "log": log}),
    )
}

/// One event from the control socket: a connection said on screen, or a call answered on its
/// `reply`. A call's messages go through `step`, the one the keyboard's take, each in `Msg::Via`.
pub fn handle<G: Graph>(
    app: &mut App<G>,
    ui: &mut Ui,
    event: Remote,
    permission: bool,
    step: &mut StepFn<G>,
) -> Result<(), AibusError> {
    match event {
        Remote::Connected { client } => {
            let name = client_name(&client);
            ui.remote = Some(name.clone());
            step(
                app,
                Msg::Client {
                    name,
                    event: ClientEvent::Connected,
                },
            )
        }
        Remote::Disconnected { client } => {
            ui.remote = None;
            step(
                app,
                Msg::Client {
                    name: client_name(&client),
                    event: ClientEvent::Disconnected,
                },
            )
        }
        Remote::Call { client, ask, reply } => {
            let name = client_name(&client);
            // The socket gave up waiting (the loop was busy, an editor open): whatever it asked is
            // dropped rather than done late, so a retry cannot do it twice.
            if reply.is_closed() {
                let why = "a call timed out before the session could take it, so nothing was done"
                    .to_owned();
                return step(
                    app,
                    Msg::Client {
                        name,
                        event: ClientEvent::Refused(why),
                    },
                );
            }
            let cx = Context::read(&app.graph);
            let busy = busy(&cx, ui);
            let answer = match &ask {
                Ask::Status => Ok(status(app, ui.remote.as_deref())),
                Ask::Transcript { run } => transcript(app, run.as_deref()),
                _ => match msgs(&ask, &cx, permission, busy) {
                    Ok(msgs) => {
                        let n = msgs.len();
                        for m in msgs {
                            step(
                                app,
                                Msg::Via {
                                    client: name.clone(),
                                    msg: Box::new(m),
                                },
                            )?;
                        }
                        Ok(json!({"accepted": true, "messages": n}))
                    }
                    Err(why) => {
                        // The refusal is on screen too; the line itself is not repeated.
                        step(
                            app,
                            Msg::Client {
                                name: name.clone(),
                                event: ClientEvent::Refused(why.clone()),
                            },
                        )?;
                        Err(why)
                    }
                },
            };
            // A client that has gone away no longer waits for the answer.
            let _ = reply.send(answer);
            Ok(())
        }
    }
}
