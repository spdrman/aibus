//! `aibus` on a terminal with no subcommand (06 section 5, Q19): the dashboard with an empty
//! prompt. The startup steps run as far as the runtime; what the terminal loop needs from the
//! config, the flags and the environment is settled here, in one function a test can call.

use std::path::{Path, PathBuf};

use aibus_core::config::Config;
use aibus_core::persist::paths::Paths;
use std::collections::HashMap;
use std::sync::Arc;

use aibus_core::{AibusError, Msg, PermissionId, Redactor, Roster, RunId, RunMail};
use aibus_tui::app::Options;
use aibus_tui::input::AccountOp;
use aibus_tui::keys::Keymap;
use aibus_tui::{Prompt, Theme};

use crate::cli::{Global, RunArgs};
use crate::launch::ChildLauncher;
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
        prompt: {
            let mut p = Prompt::default().with_text(prompt);
            p.cwd = tilde(&env.cwd, env.home.as_deref());
            p.placeholder = std::iter::once("aibus").chain(launch).collect::<Vec<_>>().join(" ");
            p.hints = cfg.ui.hints && sessions_seen(&paths.state) < aibus_tui::prompt::HINT_SESSIONS;
            p
        },
        mouse: cfg.ui.mouse && !global.no_mouse,
        term: env.term.clone(),
        layout_file: Some(paths.state.join("ui.toml")),
        log_rows: cfg.ui.log_rows,
        msgs: None,
        watch: None,
        mem: None,
        user: None,
        writer: None,
        runs_dir: None,
        agents_dir: Some(paths.config.join("agents")),
        validate: None,
        account: None,
        workflows: None,
    };
    (opts, warnings)
}

/// How many sessions have started before this one: `state/sessions_seen`, one number. The hint
/// row under the prompt is for the first five (`[ui] hints`).
fn sessions_seen(state: &Path) -> u32 {
    std::fs::read_to_string(state.join("sessions_seen")).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

fn count_session(state: &Path) {
    let n = sessions_seen(state).saturating_add(1);
    // A hint that fails to count is not worth a message.
    let _ = aibus_core::persist::fs::create_private_dir(state)
        .and_then(|()| aibus_core::persist::fs::write_private(state.join("sessions_seen"), n.to_string()));
}

/// One account operation from the Accounts screen, run by the same code as `aibus account`. It
/// returns at once: a provider verify takes seconds, and the dashboard keeps drawing meanwhile.
/// The work runs on a thread of its own (`account test` starts a runtime of its own), and what
/// it did comes back as `Msg::AccountOpDone`, after a `Msg::AccountsReloaded` when the accounts
/// changed. The key stays a `Secret` until the worker feeds it to `account add` as its stdin.
fn account_op(
    op: AccountOp,
    tx: tokio::sync::mpsc::Sender<Msg>,
    paths: &Paths,
    project_settings: &Path,
    redactor: &Redactor,
    launcher: Option<Arc<ChildLauncher>>,
) -> String {
    use crate::account::{self, Io};
    use crate::cli::{AccountAdd, AccountCmd};
    let (what, name, cmd, key) = match op {
        AccountOp::Test(name) => ("test", name.clone(), AccountCmd::Test { name }, None),
        AccountOp::Disable(name) => ("disable", name.clone(), AccountCmd::Disable { name }, None),
        AccountOp::Remove(name) => ("remove", name.clone(), AccountCmd::Remove { name }, None),
        AccountOp::Add { name, provider, key } => (
            "add",
            name.clone(),
            AccountCmd::Add(AccountAdd { name, provider, key_stdin: true, env: None, file: None, oauth: false, base_url: None, label: None, region: None, project: None }),
            Some(key),
        ),
    };
    let (paths, project_settings, redactor) = (paths.clone(), project_settings.to_path_buf(), redactor.clone());
    let shown = format!("{} {name}...", if what == "add" { "verifying" } else { what });
    std::thread::spawn(move || {
        let mut out = Vec::new();
        // The only copy of the key outside the keychain: wiped when the worker is done with it.
        let bytes = zeroize::Zeroizing::new(key.map(|k| k.expose().clone().into_bytes()).unwrap_or_default());
        let mut stdin: &[u8] = &bytes;
        let mut io = Io {
            paths: paths.clone(),
            keychain: Arc::new(crate::keychain::OsKeychain),
            factories: crate::providers::builtin_factories(),
            env: Arc::new(|v| std::env::var(v).ok()),
            redactor: redactor.clone(),
            oauth: None,
            stdin: &mut stdin,
            prompt: None,
            out: &mut out,
        };
        let ran = account::run(&cmd, false, &mut io);
        let line = String::from_utf8_lossy(&out).trim().replace('\n', "  ");
        let (ok, mut text) = match ran {
            Ok(_) if line.is_empty() => (true, format!("{what} {name}: done")),
            Ok(_) => (true, line),
            Err(e) => (false, format!("failed: {e}")),
        };
        // The accounts changed: the new set is usable by the next pick, and the screens show it.
        if ok && what != "test" {
            match crate::startup::reload_accounts(&paths, &project_settings, &crate::providers::builtin_factories()) {
                Ok((registry, accounts)) => {
                    match launcher.as_ref().filter(|l| !l.builtin) {
                        Some(l) => l.set_accounts(registry),
                        None => text.push_str(" (this session started without an account: restart to run on it)"),
                    }
                    let _ = tx.blocking_send(Msg::AccountsReloaded { accounts });
                }
                Err(e) => text.push_str(&format!(" (could not reload the accounts: {e})")),
            }
        }
        let text = redactor.redact(&text).into_owned();
        let _ = tx.blocking_send(Msg::AccountOpDone { op: what.to_owned(), account: name, ok, text });
    });
    shown
}

/// Returns the process exit code.
pub fn run(global: Global, prompt: Option<String>, redactor: Redactor) -> Result<u8, AibusError> {
    let args = RunArgs { prompt, ..RunArgs::default() };
    let mut ctx = Ctx::new(global, args, redactor);
    ctx.interactive = true;
    let started = startup::start(startup::interactive_steps(), &mut ctx)?;
    let driven = drive(&mut ctx);
    // The end of the session's continuity work (a handoff if the ledger changed, the index)
    // needs the runtime, so it comes before the steps stop.
    crate::continuity::end(&mut ctx);
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
    let (Some(rt), Some(mut app)) = (ctx.rt.as_ref(), ctx.app.take()) else {
        return Err(not_ready("runtime"));
    };
    let mut opts = opts;
    // What the runs the orchestrator starts say arrives here, and the first prompt starts the
    // lead through the launcher like any other run.
    opts.msgs = ctx.rx.take();
    if let (Some(orch), Some(launcher)) = (ctx.orch.clone(), ctx.launcher.clone()) {
        // The next `delegate` offers the new roster and starts from it.
        opts.watch = Some(Box::new(move |m| {
            if let Msg::AgentsReloaded { defs, .. } = m {
                orch.set_roster(Roster::of(defs.iter()));
                launcher.reload(defs);
            }
        }));
    }
    if ctx.global.verbose > 0 {
        opts.mem = Some(aibus_core::mem::rss_mib);
    }
    // The workflows on disk: the prompt's `/fix` family reads their names, and `/workflow list`,
    // `show` and `save` work on these directories.
    let workflows = aibus_tui::workflows::WorkflowFiles {
        dirs: aibus_core::workflow::dirs(&ctx.cwd, Some(&paths.config)),
        cap: loaded.config.limits.result_max_bytes,
        redactor: ctx.redactor.clone(),
    };
    app.step(Msg::WorkflowsLoaded { rows: aibus_core::workflow::rows(&workflows.dirs), shown: None, said: None })?;
    opts.workflows = Some(workflows);
    count_session(&paths.state);
    // Permission answers and messages reach the run that is waiting through its mailbox. The
    // map of which run asked is kept from what the runs say.
    if let Some(orch) = ctx.orch.clone() {
        let asked: Arc<std::sync::Mutex<HashMap<PermissionId, RunId>>> = Arc::default();
        let seen = asked.clone();
        let lead: Arc<std::sync::Mutex<Option<RunId>>> = Arc::default();
        let lead_seen = lead.clone();
        let mut inner = opts.watch.take();
        opts.watch = Some(Box::new(move |m| {
            if let Some(f) = inner.as_mut() {
                f(m);
            }
            match m {
                Msg::PermissionNeeded { request } => {
                    seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(request.id, request.run);
                }
                Msg::RunStarted { run, role: aibus_core::Role::Lead, .. } => {
                    *lead_seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*run);
                }
                _ => {}
            }
        }));
        let o = orch.clone();
        opts.user = Some(Box::new(move |m| match m {
            Msg::Command(aibus_core::Command::Permission { id, decision }) => {
                let run = asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id);
                if let Some(mail) = run.and_then(|r| o.mailbox(r)) {
                    let _ = mail.try_send(RunMail::Permission { id: *id, decision: *decision });
                }
            }
            Msg::Command(aibus_core::Command::SendMessage { run, text }) => {
                let _ = o.message(*run, text.clone());
            }
            // `/workflow run <name> <input>` and the shorthands: the lead makes the `workflow`
            // call at its next turn boundary, so the steps are cards under it.
            Msg::Command(aibus_core::Command::Workflow { name, input }) => {
                let run = *lead.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(mail) = run.and_then(|r| o.mailbox(r)) {
                    let call = serde_json::json!({"name": name, "input": input});
                    let _ = mail.try_send(RunMail::Call { tool: "workflow".into(), input: call });
                }
            }
            // A prompt typed while the lead runs is a message to it; slash commands are not.
            Msg::SubmitPrompt { text } if !text.starts_with('/') => {
                let run = *lead.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(run) = run.filter(|r| o.is_live(*r)) {
                    let _ = o.message(run, text.clone());
                }
            }
            _ => {}
        }));
    }
    opts.writer = ctx.writer.take();
    opts.runs_dir = ctx.session.as_ref().map(|s| s.dir.join("runs"));
    let (g, r) = (paths.clone(), ctx.redactor.clone());
    let project_settings = aibus_core::persist::paths::project_dir(&ctx.cwd).join("settings.toml");
    let launcher = ctx.launcher.clone();
    opts.account = Some(Box::new(move |op, tx| account_op(op, tx, &g, &project_settings, &r, launcher.clone())));
    let ran = rt.block_on(aibus_tui::app::run(app, opts));
    if let Some(orch) = &ctx.orch {
        orch.cancel_all();
    }
    ran
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
    fn the_log_height_and_the_hooks_come_from_config() {
        let mut cfg = Config::default();
        cfg.ui.log_rows = 7;
        let (o, _) = build(&[], &cfg, &Env::default());
        assert_eq!(o.log_rows, 7);
        assert!(o.msgs.is_none() && o.watch.is_none() && o.mem.is_none());
    }

    #[test]
    fn a_home_prefix_only_counts_at_a_path_boundary() {
        assert_eq!(tilde(Path::new("/home/me"), Some("/home/me")), "~");
        assert_eq!(tilde(Path::new("/home/mex/app"), Some("/home/me")), "/home/mex/app");
        assert_eq!(tilde(Path::new("/srv/app"), None), "/srv/app");
    }
}
