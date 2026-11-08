//! `aibus run`: the startup steps, then the headless loop (02 section 7 step 8). Each `Msg` the
//! lead run sends goes through the Elm loop and out as one NDJSON line (06 section 2); the last
//! line is `SessionEnd` with the exit code.

use std::io::Write;
use std::time::{Duration, SystemTime};

use aibus_core::persist::fs::{create_private_dir, write_private};
use aibus_core::replay::{msg_to_json, replay_hash};
use aibus_core::{AibusError, Msg, Redactor, Roster, RunError, RunErrorKind, RunResult, RunStatus};
use aibus_provider::{ProviderError, ProviderErrorKind};
use tokio::time::Instant;

use crate::cli::{Global, RunArgs};
use crate::events;
use crate::startup::{self, not_ready, Ctx};

/// Returns the process exit code.
pub fn run(global: Global, args: RunArgs, redactor: Redactor) -> Result<u8, AibusError> {
    let mut ctx = Ctx::new(global, args, redactor);
    let started = startup::start(startup::steps(), &mut ctx)?;
    let driven = drive(&mut ctx);
    let verdict = verdict(ctx.finished.as_ref(), ctx.interrupted);
    let code = verdict.as_ref().map_or_else(AibusError::exit_code, |_| 0);
    let session = ctx.session.as_ref().map(|s| s.id.to_string());
    let ended = session.map(|id| {
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
                RunErrorKind::Budget => AibusError::Budget(e.message),
                RunErrorKind::Denied => AibusError::Denied(e.message),
                RunErrorKind::Tool | RunErrorKind::Internal => e.into(),
            })
        }
        _ => Err(failure("the run did not reach a final state").into()),
    }
}

/// Prints the run's events until it finishes, or until a signal asks to stop. The first signal
/// cancels the run and gives it `GRACE` to wind down; a second one within `FORCE_WITHIN` of the
/// first stops waiting at once.
fn drive(ctx: &mut Ctx) -> Result<(), AibusError> {
    let (Some(rt), Some(app), Some(rx), Some(handle), Some(signals)) = (
        ctx.rt.as_ref(),
        ctx.app.as_mut(),
        ctx.rx.as_mut(),
        ctx.handle.as_mut(),
        ctx.signals.as_mut(),
    ) else {
        return Err(not_ready("run"));
    };
    let redactor = &ctx.redactor;
    let (orch, launcher) = (ctx.orch.as_ref(), ctx.launcher.as_ref());
    // A second signal this soon after the first is a forced quit (06 section 5); a graceful
    // stop may take this long before we give up on the run.
    const FORCE_WITHIN: Duration = Duration::from_secs(2);
    const GRACE: Duration = Duration::from_secs(10);

    let (finished, interrupted, recorded) = rt.block_on(async {
        let mut out = std::io::stdout().lock();
        let mut recorded = String::new();
        let mut finished = None;
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
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    writeln!(out, "{}", events::event(&msg, SystemTime::now(), redactor))?;
                    if let Some(mut line) = msg_to_json(&msg) {
                        redactor.redact_json(&mut line);
                        recorded.push_str(&line.to_string());
                        recorded.push('\n');
                    }
                    // Only the lead's end ends the session; its children report through it.
                    let done = match &msg {
                        Msg::RunFinished { run, result } if *run == handle.id => {
                            Some(result.clone())
                        }
                        Msg::AgentsReloaded { defs, .. } => {
                            // The next `delegate` offers the new roster and starts from it.
                            if let (Some(o), Some(l)) = (orch, launcher) {
                                o.set_roster(Roster::of(defs.iter()));
                                l.reload(defs);
                            }
                            None
                        }
                        _ => None,
                    };
                    app.step(msg)?;
                    if done.is_some() {
                        finished = done;
                        break;
                    }
                }
                () = signals.recv() => {
                    let now = Instant::now();
                    interrupted = true;
                    if first_signal.is_some_and(|f| now.duration_since(f) < FORCE_WITHIN) {
                        break;
                    }
                    first_signal = Some(now);
                    deadline = Some(now + GRACE);
                    handle.cancel.cancel();
                }
                () = timeout => break,
            }
        }
        out.flush()?;
        Ok::<_, AibusError>((finished, interrupted, recorded))
    })?;

    if finished.is_none() {
        handle.join.abort();
    }
    let _ = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), &mut handle.join).await
    });
    ctx.finished = finished;
    ctx.interrupted = interrupted;

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
