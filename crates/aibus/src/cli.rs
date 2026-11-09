//! The command line (06 section 1) and `init`. Every subcommand and global flag parses.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

/// The dashboard key map (05 section 4), short enough for `--help`.
pub const KEY_MAP: &str = "\
Keys in the dashboard:
  Tab / Shift+Tab   cycle focus (prompt, cards, log, rail)
  Enter             submit in the prompt; open the transcript on a card or log row
  j k / arrows      move selection or scroll
  x                 stop the selected run (dismiss it when finished)
  r  m  R           retry a failed run; message a run; resume a partial run
  t  a  p           lead transcript; advisor rail; oldest open permission
  y  n  a  e        in the permission modal: allow, deny, always, edit
  :  /  f           command palette; filter; follow
  A  K              agents screen; accounts screen
  ?                 key map
  Ctrl+c            cancel the focused run; twice within 2 s quits
  q                 quit (asks for the outcome if the session did work)
  Esc               close a modal; clear the prompt";

#[derive(Parser, Debug)]
#[command(
    name = "aibus",
    version,
    about = "Run agents against a shared graph, in a terminal or headless",
    after_help = KEY_MAP
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,

    /// First prompt for the TUI
    pub prompt: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options that work before or after the subcommand.
#[derive(Args, Debug, Default)]
pub struct Global {
    /// Lead model for this session (alias or id)
    #[arg(long, global = true, value_name = "ALIAS|ID")]
    pub model: Option<String>,
    /// Advisor model (alias, id or off)
    #[arg(long, global = true, value_name = "ALIAS|ID|off")]
    pub advisor: Option<String>,
    /// Router model (alias, id or off)
    #[arg(long, global = true, value_name = "ALIAS|ID|off")]
    pub router: Option<String>,
    /// Account for the lead
    #[arg(long, global = true, value_name = "NAME")]
    pub account: Option<String>,
    /// Run the session as this agent
    #[arg(long, global = true, value_name = "NAME")]
    pub agent: Option<String>,
    /// Inline agent definitions, highest precedence
    #[arg(long, global = true, value_name = "JSON")]
    pub agents: Option<String>,
    /// default, accept-edits, dont-ask, bypass or plan
    ///
    /// bypass needs AIBUS_I_KNOW=1 and skips every permission prompt. The tool allow-list is a
    /// process boundary inside aibus and not a sandbox: a command that starts through `bash`
    /// has your full rights. `[tools] sandbox` is the optional OS sandbox for it.
    #[arg(
        long,
        global = true,
        value_name = "MODE",
        value_parser = ["default", "accept-edits", "dont-ask", "bypass", "plan"]
    )]
    pub permission_mode: Option<String>,
    /// Allow a tool for this session (repeatable)
    #[arg(long, global = true, value_name = "TOOL", action = ArgAction::Append)]
    pub allow: Vec<String>,
    /// Deny a tool for this session (repeatable)
    #[arg(long, global = true, value_name = "TOOL", action = ArgAction::Append)]
    pub deny: Vec<String>,
    /// Ledger and handoff topic
    #[arg(long, global = true, value_name = "NAME")]
    pub topic: Option<String>,
    /// Project directory
    #[arg(long, global = true, value_name = "DIR")]
    pub cwd: Option<PathBuf>,
    /// Config file override
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Machine output where it applies
    #[arg(long, global = true)]
    pub json: bool,
    /// Do not run any hooks
    #[arg(long, global = true)]
    pub no_hooks: bool,
    /// UI theme
    #[arg(long, global = true, value_name = "NAME")]
    pub theme: Option<String>,
    /// ASCII-only UI
    #[arg(long, global = true)]
    pub ascii: bool,
    /// Turn the mouse off
    #[arg(long, global = true)]
    pub no_mouse: bool,
    /// More logging to the log file (-v, -vv)
    #[arg(short = 'v', global = true, action = ArgAction::Count)]
    pub verbose: u8,
    /// Less logging to the log file
    #[arg(short = 'q', global = true)]
    pub quiet: bool,
    /// Write a Chrome trace of commits and effects
    #[arg(long, global = true, value_name = "FILE")]
    pub trace: Option<PathBuf>,
}

impl Global {
    /// The project directory: `--cwd`, or where the process is.
    pub fn project_dir(&self) -> std::io::Result<PathBuf> {
        match &self.cwd {
            Some(d) => Ok(d.clone()),
            None => std::env::current_dir(),
        }
    }

    /// The XDG paths from the environment, with `--config` standing in for `AIBUS_CONFIG`.
    pub fn paths(&self) -> Result<aibus_core::persist::paths::Paths, aibus_core::AibusError> {
        let mut paths = aibus_core::persist::paths::Paths::from_env()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(c) = &self.config {
            paths.config_override = Some(c.clone());
        }
        Ok(paths)
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Headless session, NDJSON events on stdout
    Run(RunArgs),
    /// Agent definitions
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Provider accounts
    #[command(subcommand)]
    Account(AccountCmd),
    /// Past sessions
    #[command(subcommand)]
    Session(SessionCmd),
    /// Handoff files
    #[command(subcommand)]
    Handoff(HandoffCmd),
    /// Continuity ledgers
    #[command(subcommand)]
    Ledger(LedgerCmd),
    /// Agent memory
    #[command(subcommand)]
    Memory(MemoryCmd),
    /// Workflows
    #[command(subcommand)]
    Workflow(WorkflowCmd),
    /// The config file
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Put the example workflows in .aibus/workflows
    Init,
    /// Check the install and the config
    Doctor {
        /// Also print the live memory numbers of this session
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// Print a shell completion script
    Completions { shell: Shell },
    /// Print the version
    Version,
}

#[derive(Args, Debug, Default)]
pub struct RunArgs {
    /// The prompt; `-` or no prompt on a non-tty stdin reads it from stdin
    pub prompt: Option<String>,
    /// Run a workflow instead of one agent
    #[arg(long, value_name = "NAME")]
    pub workflow: Option<String>,
    /// Outcome to record in the handoff
    #[arg(long, value_name = "OUTCOME")]
    pub outcome: Option<String>,
    /// Play this RON script on the fake provider instead of the built-in one
    #[arg(long, value_name = "FILE")]
    pub script: Option<PathBuf>,
    /// Write msgs.jsonl, script.ron and snapshot.hash here for replay
    #[arg(long, value_name = "DIR")]
    pub record: Option<PathBuf>,
    /// Use this directory for the session instead of one under the state dir
    #[arg(long, value_name = "DIR")]
    pub session_dir: Option<PathBuf>,
    /// Continue this run of the session in `session_dir` instead of starting one (set by
    /// `session resume --run`)
    #[arg(skip)]
    pub resume: Option<aibus_core::RunId>,
}

#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    List {
        /// Names only, one per line (the completion scripts call this)
        #[arg(long, hide = true)]
        names: bool,
    },
    Show { name: String },
    New { name: String },
    Validate { path: Option<PathBuf> },
    Import { dir: PathBuf },
    Where,
}

#[derive(Subcommand, Debug)]
pub enum AccountCmd {
    Add(AccountAdd),
    List {
        /// Names only, one per line (the completion scripts call this)
        #[arg(long, hide = true)]
        names: bool,
    },
    Show { name: String },
    Test { name: String },
    Remove { name: String },
    Enable { name: String },
    Disable { name: String },
}

#[derive(Args, Debug)]
pub struct AccountAdd {
    pub name: String,
    #[arg(long, value_name = "ID")]
    pub provider: String,
    /// Read the key from stdin
    #[arg(long, group = "source")]
    pub key_stdin: bool,
    /// Read the key from this environment variable
    #[arg(long, value_name = "VAR", group = "source")]
    pub env: Option<String>,
    /// Read the key from this file
    #[arg(long, value_name = "PATH", group = "source")]
    pub file: Option<PathBuf>,
    /// Log in with OAuth instead of a key (only for providers that offer it)
    #[arg(long, group = "source")]
    pub oauth: bool,
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    #[arg(long, value_name = "TEXT")]
    pub label: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    List {
        /// Read the session directories again instead of trusting index.sqlite
        #[arg(long)]
        rebuild: bool,
        /// Ids only, one per line (the completion scripts call this)
        #[arg(long, hide = true)]
        names: bool,
    },
    Show {
        id: String,
    },
    Resume {
        id: String,
        #[arg(long, value_name = "RUN")]
        run: Option<String>,
        message: Option<String>,
        /// Play this RON script on the fake provider (for tests)
        #[arg(long, value_name = "FILE", hide = true)]
        script: Option<PathBuf>,
    },
    Export {
        id: String,
        #[arg(long, value_parser = ["md", "jsonl"])]
        format: Option<String>,
    },
    Log {
        id: String,
    },
    Replay {
        id: String,
        /// Replay on the fake provider: the recorded stream needs no other
        #[arg(long, value_parser = ["fake"], default_value = "fake")]
        provider: String,
    },
    Prune {
        #[arg(long, value_name = "AGE")]
        older_than: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum HandoffCmd {
    /// Write a handoff from the topic's ledger
    Create {
        /// How the session went: SUCCEEDED, PARTIAL_PLUS, PARTIAL_MINUS or FAILED (or AIBUS_OUTCOME)
        #[arg(long, value_name = "OUTCOME")]
        outcome: Option<String>,
        /// Instead of the ledger's Goal
        #[arg(long, value_name = "TEXT")]
        goal: Option<String>,
        /// Instead of the ledger's first In Progress item
        #[arg(long, value_name = "TEXT")]
        now: Option<String>,
    },
    /// The handoffs of --topic, or of every topic, newest first
    List,
    /// A handoff, by default the newest
    Show {
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum LedgerCmd {
    Show,
    New { topic: String },
    Update {
        section: String,
        text: String,
        /// Add to the section instead of replacing it
        #[arg(long)]
        append: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryCmd {
    Recall {
        query: String,
        #[arg(short = 'k', value_name = "N")]
        k: Option<u32>,
    },
    Add {
        text: String,
        #[arg(long = "type", value_name = "TYPE")]
        kind: Option<String>,
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
    },
    Index,
}

#[derive(Subcommand, Debug)]
pub enum WorkflowCmd {
    List {
        /// Names only, one per line (the completion scripts call this)
        #[arg(long, hide = true)]
        names: bool,
    },
    Run {
        name: String,
        input: String,
        /// Play this RON script on the fake provider (for tests)
        #[arg(long, value_name = "FILE", hide = true)]
        script: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print where the user config and the project settings live
    Path,
    /// Print the merged config
    Show,
    /// Write a default config.toml
    Init,
    /// Check the user and project files
    Validate,
    /// Open the user config in $EDITOR
    Edit,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Every path in 06 section 1, one per line.
    const COMMANDS: &[&str] = &[
        "run",
        "agent list",
        "agent show",
        "agent new",
        "agent validate",
        "agent import",
        "agent where",
        "account add",
        "account list",
        "account show",
        "account test",
        "account remove",
        "account enable",
        "account disable",
        "session list",
        "session show",
        "session resume",
        "session export",
        "session log",
        "session replay",
        "session prune",
        "handoff create",
        "handoff list",
        "handoff show",
        "ledger show",
        "ledger new",
        "ledger update",
        "memory recall",
        "memory add",
        "memory index",
        "workflow list",
        "workflow run",
        "init",
        "config path",
        "config show",
        "config init",
        "config validate",
        "config edit",
        "doctor",
        "completions",
        "version",
    ];

    const GLOBAL_FLAGS: &[&str] = &[
        "model",
        "advisor",
        "router",
        "account",
        "agent",
        "agents",
        "permission-mode",
        "allow",
        "deny",
        "topic",
        "cwd",
        "config",
        "json",
        "no-hooks",
        "theme",
        "ascii",
        "no-mouse",
        "trace",
    ];

    fn leaves(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        let subs: Vec<_> = cmd.get_subcommands().filter(|s| s.get_name() != "help").collect();
        if subs.is_empty() {
            out.push(prefix.trim().to_owned());
        }
        for s in subs {
            leaves(s, &format!("{prefix} {}", s.get_name()), out);
        }
    }

    #[test]
    fn clap_accepts_the_definition() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_subcommand_in_the_spec_exists_and_nothing_else_does() {
        let mut found = Vec::new();
        leaves(&Cli::command(), "", &mut found);
        // `agent`, `session` ... are not leaves of their own: only their children count.
        found.retain(|p| !p.is_empty());
        let mut want: Vec<String> = COMMANDS.iter().map(|s| (*s).to_owned()).collect();
        found.sort();
        want.sort();
        assert_eq!(found, want);
    }

    #[test]
    fn every_global_flag_exists_and_short_forms_count() {
        let root = Cli::command();
        for f in GLOBAL_FLAGS {
            let a = root
                .get_arguments()
                .find(|a| a.get_long() == Some(f))
                .unwrap_or_else(|| panic!("--{f} missing"));
            assert!(a.is_global_set(), "--{f} is not global");
        }
        for s in ['v', 'q'] {
            assert!(root.get_arguments().any(|a| a.get_short() == Some(s)), "-{s}");
        }
    }

    #[test]
    fn help_works_for_the_root_and_every_subcommand() {
        let mut paths = Vec::new();
        leaves(&Cli::command(), "", &mut paths);
        for p in std::iter::once(String::new()).chain(paths) {
            let mut argv = vec!["aibus".to_owned()];
            argv.extend(p.split_whitespace().map(str::to_owned));
            argv.push("--help".into());
            let err = Cli::try_parse_from(&argv).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp, "{argv:?}");
        }
    }

    #[test]
    fn global_flags_work_before_and_after_the_subcommand() {
        for argv in [
            ["aibus", "--model", "haiku", "--json", "run", "hi"].as_slice(),
            ["aibus", "run", "hi", "--model", "haiku", "--json"].as_slice(),
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert_eq!(cli.global.model.as_deref(), Some("haiku"));
            assert!(cli.global.json);
            let Some(Command::Run(r)) = cli.command else { panic!("not run") };
            assert_eq!(r.prompt.as_deref(), Some("hi"));
        }
    }

    #[test]
    fn a_bare_prompt_is_the_tui_prompt_and_verbosity_counts() {
        let cli = Cli::try_parse_from(["aibus", "-vv", "fix the build"]).unwrap();
        assert_eq!(cli.prompt.as_deref(), Some("fix the build"));
        assert_eq!(cli.global.verbose, 2);
        assert!(cli.command.is_none());
    }

    #[test]
    fn repeatable_flags_and_account_sources() {
        let cli = Cli::try_parse_from([
            "aibus", "--allow", "bash(cargo *)", "--allow", "edit", "--deny", "web", "account",
            "add", "work", "--provider", "anthropic", "--env", "MY_KEY", "--label", "Work",
        ])
        .unwrap();
        assert_eq!(cli.global.allow, ["bash(cargo *)", "edit"]);
        assert_eq!(cli.global.deny, ["web"]);
        let Some(Command::Account(AccountCmd::Add(a))) = cli.command else { panic!() };
        assert_eq!(a.env.as_deref(), Some("MY_KEY"));
        assert!(Cli::try_parse_from([
            "aibus", "account", "add", "w", "--provider", "p", "--env", "X", "--key-stdin"
        ])
        .is_err());
    }

    #[test]
    fn doctor_takes_a_session_and_the_name_helpers_parse() {
        let cli = Cli::try_parse_from(["aibus", "doctor", "--session", "01JC0000000000000000000000"]).unwrap();
        let Some(Command::Doctor { session }) = cli.command else { panic!() };
        assert_eq!(session.as_deref(), Some("01JC0000000000000000000000"));
        for sub in ["agent", "account", "session", "workflow"] {
            assert!(Cli::try_parse_from(["aibus", sub, "list", "--names"]).is_ok(), "{sub}");
        }
    }

    #[test]
    fn the_root_help_ends_with_the_key_map() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("Keys in the dashboard"), "{help}");
        assert!(help.trim_end().ends_with("Esc               close a modal; clear the prompt"));
    }

    #[test]
    fn a_bad_permission_mode_is_a_usage_error() {
        let err = Cli::try_parse_from(["aibus", "--permission-mode", "yolo", "doctor"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }
}
