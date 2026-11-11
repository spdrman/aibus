//! The command registry (05 section 4): one table that the slash line, the palette, the card menu,
//! the completion popup and the help text all read. A command is declared once, here, with the
//! surfaces it shows up on and what it does; nothing else in the crate names a command.
//!
//! `run` writes into the same `Out` the key handling fills, so a slash line, a palette row and a
//! menu item are three ways into one function and cannot drift apart.

use aibus_core::{AgentName, Command, Isolation, ModelTarget, Msg, Pane, RunId, Screen, UiIntent};

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
    /// The graph does not tell the dashboard about these two yet, so `Context::env` says yes.
    pub accounts: bool,
    pub mcp: bool,
}

impl<'a> Env<'a> {
    /// No session at all: only the agents and workflows given.
    #[cfg(test)]
    pub fn bare(agents: &'a [String], workflows: &'a [String]) -> Self {
        Self { agents, workflows, lead: None, focused: None, runs: &[], accounts: true, mcp: true }
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
        args: "<alias>",
        help: "change the lead's model",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(7, 0, 0),
        run: |c, out| if c.args.is_empty() { need(c, "an alias") } else { prompt_line(c, out) },
    },
    Spec {
        name: "advisor",
        args: "<alias>",
        help: "change the advisor's model",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(8, 0, 0),
        run: |c, out| if c.args.is_empty() { need(c, "an alias") } else { prompt_line(c, out) },
    },
    Spec {
        name: "workflow",
        args: "<name> <input>",
        help: "run a workflow",
        label: "",
        hint: "",
        scope: Scope::Global,
        needs: 0,
        surfaces: at(9, 0, 0),
        run: |c, out| match c.args.split_once(char::is_whitespace) {
            Some((name, input)) => command(out, Command::Workflow { name: name.to_owned(), input: input.trim().to_owned() }),
            None if !c.args.is_empty() => {
                command(out, Command::Workflow { name: c.args.to_owned(), input: String::new() })
            }
            None => need(c, "a workflow name"),
        },
    },
    Spec {
        name: "agents",
        args: "",
        help: "the agents screen",
        label: "agents screen",
        hint: "A",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(10, 1, 0),
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
        surfaces: at(11, 2, 0),
        run: |_, out| command(out, Command::OpenScreen(Screen::Accounts)),
    },
    Spec {
        name: "help",
        args: "",
        help: "the key map and the commands",
        label: "help: the key map",
        hint: "?",
        scope: Scope::Screen,
        needs: 0,
        surfaces: at(12, 3, 0),
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
        surfaces: at(13, 0, 0),
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
        surfaces: at(14, 0, 0),
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
        surfaces: at(15, 0, 0),
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
        surfaces: at(16, 0, 0),
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
        help: "cancel the run",
        label: "Cancel",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(0, 0, 2),
        run: |c, out| menu_run(c).map_or(Ok(()), |run| command(out, Command::Cancel { run })),
    },
    Spec {
        name: "resume",
        args: "",
        help: "resume a partial run",
        label: "Resume",
        hint: "",
        scope: Scope::Run,
        needs: RUNS,
        surfaces: at(0, 0, 4),
        run: |c, out| {
            menu_run(c).map_or(Ok(()), |run| command(out, Command::Resume { run, message: "continue".into(), parent: None }))
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
];

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
    /// Why `spec` is not available right now, or `None` when it is.
    fn unmet(&self, spec: &Spec) -> Option<String> {
        let missing = |bit: u8| spec.needs & bit != 0;
        if missing(LEAD) && self.lead.is_none() {
            return Some("no lead is running".into());
        }
        if missing(RUNS) && self.runs.is_empty() {
            return Some("no run yet".into());
        }
        if missing(WORKFLOWS) && !self.workflows.iter().any(|w| w == spec.name) {
            return Some(format!("no workflow named {}", spec.name));
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
    listed(|s| s.slash).into_iter().filter(|s| env.unmet(s).is_none()).map(|s| (format!("/{}", s.name), s.args)).collect()
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
    if let Some(why) = env.unmet(spec) {
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
    let mut out = Out::default();
    match (spec.run)(&Call { name: spec.name, via: Via::Slash, line, args, target, env }, &mut out) {
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
        assert_eq!(menu, ["open", "cancel", "fork", "resume", "copy_id", "copy_result"]);
        assert_eq!(menu_items()[0], ("open", "Open transcript"));
    }

    fn msgs(line: &str, agents: &[String], workflows: &[String]) -> String {
        match send(line, &Env::bare(agents, workflows)) {
            Sent::Msgs(m) => m.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join("|"),
            Sent::Bad(why) => format!("BAD {why}"),
        }
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
        assert_eq!(m("/model"), "BAD /model needs a model");
        assert_eq!(m("/advisor opus"), "Command(SetModel { target: Advisor, model: \"opus\" })");
        assert_eq!(m("/advisor off"), "Command(SetModel { target: Advisor, model: \"off\" })");
        assert_eq!(m("/router fast"), "Command(SetModel { target: Router, model: \"fast\" })");
        assert_eq!(m("/advisor"), "BAD /advisor needs a model or off");
        assert_eq!(m("/model @advisor off"), "Command(SetModel { target: Advisor, model: \"off\" })");
        assert!(m("/workflow review the diff").contains("Workflow { name: \"review\", input: \"the diff\" }"));
        assert!(m("/fix the bug").contains("Workflow { name: \"fix\", input: \"the bug\" }"));
        assert_eq!(m("/build x"), "BAD /build is not available: no workflow named build");
        assert_eq!(m("/agents"), "Command(OpenScreen(Agents))");
        assert_eq!(m("/accounts"), "Command(OpenScreen(Accounts))");
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
            Sent::Bad(why) => Err(why),
        };
        assert_eq!(line(&env, "/model @grep haiku"), Ok((ModelTarget::Run(rs[2].0), "haiku".into())));
        assert_eq!(line(&env, "/model @lead haiku"), Ok((ModelTarget::Lead, "haiku".into())));
        assert_eq!(line(&env, "/model haiku"), Ok((ModelTarget::Lead, "haiku".into())));
        env.focused = Some(card);
        assert_eq!(line(&env, "/model haiku"), Ok((ModelTarget::Run(card), "haiku".into())));
        assert_eq!(line(&env, "/model @grep"), Err("/model needs a model".into()));
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
        assert_eq!(env.unmet(spec).as_deref(), Some("no run yet"));
    }
}
