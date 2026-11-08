//! The `config` subcommands, which need no engine.

use std::io::IsTerminal;
use std::sync::Arc;

use aibus_core::config;
use aibus_core::persist::paths::project_dir;
use aibus_core::{AibusError, Redactor};
use serde_json::json;

use crate::account::{self, Io};
use crate::cli::{AccountCmd, ConfigCmd, Global};
use crate::keychain::OsKeychain;
use crate::providers::builtin_factories;

/// The account commands against the real keychain, environment, terminal and providers.
pub fn account(cmd: &AccountCmd, g: &Global, redactor: &Redactor) -> Result<u8, AibusError> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut stdout = std::io::stdout().lock();
    let ask = || rpassword::prompt_password("key (input hidden): ");
    let mut io = Io {
        paths: g.paths()?,
        keychain: Arc::new(OsKeychain),
        factories: builtin_factories(),
        env: Arc::new(|v| std::env::var(v).ok()),
        redactor: redactor.clone(),
        stdin: &mut input,
        prompt: std::io::stdin().is_terminal().then_some(&ask),
        out: &mut stdout,
    };
    account::run(cmd, g.json, &mut io)
}

pub fn config(cmd: &ConfigCmd, g: &Global) -> Result<u8, AibusError> {
    let paths = g.paths()?;
    let user = paths.config_file();
    let project = project_dir(&g.project_dir()?).join("settings.toml");
    match cmd {
        ConfigCmd::Path => {
            if g.json {
                println!("{}", json!({ "user": user, "project": project }));
            } else {
                println!("user\t{}", user.display());
                println!("project\t{}", project.display());
            }
        }
        ConfigCmd::Show => {
            let loaded = config::load(&user, Some(&project))?;
            for w in &loaded.warnings {
                eprintln!("aibus: warning: {w}");
            }
            if g.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&loaded.config).map_err(std::io::Error::other)?
                );
            } else {
                print!(
                    "{}",
                    toml::to_string_pretty(&loaded.config).map_err(std::io::Error::other)?
                );
            }
        }
        ConfigCmd::Init => {
            let text = toml::to_string_pretty(&config::Config::default())
                .map_err(std::io::Error::other)?;
            if let Some(dir) = user.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&user)?;
            std::io::Write::write_all(
                &mut file,
                format!("# aibus config: every value shown is the default (07 section 3).\n{text}")
                    .as_bytes(),
            )?;
            println!("wrote {}", user.display());
        }
        ConfigCmd::Validate => {
            let loaded = config::load(&user, Some(&project))?;
            for w in &loaded.warnings {
                eprintln!("aibus: warning: {w}");
            }
            if g.json {
                println!("{}", json!({ "ok": true, "warnings": loaded.warnings }));
            } else {
                println!("ok");
            }
        }
        ConfigCmd::Edit => {
            eprintln!("aibus: config edit: not yet");
            return Ok(2);
        }
    }
    Ok(0)
}
