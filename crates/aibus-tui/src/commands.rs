//! The command registry (05 section 4): one table that the slash line, the palette, the card menu,
//! the completion popup and the help text all read. A command is declared once, here, with the
//! surfaces it shows up on and what it does; nothing else in the crate names a command.
//!
//! `run` writes into the same `Out` the key handling fills, so a slash line, a palette row and a
//! menu item are three ways into one function and cannot drift apart.

use aibus_core::workflow::{valid_name, SaveError};
use aibus_core::{AccountName, AgentName, Command, Isolation, ModelTarget, Msg, Pane, RunId, RunStatus, Screen, UiIntent, WorkflowFile};

use crate::input::{Clip, Out};
use crate::prompt::Sent;

/// What a command takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// No target.
    Global,
    /// Takes an optional `@target` first.
    Run,
    /// Opens or acts on a screen.
    Screen,
}

/// What has to be true right now for a command to be offered and accepted (a bit set).
pub const LEAD: u8 = 1;
pub const RUNS: u8 = 2;
pub const ACCOUNTS: u8 = 4;
/// A workflow with the command's own name exists (the `/fix` family).
pub const WORKFLOWS: u8 = 8;
pub const MCP: u8 = 16;
/// The resolved run is `Failed` or `Partial`.
pub const RETRYABLE: u8 = 32;
/// The resolved run has finished.
pub const FINISHED: u8 = 64;

/// Where a command is listed: a 1-based position on that surface, 0 for not listed. The three
/// numbers vary together per command, which is why they are one struct.
#[derive(Clone, Copy, Debug)]
pub struct Surfaces {
    pub slash: u8,
    pub palette: u8,
    pub menu: u8,
}

/// How a command was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Slash,
    Palette,
    Menu,
}

/// The session as far as commands care, read from the graph by `Context::env`.
#[derive(Clone, Copy, Debug)]
pub struct Env<'a> {
    pub agents: &'a [String],
    pub workflows: &'a [String],
    pub lead: Option<RunId>,
    /// The selected card, when the cards pane has focus.
    pub focused: Option<RunId>,
    /// Every run, oldest first, with its agent.
    pub runs: &'a [(RunId, String)],
    /// Where each run stands.
    pub states: &'a [(RunId, RunStatus)],
    /// The graph does not tell the dashboard about these two yet, so `Context::env` says yes.
    pub accounts: bool,
    pub mcp: bool,
    /// The accounts there are, for `/logout`.
    pub account_names: &'a [String],
}

impl<'a> Env<'a> {
    /// No session at all: only the agents and workflows given.
    #[cfg(test)]
    pub fn bare(agents: &'a [String], workflows: &'a [String]) -> Self {
        Self { agents, workflows, lead: None, focused: None, runs: &[], states: &[], accounts: true, mcp: true, account_names: &[] }
    }
}

/// One call of a command.
pub struct Call<'a> {
    pub name: &'static str,
    pub via: Via,
    /// The slash line, trimmed.
    pub line: &'a str,
    /// What follows the command word (and the `@target`, for a `Run` command).
    pub args: &'a str,
    /// Set for a `Run` command, and by the menu for the run it was opened on.
    pub target: Option<ModelTarget>,
    pub env: &'a Env<'a>,
}

pub struct Spec {
    pub name: &'static str,
    /// The argument hint the popup shows.
    pub args: &'static str,
    /// One line for `/help`.
    pub help: &'static str,
    /// The palette row's or the menu item's label.
    pub label: &'static str,
    /// The palette row's hint: the key or the slash form.
    pub hint: &'static str,
    pub scope: Scope,
    pub needs: u8,
    pub surfaces: Surfaces,
    pub run: fn(&Call, &mut Out) -> Result<(), String>,
}

const fn at(slash: u8, palette: u8, menu: u8) -> Surfaces {
    Surfaces { slash, palette, menu }
}

fn need(c: &Call, what: &str) -> Result<(), String> {
    Err(format!("/{} needs {what}", c.name))
}

fn menu_run(c: &Call) -> Option<RunId> {
    match c.target {
        Some(ModelTarget::Run(r)) => Some(r),
        _ => None,
    }
}

/// The run a `Run` command acts on. The seats have no run, so a command that needs one says so.
fn run_of(c: &Call) -> Result<RunId, String> {
    match c.target {
        Some(ModelTarget::Run(run)) => Ok(run),
        Some(ModelTarget::Lead) => c.env.lead.ok_or_else(|| "no lead is running".to_owned()),
        Some(ModelTarget::Advisor | ModelTarget::Router) => Err(format!("/{} takes a run, not a seat", c.name)),
        None => Err(format!("/{} needs a run", c.name)),
    }
}

fn prompt_line(c: &Call, out: &mut Out) -> Result<(), String> {
    out.msgs.push(Msg::SubmitPrompt { text: c.line.to_owned() });
    Ok(())
}

fn command(out: &mut Out, c: Command) -> Result<(), String> {
    out.msgs.push(Msg::Command(c));
    Ok(())
}

/// Every command, once. The positions in `surfaces` fix the order on each surface.
pub const REGISTRY: &[Spec] = &[
    Spec {
        name: "fork",
        args: "<task>",
        help: "fork the lead with a new task",
        label: "Fork",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(1, 0, 3),
        run: |c, out| match (c.via, menu_run(c)) {
            (Via::Menu, Some(run)) => command(
                out,
                Command::Fork { prompt: format!("continue the work of run {run}"), parent: Some(run), run: None },
            ),
            _ => prompt_line(c, out),
        },
    },
    Spec {
        name: "delegate",
        args: "<agent> <task>",
        help: "start a background run of an agent",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(2, 0, 0),
        run: |c, out| match c.args.split_once(char::is_whitespace) {
            Some((agent, task)) if !task.trim().is_empty() => match agent.parse::<AgentName>() {
                Ok(agent) if c.env.agents.iter().any(|a| a == agent.as_str()) => command(
                    out,
                    Command::Delegate {
                        agent,
                        prompt: task.trim().to_owned(),
                        background: true,
                        isolation: Isolation::None,
                        parent: None,
                        run: None,
                        model: None,
                        account: None,
                    },
                ),
                _ => Err(format!("/delegate: no agent named {agent}")),
            },
            _ => need(c, "an agent and a task"),
        },
    },
    Spec {
        name: "compact",
        args: "",
        help: "compact the lead's context",
        label: "compact the lead",
        hint: "/compact",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(3, 9, 0),
        run: |c, out| match c.via {
            Via::Palette => {
                out.msgs.push(Msg::Command(Command::CloseModal));
                if let Some(run) = c.env.lead {
                    out.msgs.push(Msg::Command(Command::Compact { run }));
                }
                Ok(())
            }
            _ => prompt_line(c, out),
        },
    },
    Spec {
        name: "clear",
        args: "",
        help: "clear the session log (asks first)",
        label: "clear the session log",
        hint: "/clear",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(4, 7, 0),
        run: |_, out| command(out, Command::OpenScreen(Screen::ConfirmClear)),
    },
    Spec {
        name: "handoff",
        args: "",
        help: "write a handoff",
        label: "write a handoff",
        hint: "/handoff",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(5, 6, 0),
        run: |c, out| {
            if c.via == Via::Palette {
                out.msgs.push(Msg::Command(Command::CloseModal));
            }
            command(out, Command::CreateHandoff { outcome: None })
        },
    },
    Spec {
        name: "ledger",
        args: "<section> <text>",
        help: "add text to a ledger section",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(6, 0, 0),
        run: |c, out| match c.args.split_once(char::is_whitespace) {
            Some((section, text)) if !text.trim().is_empty() => {
                command(out, Command::LedgerUpdate { section: section.to_owned(), text: text.trim().to_owned() })
            }
            _ => need(c, "a section and text"),
        },
    },
    Spec {
        name: "model",
        args: "[@target] [ref]",
        help: "pick a model from the connected accounts, or set one by name",
        label: "",
        hint: "",
        scope: Scope::Run,
        needs: 0,
        surfaces: at(7, 0, 0),
        run: |c, out| {
            let target = c.target.unwrap_or(ModelTarget::Lead);
            if c.args.is_empty() {
                return command(out, Command::OpenScreen(Screen::Models(target)));
            }
            command(out, Command::SetModel { target, model: c.args.to_owned() })
        },
    },
    Spec {
        name: "advisor",
        args: "<ref|off>",
        help: "change the advisor's model, or turn it off",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(8, 0, 0),
        run: |c, out| {
            if c.args.is_empty() {
                return need(c, "a model or off");
            }
            command(out, Command::SetModel { target: ModelTarget::Advisor, model: c.args.to_owned() })
        },
    },
    Spec {
        name: "router",
        args: "<ref|off>",
        help: "change the router's model, or turn it off",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(9, 0, 0),
        run: |c, out| {
            if c.args.is_empty() {
                return need(c, "a model or off");
            }
            command(out, Command::SetModel { target: ModelTarget::Router, model: c.args.to_owned() })
        },
    },
    Spec {
        name: "workflow",
        args: "new <goal> | list | show <name> | save <name> | run <name> <input>",
        help: "draft (new), list, show, save and run workflows (/workflow <name> <input> runs one too)",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(10, 0, 0),
        run: workflow,
    },
    Spec {
        name: "agents",
        args: "",
        help: "the agents screen",
        label: "agents screen",
        hint: "A",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(11, 1, 0),
        run: |_, out| command(out, Command::OpenScreen(Screen::Agents)),
    },
    Spec {
        name: "accounts",
        args: "",
        help: "the accounts screen",
        label: "accounts screen",
        hint: "K",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(12, 2, 0),
        run: |_, out| command(out, Command::OpenScreen(Screen::Accounts)),
    },
    Spec {
        name: "login",
        args: "[provider]",
        help: "add an account: pick a provider, name it, type the key",
        label: "",
        hint: "",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(26, 0, 0),
        run: |c, out| match c.args.split_whitespace().collect::<Vec<_>>()[..] {
            [] => command(out, Command::OpenScreen(Screen::Providers)),
            // Anything after the provider could be a key, and a key typed on a line is in the
            // scrollback and the history. It is refused, and the line says where the key goes.
            [provider, _, ..] => Err(format!("/login {provider} [redacted]: type the key in the masked field after the name, never on the line")),
            [provider] if aibus_provider::presets::preset(provider).is_some() => {
                out.login = Some(provider.to_owned());
                Ok(())
            }
            [provider] => Err(format!("/login: no provider named {provider} (/login lists them)")),
        },
    },
    Spec {
        name: "logout",
        args: "<account>",
        help: "remove an account and its key (asks first)",
        label: "",
        hint: "",
        scope: Scope::Screen,
        needs: ACCOUNTS,
        surfaces: at(27, 0, 0),
        run: |c, out| match c.env.account_names.iter().find(|a| a.as_str() == c.args) {
            Some(account) => command(out, Command::OpenScreen(Screen::ConfirmLogout(AccountName(account.as_str().into())))),
            None if c.args.is_empty() => need(c, "an account"),
            None => Err(format!("/logout: no account named {}", c.args)),
        },
    },
    Spec {
        name: "help",
        args: "",
        help: "the key map and the commands",
        label: "help: the key map",
        hint: "?",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(13, 3, 0),
        run: |_, out| command(out, Command::OpenHelp),
    },
    Spec {
        name: "fix",
        args: "<input>",
        help: "shorthand for /workflow fix",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: WORKFLOWS,
        surfaces: at(14, 0, 0),
        run: shorthand,
    },
    Spec {
        name: "build",
        args: "<input>",
        help: "shorthand for /workflow build",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: WORKFLOWS,
        surfaces: at(15, 0, 0),
        run: shorthand,
    },
    Spec {
        name: "review",
        args: "<input>",
        help: "shorthand for /workflow review",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: WORKFLOWS,
        surfaces: at(16, 0, 0),
        run: shorthand,
    },
    Spec {
        name: "explore",
        args: "<input>",
        help: "shorthand for /workflow explore",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: WORKFLOWS,
        surfaces: at(17, 0, 0),
        run: shorthand,
    },
    Spec {
        name: "lead",
        args: "",
        help: "open the lead's transcript",
        label: "lead transcript",
        hint: "t",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(0, 4, 0),
        run: |c, out| match c.env.lead {
            Some(run) => command(out, Command::OpenTranscript(run)),
            None => command(out, Command::CloseModal),
        },
    },
    Spec {
        name: "permission",
        args: "",
        help: "jump to the oldest permission prompt",
        label: "oldest permission prompt",
        hint: "p",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(0, 5, 0),
        run: |_, out| {
            out.msgs.push(Msg::Ui(UiIntent::ShowPermission));
            Ok(())
        },
    },
    Spec {
        name: "rail",
        args: "",
        help: "focus the advisor rail",
        label: "advisor rail",
        hint: "a",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(0, 8, 0),
        run: |_, out| {
            out.msgs.extend([
                Msg::Command(Command::CloseModal),
                Msg::Ui(UiIntent::Focus(Pane::Advisor)),
                Msg::Ui(UiIntent::ToggleRail),
            ]);
            Ok(())
        },
    },
    Spec {
        name: "quit",
        args: "",
        help: "quit",
        label: "quit",
        hint: "q",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(0, 10, 0),
        run: |_, out| {
            out.msgs.extend([Msg::Command(Command::CloseModal), Msg::Command(Command::Quit { force: false })]);
            Ok(())
        },
    },
    Spec {
        name: "open",
        args: "",
        help: "open the run's transcript",
        label: "Open transcript",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(0, 0, 1),
        run: |c, out| menu_run(c).map_or(Ok(()), |run| command(out, Command::OpenTranscript(run))),
    },
    Spec {
        name: "cancel",
        args: "",
        help: "cancel the run (asks first on the lead)",
        label: "Cancel",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(21, 0, 2),
        run: |c, out| {
            let run = run_of(c)?;
            // Typed against the lead, it asks first; on a worker, or from the menu, it does not.
            if c.via == Via::Slash && c.env.lead == Some(run) {
                return command(out, Command::OpenScreen(Screen::ConfirmCancel(run)));
            }
            command(out, Command::Cancel { run })
        },
    },
    Spec {
        name: "resume",
        args: "<text>",
        help: "resume a finished run with a message",
        label: "Resume",
        hint: "",
        scope: Scope::Run,
        needs: RUNS | FINISHED,
        surfaces: at(24, 0, 4),
        run: |c, out| {
            let run = run_of(c)?;
            let message = if c.args.is_empty() { "continue".to_owned() } else { c.args.to_owned() };
            command(out, Command::Resume { run, message, parent: None })
        },
    },
    Spec {
        name: "copy_id",
        args: "",
        help: "copy the run's id",
        label: "Copy id",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(0, 0, 5),
        run: |c, out| {
            out.copy = menu_run(c).map(|run| Clip::Text(run.to_string()));
            Ok(())
        },
    },
    Spec {
        name: "copy_result",
        args: "",
        help: "copy the run's result",
        label: "Copy result",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(0, 0, 6),
        run: |c, out| {
            out.copy = menu_run(c).map(Clip::Result);
            Ok(())
        },
    },
    Spec {
        name: "status",
        args: "[@target]",
        help: "a run's numbers, or a seat's (@advisor, @router)",
        label: "Status",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(18, 0, 7),
        run: |c, out| open_status(c, c.target, out),
    },
    Spec {
        name: "cost",
        args: "[@target]",
        help: "the session's cost per run, or one target's status",
        label: "Cost",
        hint: "",
        // Not `Run`: with no `@target` it is the session table, not the focused card.
        scope: Scope::Global,
        needs: RUNS,
        surfaces: at(19, 0, 8),
        run: |c, out| {
            if c.via == Via::Slash && c.args.starts_with('@') && !c.args.starts_with("@agent-") {
                let (target, _) = parse_target(c.args, c.env)?;
                return open_status(c, Some(target), out);
            }
            command(out, Command::OpenScreen(Screen::Cost))
        },
    },
    Spec {
        name: "log",
        args: "[@target]",
        help: "open the run's transcript",
        label: "",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(20, 0, 0),
        run: |c, out| command(out, Command::OpenTranscript(run_of(c)?)),
    },
    Spec {
        name: "retry",
        args: "[@target]",
        help: "retry a failed or partial run",
        label: "",
        hint: "",
        scope: Scope::Run,
        needs: RUNS | RETRYABLE,
        surfaces: at(22, 0, 0),
        run: |c, out| command(out, Command::Retry { run: run_of(c)? }),
    },
    Spec {
        name: "message",
        args: "@target <text>",
        help: "send a message to a run",
        label: "",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(23, 0, 0),
        run: |c, out| {
            if c.args.is_empty() {
                return need(c, "a message");
            }
            command(out, Command::SendMessage { run: run_of(c)?, text: c.args.to_owned() })
        },
    },
    Spec {
        name: "background",
        args: "[@target]",
        help: "move the run's wait to the background",
        label: "",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(25, 0, 0),
        run: |c, out| command(out, Command::Background { run: run_of(c)? }),
    },
];

/// `/status` and `/cost @t`: the modal of a run or of a seat.
fn open_status(c: &Call, target: Option<ModelTarget>, out: &mut Out) -> Result<(), String> {
    let screen = match target {
        Some(ModelTarget::Advisor) => Screen::AdvisorStatus,
        Some(ModelTarget::Router) => Screen::RouterStatus,
        _ => Screen::RunStatus(run_of(&Call { target, ..*c })?),
    };
    command(out, Command::OpenScreen(screen))
}

/// `/workflow` and its subcommands. A first word that is not one of them is the name of a
/// workflow to run, so `/workflow review <input>` keeps working (a workflow called `list`,
/// `show`, `save` or `run` is reached as `/workflow run list <input>`).
fn workflow(c: &Call, out: &mut Out) -> Result<(), String> {
    let (sub, rest) = c.args.split_once(char::is_whitespace).map_or((c.args, ""), |(w, r)| (w, r.trim()));
    let run = |args: &str, out: &mut Out| match args.split_once(char::is_whitespace) {
        Some((name, input)) => command(out, Command::Workflow { name: name.to_owned(), input: input.trim().to_owned() }),
        None if !args.is_empty() => command(out, Command::Workflow { name: args.to_owned(), input: String::new() }),
        None => need(c, "a workflow name"),
    };
    match sub {
        "new" => {
            if let Some(why) = c.env.lacking(c.name, LEAD | MCP, None) {
                return Err(format!("/workflow new is not available: {why}"));
            }
            if rest.is_empty() {
                return need(c, "a goal, in words");
            }
            command(out, Command::WorkflowAuthor { goal: rest.to_owned() })
        }
        "list" => command(out, Command::WorkflowFile(WorkflowFile::List)),
        "show" if rest.is_empty() => need(c, "a workflow name"),
        "show" => command(out, Command::WorkflowFile(WorkflowFile::Show(rest.to_owned()))),
        "save" => {
            let (mut name, mut user, mut force) = (None, false, false);
            for word in rest.split_whitespace() {
                match word {
                    "--user" => user = true,
                    "--force" => force = true,
                    w if w.starts_with("--") => return Err(format!("/workflow save: no flag {w} (--user, --force)")),
                    w if name.is_none() => name = Some(w),
                    _ => return Err("/workflow save takes one name".into()),
                }
            }
            let name = name.ok_or_else(|| format!("/{} save needs a name", c.name))?;
            // Before any file is looked at: the name is a name, never a path.
            if !valid_name(name) {
                return Err(SaveError::Name(name.to_owned()).to_string());
            }
            command(out, Command::WorkflowFile(WorkflowFile::Save { name: name.to_owned(), user, force }))
        }
        "run" => run(rest, out),
        _ => run(c.args, out),
    }
}

/// `/fix <input>` and the other three are `/workflow <name> <input>`.
fn shorthand(c: &Call, out: &mut Out) -> Result<(), String> {
    command(out, Command::Workflow { name: c.name.to_owned(), input: c.args.to_owned() })
}

fn find(name: &str) -> Option<&'static Spec> {
    REGISTRY.iter().find(|s| s.name == name)
}

/// The specs on one surface, in that surface's order.
fn listed(pos: fn(&Surfaces) -> u8) -> Vec<&'static Spec> {
    let mut v: Vec<&Spec> = REGISTRY.iter().filter(|s| pos(&s.surfaces) > 0).collect();
    v.sort_by_key(|s| pos(&s.surfaces));
    v
}

impl Env<'_> {
    /// Why `spec` is not available right now, or `None` when it is. `run` is the run it would act
    /// on, when that is known yet: the state needs are only judged against one.
    fn unmet(&self, spec: &Spec, run: Option<RunId>) -> Option<String> {
        self.lacking(spec.name, spec.needs, run)
    }

    /// Why `needs` (a bit set) is not met right now for the command `name`, or `None` when it
    /// is. A subcommand that needs more than its row (`/workflow new`) asks this directly.
    fn lacking(&self, name: &str, needs: u8, run: Option<RunId>) -> Option<String> {
        let missing = |bit: u8| needs & bit != 0;
        if missing(LEAD) && self.lead.is_none() {
            return Some("no lead is running".into());
        }
        if missing(RUNS) && self.runs.is_empty() {
            return Some("no run yet".into());
        }
        if missing(WORKFLOWS) && !self.workflows.iter().any(|w| w == name) {
            return Some(format!("no workflow named {name}"));
        }
        let state = run.and_then(|r| self.states.iter().find(|(id, _)| *id == r)).map(|(_, st)| *st);
        if let Some(st) = state {
            let retryable = matches!(st, RunStatus::Failed | RunStatus::Partial);
            if (missing(RETRYABLE) && !retryable) || (missing(FINISHED) && !st.is_terminal()) {
                return Some(format!("the run is {}", st.as_value()));
            }
        }
        if missing(ACCOUNTS) && !self.accounts {
            return Some("no account is set up".into());
        }
        if missing(MCP) && !self.mcp {
            return Some("no MCP server is configured".into());
        }
        None
    }
}

/// The slash commands the popup offers right now: `(/name, args)` in order.
pub fn slash_offered(env: &Env) -> Vec<(String, &'static str)> {
    // What a bare `/retry` would act on: the focused card, else the lead.
    let fallback = env.focused.or(env.lead);
    listed(|s| s.slash)
        .into_iter()
        .filter(|s| env.unmet(s, if s.scope == Scope::Run { fallback } else { None }).is_none())
        .map(|s| (format!("/{}", s.name), s.args))
        .collect()
}

/// The palette's command rows: `(id, label, hint)`, the id being the command's name.
pub fn palette_rows() -> Vec<(&'static str, &'static str, &'static str)> {
    listed(|s| s.palette).into_iter().map(|s| (s.name, s.label, s.hint)).collect()
}

/// The card menu's items: `(id, label)`.
pub fn menu_items() -> Vec<(&'static str, &'static str)> {
    listed(|s| s.menu).into_iter().map(|s| (s.name, s.label)).collect()
}

/// The help screen's command section: `(/name args, one line)`, for what is on the slash line.
pub fn help_lines() -> Vec<(String, &'static str)> {
    listed(|s| s.slash)
        .into_iter()
        .map(|s| (if s.args.is_empty() { format!("/{}", s.name) } else { format!("/{} {}", s.name, s.args) }, s.help))
        .collect()
}

/// Runs a command that was picked on the palette or the card menu. `run` is the card's run for
/// the menu. Unknown names do nothing.
pub fn pick(name: &str, via: Via, run: Option<RunId>, env: &Env, out: &mut Out) {
    let Some(spec) = find(name) else { return };
    let call = Call { name: spec.name, via, line: "", args: "", target: run.map(ModelTarget::Run), env };
    // A pick has no way to show a notice, and none of the picked commands has a bad shape.
    let _ = (spec.run)(&call, out);
}

/// A `/login` line with something after the provider: a key typed where it would stay in the
/// history and the scrollback.
pub fn carries_key(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("/login") && words.nth(1).is_some()
}

/// What a line of text means: a slash command becomes the messages it stands for, anything else
/// is a prompt for the lead (which `reduce` hands on, or turns into a delegate for `@agent-`).
pub fn send(line: &str, env: &Env) -> Sent {
    let line = line.trim();
    let Some(rest) = line.strip_prefix('/') else {
        return Sent::Msgs(vec![Msg::SubmitPrompt { text: line.to_owned() }]);
    };
    let (word, args) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(w, a)| (w, a.trim()));
    let Some(spec) = REGISTRY.iter().find(|s| s.surfaces.slash > 0 && s.name == word) else {
        return Sent::Bad(format!("no command /{word} (try /help)"));
    };
    if let Some(why) = env.unmet(spec, None) {
        return Sent::Bad(format!("/{word} is not available: {why}"));
    }
    let (target, args) = if spec.scope == Scope::Run {
        match parse_target(args, env) {
            Ok((t, rest)) => (Some(t), rest),
            Err(why) => return Sent::Bad(why),
        }
    } else {
        (None, args)
    };
    // A command that only fits a run in some state is judged against the run it resolved to.
    let run = match target {
        Some(ModelTarget::Run(r)) => Some(r),
        Some(ModelTarget::Lead) => env.lead,
        _ => None,
    };
    if let Some(why) = env.unmet(spec, run) {
        return Sent::Bad(format!("/{word} is not available: {why}"));
    }
    let mut out = Out::default();
    match (spec.run)(&Call { name: spec.name, via: Via::Slash, line, args, target, env }, &mut out) {
        Ok(()) if out.login.is_some() => Sent::Login(out.login.take().unwrap_or_default(), out.msgs),
        Ok(()) => Sent::Msgs(out.msgs),
        Err(why) => Sent::Bad(why),
    }
}

/// The `@target` that starts `args`, and the rest. `@lead`, `@advisor` and `@router` are the
/// seats; otherwise the word is an agent (its newest run) or a unique prefix of a run id. No
/// target means the focused card, else the lead. `@agent-<name>` is a mention, not a target.
pub fn parse_target<'a>(args: &'a str, env: &Env) -> Result<(ModelTarget, &'a str), String> {
    let args = args.trim_start();
    let fallback = env.focused.map_or(ModelTarget::Lead, ModelTarget::Run);
    let Some(word) = args.split_whitespace().next().filter(|w| w.starts_with('@') && !w.starts_with("@agent-")) else {
        return Ok((fallback, args));
    };
    let rest = args[word.len()..].trim_start();
    let name = &word[1..];
    let target = match name {
        "lead" => ModelTarget::Lead,
        "advisor" => ModelTarget::Advisor,
        "router" => ModelTarget::Router,
        _ => match env.runs.iter().rev().find(|(_, agent)| agent == name) {
            Some((run, _)) => ModelTarget::Run(*run),
            None => {
                let up = name.to_uppercase();
                let hits: Vec<&RunId> = env.runs.iter().map(|(r, _)| r).filter(|r| r.to_string().starts_with(&up)).collect();
                match hits[..] {
                    [one] => ModelTarget::Run(*one),
                    [] => return Err(format!("no agent or run matches @{name}")),
                    _ => {
                        let list: Vec<String> = hits.iter().map(|r| r.to_string()).collect();
                        return Err(format!("@{name} matches {}", list.join(", ")));
                    }
                }
            }
        },
    };
    Ok((target, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command names on the spec's slash line (05 section 4), without the slash.
    fn spec_slash_names() -> Vec<String> {
        let spec = include_str!("../../../docs/spec/05-ui.md");
        let line = spec.lines().find(|l| l.starts_with("Slash commands in the prompt:")).expect("the slash line");
        let mut names: Vec<String> = line
            .split('`')
            .skip(1)
            .step_by(2)
            .filter_map(|t| t.strip_prefix('/'))
            .filter_map(|t| t.split_whitespace().next())
            .map(str::to_owned)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn login_refuses_anything_after_the_provider_and_never_echoes_it() {
        let env = Env::bare(&[], &[]);
        for line in ["/login anthropic sk-ant-api03-abcdef", "/login anthropic --key sk-1 sk-2"] {
            let Sent::Bad(why) = send(line, &env) else { panic!("{line} is refused") };
            assert!(why.contains("anthropic") && why.contains("[redacted]"), "{why}");
            assert!(!why.contains("sk-"), "{why}");
        }
        assert!(carries_key("/login anthropic sk-ant-api03-abcdef"));
        assert!(!carries_key("/login anthropic") && !carries_key("/login") && !carries_key("/model a b"));
    }

    #[test]
    fn logout_names_an_account_that_exists() {
        let names = vec!["work".to_owned()];
        let env = Env { account_names: &names, ..Env::bare(&[], &[]) };
        let m = |l: &str| match send(l, &env) {
            Sent::Msgs(m) => m.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join("|"),
            Sent::Login(..) => "login".to_owned(),
            Sent::Bad(why) => format!("BAD {why}"),
        };
        assert_eq!(m("/logout work"), "Command(OpenScreen(ConfirmLogout(AccountName(\"work\"))))");
        assert_eq!(m("/logout"), "BAD /logout needs an account");
        assert_eq!(m("/logout nobody"), "BAD /logout: no account named nobody");
    }

    #[test]
    fn registry_matches_spec_05_section_4() {
        let mut ours: Vec<String> = REGISTRY.iter().filter(|s| s.surfaces.slash > 0).map(|s| s.name.to_owned()).collect();
        ours.sort();
        assert_eq!(ours, spec_slash_names(), "a slash command is in the registry and in 05 section 4, or in neither");
    }

    #[test]
    fn every_registry_entry_has_help_and_a_surface() {
        for s in REGISTRY {
            assert!(!s.help.is_empty(), "{} has no help", s.name);
            let on = &s.surfaces;
            assert!(on.slash + on.palette + on.menu > 0, "{} is on no surface", s.name);
            if on.palette > 0 || on.menu > 0 {
                assert!(!s.label.is_empty(), "{} has no label", s.name);
            }
        }
        let names: std::collections::BTreeSet<_> = REGISTRY.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), REGISTRY.len(), "names are unique");
        for pos in [(|s: &Surfaces| s.slash) as fn(&Surfaces) -> u8, |s| s.palette, |s| s.menu] {
            let n = listed(pos).len();
            let got: Vec<u8> = listed(pos).iter().map(|s| pos(&s.surfaces)).collect();
            assert_eq!(got, (1..=n as u8).collect::<Vec<_>>(), "positions run 1..n with no gap or repeat");
        }
    }

    #[test]
    fn palette_rows_and_menu_items_come_from_the_registry() {
        let ids: Vec<&str> = palette_rows().iter().map(|r| r.0).collect();
        assert_eq!(
            ids,
            ["agents", "accounts", "help", "lead", "permission", "handoff", "clear", "rail", "compact", "quit"]
        );
        assert_eq!(palette_rows()[5], ("handoff", "write a handoff", "/handoff"));
        let menu: Vec<&str> = menu_items().iter().map(|r| r.0).collect();
        assert_eq!(menu, ["open", "cancel", "fork", "resume", "copy_id", "copy_result", "status", "cost"]);
        assert_eq!(menu_items()[0], ("open", "Open transcript"));
    }

    fn msgs(line: &str, agents: &[String], workflows: &[String]) -> String {
        match send(line, &Env::bare(agents, workflows)) {
            Sent::Msgs(m) => m.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join("|"),
            Sent::Login(p, _) => format!("LOGIN {p}"),
            Sent::Bad(why) => format!("BAD {why}"),
        }
    }

    #[test]
    fn workflow_subcommands_are_file_commands_and_run_is_the_old_command() {
        let m = |line: &str| msgs(line, &[], &[]);
        assert_eq!(m("/workflow list"), "Command(WorkflowFile(List))");
        assert_eq!(m("/workflow show review"), "Command(WorkflowFile(Show(\"review\")))");
        assert_eq!(m("/workflow show"), "BAD /workflow needs a workflow name");
        assert_eq!(
            m("/workflow save review"),
            "Command(WorkflowFile(Save { name: \"review\", user: false, force: false }))"
        );
        assert_eq!(
            m("/workflow save --force review --user"),
            "Command(WorkflowFile(Save { name: \"review\", user: true, force: true }))"
        );
        assert_eq!(m("/workflow save"), "BAD /workflow save needs a name");
        assert!(m("/workflow save a b").starts_with("BAD"));
        assert!(m("/workflow save --nope x").starts_with("BAD"));
        // A bad name never becomes a message, so nothing downstream can look at the disk for it.
        for bad in ["../x", "A", ".x", "a/b", "-x"] {
            assert!(m(&format!("/workflow save {bad}")).starts_with("BAD `"), "{bad}");
        }
        assert_eq!(m("/workflow run review the diff"), m("/workflow review the diff"));
    }

    #[test]
    fn workflow_new_needs_a_goal_and_a_lead() {
        let agents: Vec<String> = Vec::new();
        let workflows: Vec<String> = Vec::new();
        let bare = Env::bare(&agents, &workflows);
        let with_lead = Env { lead: Some(lead_run()), ..bare };
        let said = |env: &Env, line: &str| match send(line, env) {
            Sent::Msgs(m) => m.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join("|"),
            Sent::Bad(why) => format!("BAD {why}"),
        };
        assert_eq!(said(&bare, "/workflow new review a pull request"), "BAD /workflow new is not available: no lead is running");
        assert_eq!(said(&with_lead, "/workflow new"), "BAD /workflow needs a goal, in words");
        assert_eq!(
            said(&with_lead, "/workflow new review a pull request and leave a summary"),
            "Command(WorkflowAuthor { goal: \"review a pull request and leave a summary\" })"
        );
    }

    fn lead_run() -> RunId {
        "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
    }

    #[test]
    fn send_table_covers_every_existing_command_unchanged() {
        let agents = vec!["search".to_owned()];
        let m = |line: &str| msgs(line, &agents, &["fix".to_owned()]);
        assert!(m("/fork try the other way").starts_with("SubmitPrompt { text: \"/fork try the other way\" }"));
        assert!(m("/compact").contains("/compact"));
        assert!(m("/delegate search map it").contains("Delegate") && m("/delegate search map it").contains("map it"));
        assert!(m("/delegate nobody map it").starts_with("BAD"));
        assert!(m("/delegate search").starts_with("BAD"));
        assert_eq!(m("/clear"), "Command(OpenScreen(ConfirmClear))");
        assert_eq!(m("/handoff"), "Command(CreateHandoff { outcome: None })");
        assert!(m("/ledger now ship it").contains("LedgerUpdate { section: \"now\", text: \"ship it\" }"));
        assert!(m("/ledger now").starts_with("BAD"));
        assert_eq!(m("/model opus"), "Command(SetModel { target: Lead, model: \"opus\" })");
        assert_eq!(m("/model"), "Command(OpenScreen(Models(Lead)))");
        assert_eq!(m("/advisor opus"), "Command(SetModel { target: Advisor, model: \"opus\" })");
        assert_eq!(m("/advisor off"), "Command(SetModel { target: Advisor, model: \"off\" })");
        assert_eq!(m("/router fast"), "Command(SetModel { target: Router, model: \"fast\" })");
        assert_eq!(m("/advisor"), "BAD /advisor needs a model or off");
        assert_eq!(m("/model @advisor off"), "Command(SetModel { target: Advisor, model: \"off\" })");
        assert!(m("/workflow review the diff").contains("Workflow { name: \"review\", input: \"the diff\" }"));
        assert_eq!(m("/workflow run review the diff"), m("/workflow review the diff"));
        assert_eq!(m("/workflow run"), "BAD /workflow needs a workflow name");
        assert!(m("/fix the bug").contains("Workflow { name: \"fix\", input: \"the bug\" }"));
        assert_eq!(m("/build x"), "BAD /build is not available: no workflow named build");
        assert_eq!(m("/agents"), "Command(OpenScreen(Agents))");
        assert_eq!(m("/accounts"), "Command(OpenScreen(Accounts))");
        assert_eq!(m("/login"), "Command(OpenScreen(Providers))");
        assert_eq!(m("/login anthropic"), "LOGIN anthropic");
        assert_eq!(m("/login sk-ant-123"), "BAD /login: no provider named sk-ant-123 (/login lists them)");
        assert_eq!(m("/help"), "Command(OpenHelp)");
        assert_eq!(m("/nope"), "BAD no command /nope (try /help)");
        assert_eq!(m("hello"), "SubmitPrompt { text: \"hello\" }");
    }

    fn runs() -> Vec<(RunId, String)> {
        ["grep", "search", "grep"].iter().map(|a| (RunId::new(), (*a).to_owned())).collect()
    }

    #[test]
    fn target_resolves_agent_run_prefix_lead_advisor_router_and_fallbacks() {
        let rs = runs();
        let lead = RunId::new();
        let card = rs[1].0;
        let mut env = Env { runs: &rs, lead: Some(lead), ..Env::bare(&[], &[]) };
        let t = |env: &Env, s: &str| parse_target(s, env).map(|(t, rest)| (t, rest.to_owned()));
        // The newest run of the agent.
        assert_eq!(t(&env, "@grep haiku"), Ok((ModelTarget::Run(rs[2].0), "haiku".into())));
        assert_eq!(t(&env, "@search haiku"), Ok((ModelTarget::Run(rs[1].0), "haiku".into())));
        // A run id prefix.
        let id = rs[0].0.to_string();
        assert_eq!(t(&env, &format!("@{} x", &id[..20])), Ok((ModelTarget::Run(rs[0].0), "x".into())));
        assert_eq!(t(&env, "@lead x"), Ok((ModelTarget::Lead, "x".into())));
        assert_eq!(t(&env, "@advisor off"), Ok((ModelTarget::Advisor, "off".into())));
        assert_eq!(t(&env, "@router fast"), Ok((ModelTarget::Router, "fast".into())));
        // No target: the lead, or the focused card.
        assert_eq!(t(&env, "opus"), Ok((ModelTarget::Lead, "opus".into())));
        env.focused = Some(card);
        assert_eq!(t(&env, "opus"), Ok((ModelTarget::Run(card), "opus".into())));
        assert!(t(&env, "@nobody x").is_err());
    }

    #[test]
    fn model_at_agent_changes_that_run_and_a_focused_card_is_the_default() {
        let rs = runs();
        let card = rs[1].0;
        let mut env = Env { runs: &rs, ..Env::bare(&[], &[]) };
        let line = |env: &Env, l: &str| match send(l, env) {
            Sent::Msgs(m) => match &m[..] {
                [Msg::Command(Command::SetModel { target, model })] => Ok((*target, model.clone())),
                other => panic!("{other:?}"),
            },
            Sent::Login(..) => panic!("/model never logs in"),
            Sent::Bad(why) => Err(why),
        };
        assert_eq!(line(&env, "/model @grep haiku"), Ok((ModelTarget::Run(rs[2].0), "haiku".into())));
        assert_eq!(line(&env, "/model @lead haiku"), Ok((ModelTarget::Lead, "haiku".into())));
        assert_eq!(line(&env, "/model haiku"), Ok((ModelTarget::Lead, "haiku".into())));
        env.focused = Some(card);
        assert_eq!(line(&env, "/model haiku"), Ok((ModelTarget::Run(card), "haiku".into())));
        // With no reference the line opens the picker for that target.
        let Sent::Msgs(m) = send("/model @grep", &env) else { panic!("the picker opens") };
        assert!(matches!(&m[..], [Msg::Command(Command::OpenScreen(Screen::Models(ModelTarget::Run(r))))] if *r == rs[2].0), "{m:?}");
    }

    #[test]
    fn ambiguous_run_prefix_is_a_notice_with_candidates() {
        let rs = runs();
        let env = Env { runs: &rs, ..Env::bare(&[], &[]) };
        // Every ULID made in this test starts with the same few characters.
        let common: String = rs[0].0.to_string().chars().take(4).collect();
        let err = parse_target(&format!("@{common} x"), &env).unwrap_err();
        assert!(err.starts_with(&format!("@{common} matches ")), "{err}");
        assert!(err.contains(&rs[0].0.to_string()) && err.contains(&rs[2].0.to_string()), "{err}");
    }

    #[test]
    fn agent_mention_prefix_is_never_a_command() {
        let agents = vec!["grep".to_owned()];
        assert_eq!(msgs("@agent-grep find it", &agents, &[]), "SubmitPrompt { text: \"@agent-grep find it\" }");
        let env = Env::bare(&agents, &[]);
        assert_eq!(parse_target("@agent-grep x", &env), Ok((ModelTarget::Lead, "@agent-grep x")));
    }

    #[test]
    fn a_need_that_is_not_met_is_a_notice_and_the_popup_leaves_the_command_out() {
        let env = Env::bare(&[], &[]);
        assert!(slash_offered(&env).iter().all(|(n, _)| n != "/fix"));
        let wf = ["fix".to_owned()];
        assert!(slash_offered(&Env::bare(&[], &wf)).iter().any(|(n, _)| n == "/fix"));
        assert_eq!(slash_offered(&env).len(), 13);
        let spec = find("open").unwrap();
        assert_eq!(env.unmet(spec, None).as_deref(), Some("no run yet"));
    }

    /// A session with a lead and three workers in different states, for the per-run commands.
    struct Session {
        runs: Vec<(RunId, String)>,
        states: Vec<(RunId, RunStatus)>,
    }

    fn session() -> Session {
        let rows = [("lead", RunStatus::Running), ("grep", RunStatus::Running), ("search", RunStatus::Failed), ("review", RunStatus::Completed)];
        let runs: Vec<(RunId, String)> = rows.iter().map(|(a, _)| (RunId::new(), (*a).to_owned())).collect();
        let states = runs.iter().zip(rows).map(|((id, _), (_, st))| (*id, st)).collect();
        Session { runs, states }
    }

    fn env(s: &Session) -> Env<'_> {
        Env { runs: &s.runs, states: &s.states, lead: Some(s.runs[0].0), ..Env::bare(&[], &[]) }
    }

    fn sent(line: &str, env: &Env) -> String {
        match send(line, env) {
            Sent::Msgs(m) => m.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join("|"),
            Sent::Login(p, _) => format!("LOGIN {p}"),
            Sent::Bad(why) => format!("BAD {why}"),
        }
    }

    #[test]
    fn every_run_command_resolves_the_target_the_same_way() {
        let s = session();
        let (lead, grep, search) = (s.runs[0].0, s.runs[1].0, s.runs[2].0);
        let prefix = grep.to_string();
        let mut e = env(&s);
        // (line, what the command sends for the run it resolves to, with `{}` for the run)
        let rows = [
            ("/background", "Command(Background { run: {} })"),
            ("/log", "Command(OpenTranscript({}))"),
            ("/status", "Command(OpenScreen(RunStatus({})))"),
            ("/message hi", "Command(SendMessage { run: {}, text: \"hi\" })"),
        ];
        for (line, want) in rows {
            let (word, rest) = line.split_once(' ').map_or((line, ""), |(w, r)| (w, r));
            let with = |t: &str| format!("{word} {t} {rest}").trim().to_owned();
            e.focused = None;
            assert_eq!(sent(&with("@grep"), &e), want.replace("{}", &format!("{grep:?}")), "{line} @agent");
            assert_eq!(sent(&with(&format!("@{prefix}")), &e), want.replace("{}", &format!("{grep:?}")), "{line} @runprefix");
            assert_eq!(sent(&with("@lead"), &e), want.replace("{}", &format!("{lead:?}")), "{line} @lead");
            assert_eq!(sent(&with(""), &e), want.replace("{}", &format!("{lead:?}")), "{line} lead fallback");
            e.focused = Some(search);
            assert_eq!(sent(&with(""), &e), want.replace("{}", &format!("{search:?}")), "{line} focused card");
        }
    }

    #[test]
    fn cancel_asks_on_the_lead_and_not_on_a_worker() {
        let s = session();
        let e = env(&s);
        assert_eq!(sent("/cancel @lead", &e), format!("Command(OpenScreen(ConfirmCancel({:?})))", s.runs[0].0));
        assert_eq!(sent("/cancel @grep", &e), format!("Command(Cancel {{ run: {:?} }})", s.runs[1].0));
    }

    #[test]
    fn retry_and_resume_are_notices_in_the_wrong_state() {
        let s = session();
        let e = env(&s);
        assert_eq!(sent("/retry @grep", &e), "BAD /retry is not available: the run is running");
        assert_eq!(sent("/retry @search", &e), format!("Command(Retry {{ run: {:?} }})", s.runs[2].0));
        assert_eq!(sent("/resume @grep go on", &e), "BAD /resume is not available: the run is running");
        assert!(sent("/resume @review go on", &e).contains("message: \"go on\""));
        assert!(sent("/resume @review", &e).contains("message: \"continue\""));
        let offered: Vec<String> = slash_offered(&e).into_iter().map(|(n, _)| n).collect();
        assert!(offered.contains(&"/status".to_owned()) && !offered.contains(&"/retry".to_owned()), "{offered:?}");
        let focused = Env { focused: Some(s.runs[2].0), ..e };
        assert!(slash_offered(&focused).iter().any(|(n, _)| n == "/retry"));
    }

    #[test]
    fn status_takes_a_seat_and_cost_without_a_target_is_the_session_table() {
        let s = session();
        let e = env(&s);
        assert_eq!(sent("/status @advisor", &e), "Command(OpenScreen(AdvisorStatus))");
        assert_eq!(sent("/status @router", &e), "Command(OpenScreen(RouterStatus))");
        assert_eq!(sent("/cancel @advisor", &e), "BAD /cancel takes a run, not a seat");
        assert_eq!(sent("/cost", &e), "Command(OpenScreen(Cost))");
        let focused = Env { focused: Some(s.runs[1].0), ..e };
        assert_eq!(sent("/cost", &focused), "Command(OpenScreen(Cost))", "a focused card does not turn it into a status");
        assert_eq!(sent("/cost @grep", &e), format!("Command(OpenScreen(RunStatus({:?})))", s.runs[1].0));
        assert_eq!(sent("/message @grep", &e), "BAD /message needs a message");
    }

    #[test]
    fn the_run_commands_send_the_existing_variants() {
        // No new `Command` variant: each row maps onto one the keys already use.
        let s = session();
        let e = env(&s);
        for (line, variant) in [("/cancel @grep", "Cancel"), ("/retry @search", "Retry"), ("/message @grep x", "SendMessage"), ("/resume @review", "Resume"), ("/background @grep", "Background"), ("/log @grep", "OpenTranscript")] {
            assert!(sent(line, &e).starts_with(&format!("Command({variant}")), "{line}");
        }
    }
}
