//! The aibus binary: wiring for the TUI and headless modes.
#![forbid(unsafe_code)]

mod account;
mod agent_cmd;
mod agent_watch;
mod cli;
mod commands;
mod completions;
mod continuity;
mod continuity_cmd;
mod control;
mod doctor;
mod env;
mod events;
mod gcloud;
mod headless;
mod keychain;
mod launch;
mod logfile;
mod logging;
mod mcp_cmd;
mod mcp_http;
mod net;
mod oauth;
mod plain;
mod providers;
mod session_cmd;
mod signals;
mod startup;
mod term;
mod trace;
mod tui;

use std::io::IsTerminal;

use aibus_core::{AibusError, Redactor};
use clap::{CommandFactory, FromArgMatches};

use cli::{Cli, Command, RunArgs, WorkflowCmd};

fn main() -> std::process::ExitCode {
    let redactor = Redactor::new();
    // A panic is an unexpected error: exit 1 (06 section 3), not Rust's 101.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| real_main(&redactor)));
    match outcome {
        Ok(Ok(code)) => std::process::ExitCode::from(code),
        Ok(Err(e)) => {
            eprintln!("aibus: {}", redactor.redact(&e.to_string()));
            std::process::ExitCode::from(e.exit_code())
        }
        Err(_) => std::process::ExitCode::from(1),
    }
}

fn real_main(redactor: &Redactor) -> Result<u8, AibusError> {
    // clap prints its own help and usage errors and exits 0 or 2.
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    match cli.command {
        None => bare(cli.global, cli.prompt, redactor),
        Some(Command::Run(args)) => headless::run(cli.global, args, redactor.clone()),
        Some(Command::Version) => {
            if cli.global.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "version": env!("CARGO_PKG_VERSION"),
                        "tag": Some(env!("AIBUS_BUILD_TAG")).filter(|t| !t.is_empty()),
                        "commit": env!("AIBUS_BUILD_COMMIT"),
                    })
                );
            } else {
                println!("aibus {}", doctor::build_id());
            }
            Ok(0)
        }
        Some(Command::Config(c)) => commands::config(&c, &cli.global),
        Some(Command::Agent(c)) => agent_cmd::agent(&c, &cli.global),
        Some(Command::Account(c)) => commands::account(&c, &cli.global, redactor),
        Some(Command::Session(cli::SessionCmd::Resume {
            id,
            run: Some(run),
            message,
            script,
        })) => {
            let mut args = session_cmd::resume_args(&cli.global, &id, &run, message)?;
            args.script = script;
            headless::run(cli.global, args, redactor.clone())
        }
        Some(Command::Session(c)) => session_cmd::session(&c, &cli.global, redactor),
        Some(Command::Ledger(c)) => continuity_cmd::ledger(&c, &cli.global),
        Some(Command::Handoff(c)) => continuity_cmd::handoff(&c, &cli.global),
        Some(Command::Memory(c)) => continuity_cmd::memory(&c, &cli.global, redactor),
        Some(Command::Workflow(WorkflowCmd::List { names })) => {
            commands::workflow_list(&cli.global, names)
        }
        Some(Command::Workflow(WorkflowCmd::Validate { target })) => {
            commands::workflow_validate(&cli.global, target.as_deref())
        }
        Some(Command::Workflow(WorkflowCmd::Show { name })) => commands::workflow_show(&cli.global, &name),
        Some(Command::Workflow(WorkflowCmd::Run { name, input, script })) => {
            // The same session as `aibus run --workflow <name> <input>`.
            let args = RunArgs {
                prompt: Some(input),
                workflow: Some(name),
                script,
                ..RunArgs::default()
            };
            headless::run(cli.global, args, redactor.clone())
        }
        Some(Command::Mcp(args)) => mcp_cmd::mcp(&cli.global, &args, redactor),
        Some(Command::Init) => commands::init(&cli.global),
        Some(Command::Doctor { session }) => {
            doctor::doctor(&cli.global, session.as_deref(), redactor, &keychain::OsKeychain)
        }
        Some(Command::Demo { text }) => aibus_tui::demo::print(text, std::io::stdout().lock())
            .map(|()| 0)
            .map_err(|e| AibusError::Io(std::io::Error::other(e))),
        Some(Command::Completions { shell }) => {
            print!("{}", completions::script(shell));
            Ok(0)
        }
    }
}

/// No subcommand (06 section 5). With stdout piped this is `aibus run`, with a note on stderr;
/// on a terminal it is the dashboard, which a `TERM=dumb` terminal cannot draw.
fn bare(
    global: cli::Global,
    prompt: Option<String>,
    redactor: &Redactor,
) -> Result<u8, AibusError> {
    if !std::io::stdout().is_terminal() {
        eprintln!("aibus: stdout is not a terminal, running as `aibus run`");
        let args = RunArgs {
            prompt,
            ..RunArgs::default()
        };
        return headless::run(global, args, redactor.clone());
    }
    term::check_tui(std::env::var("TERM").ok().as_deref())?;
    tui::run(global, prompt, redactor.clone())
}
