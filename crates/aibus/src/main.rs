//! The aibus binary: wiring for the TUI and headless modes.
#![forbid(unsafe_code)]

mod account;
mod agent_cmd;
mod agent_watch;
mod cli;
mod commands;
mod completions;
mod control;
mod doctor;
mod env;
mod events;
mod headless;
mod keychain;
mod logfile;
mod logging;
mod plain;
mod providers;
mod session_cmd;
mod signals;
mod startup;
mod term;
mod trace;

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
    let path = cli::subcommand_path(&matches).join(" ");
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let not_yet = |what: &str| -> Result<u8, AibusError> {
        eprintln!("aibus: {what}: not yet");
        Ok(2)
    };
    match cli.command {
        None => bare(cli.global, cli.prompt, redactor),
        Some(Command::Run(args)) => headless::run(cli.global, args, redactor.clone()),
        Some(Command::Version) => {
            let v = env!("CARGO_PKG_VERSION");
            if cli.global.json {
                println!("{}", serde_json::json!({ "version": v }));
            } else {
                println!("aibus {v}");
            }
            Ok(0)
        }
        Some(Command::Config(c)) => commands::config(&c, &cli.global),
        Some(Command::Agent(c)) => agent_cmd::agent(&c, &cli.global),
        Some(Command::Account(c)) => commands::account(&c, &cli.global, redactor),
        Some(Command::Session(c)) => session_cmd::session(&c, &cli.global, redactor),
        Some(Command::Workflow(WorkflowCmd::List { names })) => {
            commands::workflow_list(&cli.global, names)
        }
        Some(Command::Doctor { session }) => {
            doctor::doctor(&cli.global, session.as_deref(), redactor, &keychain::OsKeychain)
        }
        Some(Command::Completions { shell }) => {
            print!("{}", completions::script(shell));
            Ok(0)
        }
        Some(_) => not_yet(&path),
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
    eprintln!("aibus: the TUI: not yet");
    Ok(2)
}
