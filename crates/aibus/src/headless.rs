//! `aibus run`: the startup steps, then the headless loop (02 section 7 step 8). Each `Msg` the
//! lead run sends goes through the Elm loop and out as one NDJSON line with `--json` (06 section
//! 2), the last being `SessionEnd` with the exit code, or as plain text without it.

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::time::{Duration, SystemTime};

use aibus_core::persist::fs::{create_private_dir, write_private};
use aibus_core::replay::{msg_to_json, replay_hash};
use aibus_core::{
    AibusError, CredentialError, Decision, Msg, PermissionId, Redactor, RunError, RunErrorKind,
    RunMail, RunResult, RunStatus,
};
use aibus_graph::Graph;
use aibus_provider::{ProviderError, ProviderErrorKind};
use tokio::time::Instant;

use crate::cli::{Global, RunArgs};
use crate::control::{Control, Line};
use crate::events::{self, Events};
use crate::plain::Plain;
use crate::signals::Signal;
use crate::startup::{self, not_ready, Ctx};
use crate::term;

/// Returns the process exit code.
pub fn run(global: Global, args: RunArgs, redactor: Redactor) -> Result<u8, AibusError> {
    let json = global.json;
    let mut ctx = Ctx::new(global, args, redactor);
    let started = startup::start(startup::steps(), &mut ctx)?;
    let driven = drive(&mut ctx);
    let verdict = verdict(ctx.finished.as_ref(), ctx.interrupted);
    let code = verdict.as_ref().map_or_else(AibusError::exit_code, |_| 0);
    let session = ctx.session.as_ref().map(|s| s.id.to_string());
    let ended = session.filter(|_| json).map(|id| {
        // The last line of a session; `handoff` names the file once handoffs are written.
        serde_json::json!({
            "type": "SessionEnd",
            "t": events::rfc3339(SystemTime::now()),
            "session": id,
            "exit_code": code,
            "handoff": null,
        })
    });
    let stopped = startup::stop(started, &mut ctx);
    if let Some(line) = ended {
        // After the steps are stopped, so the run task has flushed everything it was going to.
        let mut line = line;
        ctx.redactor.redact_json(&mut line);
        let _ = writeln!(std::io::stdout().lock(), "{line}");
    }
    driven?;
    verdict?;
    stopped?;
    Ok(0)
}

/// How a finished (or interrupted) lead run maps to an error and so to an exit code
/// (06 section 3). `Ok` is a run that ended `Completed`.
pub fn verdict(result: Option<&RunResult>, interrupted: bool) -> Result<(), AibusError> {
    let Some(r) = result else {
        return Err(if interrupted {
            AibusError::Interrupted
        } else {
            RunError {
                kind: RunErrorKind::Internal,
                message: "the run ended without a result".into(),
                retryable: false,
            }
            .into()
        });
    };
    let failure = |fallback: &str| {
        r.error.clone().unwrap_or(RunError {
            kind: RunErrorKind::Internal,
            message: fallback.into(),
            retryable: false,
        })
    };
    match r.status {
        RunStatus::Completed => Ok(()),
        RunStatus::Cancelled if interrupted => Err(AibusError::Interrupted),
        RunStatus::Cancelled => Err(failure("cancelled").into()),
        // A run stopped by its token or cost budget keeps its text (`Partial`) but exits 8.
        RunStatus::Partial if r.error.as_ref().is_some_and(|e| e.kind == RunErrorKind::Budget) => {
            Err(AibusError::Budget(failure("budget exceeded").message))
        }
        // A rejected credential is exit 4 even when the run kept some text.
        RunStatus::Partial if r.error.as_ref().is_some_and(|e| e.kind == RunErrorKind::Auth) => {
            Err(CredentialError::RunRejected(failure("credential rejected").message).into())
        }
        RunStatus::Partial => Err(failure("ended with a partial result").into()),
        RunStatus::Denied => Err(AibusError::Denied(
            r.error.as_ref().map_or("run denied".into(), |e| e.message.clone()),
        )),
        RunStatus::Failed => {
            let e = failure("failed");
            Err(match e.kind {
                RunErrorKind::Provider => {
                    ProviderError::new(ProviderErrorKind::Unknown, e.message).into()
                }
                RunErrorKind::Auth => CredentialError::RunRejected(e.message).into(),
                RunErrorKind::Budget => AibusError::Budget(e.message),
                RunErrorKind::Denied => AibusError::Denied(e.message),
                RunErrorKind::Tool | RunErrorKind::Internal => e.into(),
            })
        }
        _ => Err(failure("the run did not reach a final state").into()),
    }
}

/// What the loop does with each `Msg`: print it, remember it for `--record`, keep track of the
/// permission prompts that are open, and commit it. These vary together per message.
struct Pump<'a> {
    json: bool,
    events: Events<'a>,
    plain: Plain<'a>,
    out_color: bool,
    recorded: String,
    finished: Option<RunResult>,
    /// Asked for and not yet answered.
    pending: HashSet<PermissionId>,
    /// Denied by us because nobody is left to answer (stdin closed).
    auto_denied: HashSet<PermissionId>,
}

impl Pump<'_> {
    fn handle(
        &mut self,
        mut msg: Msg,
        redactor: &Redactor,
        app: &mut aibus_core::App<aibus_graph::mini::MiniGraph>,
    ) -> Result<(), AibusError> {
        match &mut msg {
            Msg::PermissionNeeded { request } => {
                self.pending.insert(request.id);
            }
            Msg::PermissionDecided { id, reason, .. } => {
                self.pending.remove(id);
                if self.auto_denied.remove(id) {
                    *reason = Some("no tty".into());
                }
            }
            _ => {}
        }
        if self.json {
            let mut out = std::io::stdout().lock();
            for line in self.events.event(&msg, SystemTime::now()) {
                writeln!(out, "{line}")?;
            }
            out.flush()?;
        } else {
            self.plain.show(&msg, self.out_color)?;
        }
        if let Some(mut line) = msg_to_json(&msg) {
            redactor.redact_json(&mut line);
            self.recorded.push_str(&line.to_string());
            self.recorded.push('\n');
        }
        if let Msg::RunFinished { result, .. } = &msg {
            self.finished = Some(result.clone());
        }
        app.step(msg)
    }
}

/// Prints the run's events until it finishes, or until a signal asks to stop. The first signal
/// cancels the run and gives it `GRACE` to wind down; a second one within `FORCE_WITHIN` of the
/// first stops waiting at once. SIGWINCH is a `Resize`. With `--json` the control channel on
/// stdin answers permission prompts and mails messages to the run.
fn drive(ctx: &mut Ctx) -> Result<(), AibusError> {
    let (Some(rt), Some(app), Some(rx), Some(handle), Some(signals), Some(loaded)) = (
        ctx.rt.as_ref(),
        ctx.app.as_mut(),
        ctx.rx.as_mut(),
        ctx.handle.as_mut(),
        ctx.signals.as_mut(),
        ctx.loaded.as_ref(),
    ) else {
        return Err(not_ready("run"));
    };
    let redactor = &ctx.redactor;
    // A second signal this soon after the first is a forced quit (06 section 5); a graceful
    // stop may take this long before we give up on the run.
    const FORCE_WITHIN: Duration = Duration::from_secs(2);
    const GRACE: Duration = Duration::from_secs(10);

    let env_no_color = std::env::var("NO_COLOR").ok();
    let term_var = std::env::var("TERM").ok();
    let color = |is_tty: bool| term::use_color(is_tty, env_no_color.as_deref(), term_var.as_deref());
    let mut pump = Pump {
        json: ctx.global.json,
        events: Events::new(redactor, loaded.config.limits.result_max_bytes),
        plain: Plain::new(redactor, color(std::io::stderr().is_terminal())),
        out_color: color(std::io::stdout().is_terminal()),
        recorded: String::new(),
        finished: None,
        pending: HashSet::new(),
        auto_denied: HashSet::new(),
    };
    let mut control = ctx.control.take();
    // Without a channel the run refuses prompts itself ("no tty"); with one, a closed stdin
    // means nobody is left to answer, which we do for it.
    let has_control = control.is_some();
    let mut control_open = has_control;
    let mailbox = handle.mailbox.clone();
    let deny = |id: PermissionId| {
        mailbox.try_send(RunMail::Permission {
            id,
            decision: Decision::Deny,
        })
    };

    let interrupted = rt.block_on(async {
        let mut interrupted = false;
        let mut first_signal: Option<Instant> = None;
        let mut deadline: Option<Instant> = None;
        loop {
            let timeout = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            let next_control = async {
                match control.as_mut() {
                    Some(c) => c.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    let asked = match &msg {
                        Msg::PermissionNeeded { request } if has_control && !control_open => {
                            Some(request.id)
                        }
                        _ => None,
                    };
                    pump.handle(msg, redactor, app)?;
                    if let Some(id) = asked {
                        if deny(id).is_ok() {
                            pump.auto_denied.insert(id);
                        }
                    }
                    if pump.finished.is_some() {
                        break;
                    }
                }
                sig = signals.recv() => match sig {
                    Signal::Resize => {
                        let (cols, rows) = term::size();
                        pump.handle(Msg::Resize { cols, rows }, redactor, app)?;
                    }
                    Signal::Stop => {
                        let now = Instant::now();
                        interrupted = true;
                        if first_signal.is_some_and(|f| now.duration_since(f) < FORCE_WITHIN) {
                            break;
                        }
                        first_signal = Some(now);
                        deadline = Some(now + GRACE);
                        handle.cancel.cancel();
                    }
                },
                line = next_control, if control_open => match line {
                    Some(Line::Ok(Control::Permission { id, decision })) => {
                        let sent = mailbox.try_send(RunMail::Permission { id, decision });
                        if sent.is_err() {
                            eprintln!("aibus: control: the run is not taking messages");
                        }
                    }
                    Some(Line::Ok(Control::SendMessage { run, text })) => {
                        if run != handle.id {
                            eprintln!("aibus: control: no run {run} (the lead is {})", handle.id);
                        } else if mailbox.try_send(RunMail::UserMessage(text)).is_err() {
                            eprintln!("aibus: control: the run is not taking messages");
                        }
                    }
                    Some(Line::Bad(why)) => eprintln!("aibus: control: {why}"),
                    Some(Line::Eof) | None => {
                        control_open = false;
                        for id in pump.pending.iter().copied().collect::<Vec<_>>() {
                            if deny(id).is_ok() {
                                pump.auto_denied.insert(id);
                            }
                        }
                    }
                },
                () = timeout => break,
            }
        }
        Ok::<_, AibusError>(interrupted)
    })?;
    let _ = std::io::stdout().flush();

    let Pump {
        recorded, finished, ..
    } = pump;
    if finished.is_none() {
        handle.join.abort();
    }
    let _ = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), &mut handle.join).await
    });
    ctx.finished = finished;
    ctx.interrupted = interrupted;

    // The commit log, for `aibus session log` (06 section 4): one line per commit.
    if let Some(session) = &ctx.session {
        let mut log = String::new();
        for c in app.graph.commit_log() {
            let line = serde_json::json!({
                "time": c.time,
                "intent": c.intent.as_str(),
                "changed": c.changed_nodes.len(),
                "at": events::rfc3339(c.at),
            });
            log.push_str(&line.to_string());
            log.push('\n');
        }
        write_private(session.dir.join("commits.jsonl"), log)?;
    }

    if let (Some(dir), Some(_)) = (&ctx.run.record, &ctx.finished) {
        create_private_dir(dir)?;
        write_private(dir.join("msgs.jsonl"), &recorded)?;
        if let Some(accounts) = &ctx.accounts {
            write_private(dir.join("script.ron"), &accounts.script_text)?;
        }
        let hash = replay_hash(&recorded).map_err(AibusError::Usage)?;
        write_private(dir.join("snapshot.hash"), format!("{hash}\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_core::{AgentName, RunId};
    use aibus_provider::Usage;

    fn result(status: RunStatus, error: Option<RunError>) -> RunResult {
        RunResult {
            run_id: RunId::new(),
            agent: "lead".parse::<AgentName>().unwrap(),
            status,
            turns: 1,
            usage: Usage::default(),
            summary: String::new(),
            summary_file: None,
            duration: Duration::ZERO,
            error,
        }
    }

    fn err(kind: RunErrorKind) -> Option<RunError> {
        Some(RunError {
            kind,
            message: "m".into(),
            retryable: false,
        })
    }

    fn code(r: Option<&RunResult>, interrupted: bool) -> u8 {
        verdict(r, interrupted).map_or_else(|e| e.exit_code(), |()| 0)
    }

    #[test]
    fn exit_codes_follow_06_section_3() {
        let c = |s, e| code(Some(&result(s, e)), false);
        assert_eq!(c(RunStatus::Completed, None), 0);
        assert_eq!(c(RunStatus::Failed, err(RunErrorKind::Provider)), 5);
        assert_eq!(c(RunStatus::Failed, err(RunErrorKind::Auth)), 4);
        assert_eq!(c(RunStatus::Partial, err(RunErrorKind::Auth)), 4);
        assert_eq!(c(RunStatus::Denied, None), 6);
        assert_eq!(c(RunStatus::Failed, err(RunErrorKind::Denied)), 6);
        assert_eq!(c(RunStatus::Failed, err(RunErrorKind::Budget)), 8);
        assert_eq!(c(RunStatus::Failed, err(RunErrorKind::Tool)), 10);
        assert_eq!(c(RunStatus::Failed, err(RunErrorKind::Internal)), 10);
        assert_eq!(c(RunStatus::Partial, err(RunErrorKind::Provider)), 10);
        assert_eq!(c(RunStatus::Partial, err(RunErrorKind::Budget)), 8);
        assert_eq!(c(RunStatus::Partial, None), 10);
        assert_eq!(c(RunStatus::Cancelled, None), 10);
    }

    #[test]
    fn a_signal_makes_it_7_unless_the_run_still_completed() {
        assert_eq!(code(Some(&result(RunStatus::Cancelled, None)), true), 7);
        assert_eq!(code(None, true), 7);
        assert_eq!(code(None, false), 10);
        assert_eq!(code(Some(&result(RunStatus::Completed, None)), true), 0);
    }
}
