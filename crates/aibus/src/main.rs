//! The aibus binary: wiring for the TUI and headless modes.
#![forbid(unsafe_code)]

mod account;
mod cli;
mod commands;
mod events;
mod headless;
mod keychain;
mod providers;
mod signals;
mod startup;

use aibus_core::{AibusError, Redactor};
use clap::{CommandFactory, FromArgMatches};

use cli::{Cli, Command};

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
        None => not_yet("the TUI"),
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
        Some(Command::Account(c)) => commands::account(&c, &cli.global, redactor),
        Some(_) => not_yet(&path),
    }
}
