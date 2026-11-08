//! `aibus` on a terminal with no subcommand (06 section 5, Q19): the dashboard with an empty
//! prompt. The startup steps run as far as the runtime; what the terminal loop needs from the
//! config, the flags and the environment is settled here, in one function a test can call.

use std::path::{Path, PathBuf};

use aibus_core::config::Config;
use aibus_core::persist::paths::Paths;
use aibus_core::{AibusError, Redactor};
use aibus_tui::app::Options;
use aibus_tui::keys::Keymap;
use aibus_tui::{Prompt, Theme};

use crate::cli::{Global, RunArgs};
use crate::startup::{self, not_ready, Ctx};

/// The environment the loop reads: names, so a test can pass its own.
#[derive(Default)]
pub struct Env {
    pub no_color: Option<String>,
    pub colorterm: Option<String>,
    pub term: Option<String>,
    pub home: Option<String>,
    pub cwd: PathBuf,
    /// The command line, program name first.
    pub args: Vec<String>,
}

impl Env {
    pub fn from_process(cwd: &Path) -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self {
            no_color: var("NO_COLOR"),
            colorterm: var("COLORTERM"),
            term: var("TERM"),
            home: var("HOME"),
            cwd: cwd.to_owned(),
            args: std::env::args().collect(),
        }
    }
}

/// `/home/me/app` as `~/app`.
fn tilde(cwd: &Path, home: Option<&str>) -> String {
    let shown = cwd.to_string_lossy();
    match home.filter(|h| !h.is_empty()).and_then(|h| shown.strip_prefix(h)) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => shown.into_owned(),
    }
}

/// Everything the terminal loop is told: the theme (flag over config; `NO_COLOR` and `COLORTERM`
/// settle the colours), the mouse (`[ui] mouse` and `--no-mouse`), the key map (`[keys]` with
/// `keys.toml` on top) and where dragged sizes are kept. The strings are warnings to print.
pub fn options(
    global: &Global,
    cfg: &Config,
    paths: &Paths,
    keys_toml: Option<&str>,
    prompt: &str,
    env: &Env,
) -> (Options, Vec<String>) {
    let name = global.theme.as_deref().unwrap_or(&cfg.ui.theme);
    let ascii = global.ascii || cfg.ui.ascii;
    let theme = Theme::resolve(name, env.no_color.as_deref(), env.colorterm.as_deref(), ascii);
    let (keys, warnings) = Keymap::load(&cfg.keys, keys_toml);
    let launch: Vec<&str> = env.args.iter().skip(1).map(String::as_str).collect();
    let opts = Options {
        hz: cfg.ui.tick_hz,
        theme,
        keys,
        prompt: Prompt {
            cwd: tilde(&env.cwd, env.home.as_deref()),
            text: prompt.to_owned(),
            placeholder: std::iter::once("aibus").chain(launch).collect::<Vec<_>>().join(" "),
        },
        mouse: cfg.ui.mouse && !global.no_mouse,
        term: env.term.clone(),
        layout_file: Some(paths.state.join("ui.toml")),
    };
    (opts, warnings)
}

/// Returns the process exit code.
pub fn run(global: Global, prompt: Option<String>, redactor: Redactor) -> Result<u8, AibusError> {
    let args = RunArgs { prompt, ..RunArgs::default() };
    let mut ctx = Ctx::new(global, args, redactor);
    ctx.interactive = true;
    let started = startup::start(startup::interactive_steps(), &mut ctx)?;
    let driven = drive(&mut ctx);
    let stopped = startup::stop(started, &mut ctx);
    driven?;
    stopped?;
    Ok(0)
}

fn drive(ctx: &mut Ctx) -> Result<(), AibusError> {
    let (Some(loaded), Some(paths)) = (ctx.loaded.as_ref(), ctx.paths.as_ref()) else {
        return Err(not_ready("config"));
    };
    let keys_toml = std::fs::read_to_string(paths.config.join("keys.toml")).ok();
    let (opts, warnings) = options(
        &ctx.global,
        &loaded.config,
        paths,
        keys_toml.as_deref(),
        &ctx.prompt,
        &Env::from_process(&ctx.cwd),
    );
    for w in warnings {
        eprintln!("aibus: warning: {w}");
    }
    let (Some(rt), Some(app)) = (ctx.rt.as_ref(), ctx.app.take()) else {
        return Err(not_ready("runtime"));
    };
    rt.block_on(aibus_tui::app::run(app, opts))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::Cli;

    fn global(args: &[&str]) -> Global {
        let mut argv = vec!["aibus"];
        argv.extend(args);
        Cli::parse_from(argv).global
    }

    fn paths() -> Paths {
        Paths::resolve(|k| match k {
            "HOME" => Some("/home/me".into()),
            _ => None,
        })
        .unwrap()
    }

    fn build(args: &[&str], cfg: &Config, env: &Env) -> (Options, Vec<String>) {
        options(&global(args), cfg, &paths(), None, "", env)
    }

    #[test]
    fn the_mouse_is_on_unless_the_config_or_the_flag_says_otherwise() {
        let env = Env::default();
        let mut cfg = Config::default();
        assert!(build(&[], &cfg, &env).0.mouse);
        assert!(!build(&["--no-mouse"], &cfg, &env).0.mouse);
        cfg.ui.mouse = false;
        assert!(!build(&[], &cfg, &env).0.mouse);
    }

    #[test]
    fn theme_ascii_and_colour_come_from_flags_config_and_environment() {
        let cfg = Config::default();
        let truecolor = Env { colorterm: Some("truecolor".into()), ..Env::default() };
        let (o, _) = build(&[], &cfg, &truecolor);
        assert_eq!(o.theme, Theme::recording());
        let (o, _) = build(&["--theme", "light", "--ascii"], &cfg, &truecolor);
        assert!(o.theme.ascii);
        assert_eq!(o.theme.bg, Theme::light().bg);
        // Without COLORTERM the hex colours become palette indices.
        let (o, _) = build(&[], &cfg, &Env::default());
        assert_eq!(o.theme, Theme::recording().to_256());
        let no_color = Env { no_color: Some("1".into()), ..truecolor };
        assert!(build(&[], &cfg, &no_color).0.theme.mono);
    }

    #[test]
    fn keys_prompt_and_state_file_are_settled() {
        let mut cfg = Config::default();
        cfg.keys.insert("stop_run".into(), "X".into());
        cfg.keys.insert("nonsense".into(), "z".into());
        let env = Env {
            home: Some("/home/me".into()),
            cwd: PathBuf::from("/home/me/app"),
            args: vec!["/usr/bin/aibus".into(), "--model".into(), "sonnet".into()],
            ..Env::default()
        };
        let (o, warnings) = options(&global(&[]), &cfg, &paths(), None, "", &env);
        assert_eq!(o.prompt.cwd, "~/app");
        assert_eq!(o.prompt.text, "");
        assert_eq!(o.prompt.placeholder, "aibus --model sonnet");
        assert_eq!(o.layout_file, Some(PathBuf::from("/home/me/.local/state/aibus/ui.toml")));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown action `nonsense`"), "{warnings:?}");
        assert_eq!(o.hz, 30);
    }

    #[test]
    fn a_home_prefix_only_counts_at_a_path_boundary() {
        assert_eq!(tilde(Path::new("/home/me"), Some("/home/me")), "~");
        assert_eq!(tilde(Path::new("/home/mex/app"), Some("/home/me")), "/home/mex/app");
        assert_eq!(tilde(Path::new("/srv/app"), None), "/srv/app");
    }
}
