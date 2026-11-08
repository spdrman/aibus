//! `command` hooks (01 section 6): shell commands that get one JSON object on stdin and can
//! block by exit code. The runner is built once per session from `[hooks]`; a definition's own
//! hooks come in with each call, so they only ever run for that agent's runs.

mod exec;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent::{HookSet, HookSpec, PermissionMode, Role};
use crate::config;
use crate::ids::RunId;
use crate::msg::{HookEvent, HookOutcome};
use crate::persist::fs::{create_private_dir, write_private};
use crate::redact::Redactor;
use crate::tools::scrubbed_env;

/// What a hook is told. The fields every event carries (01 section 6.2) are named; the
/// per-event extras (`tool_name`, `last_assistant_message`, `trigger`, ...) go in `extra`.
pub struct HookInput<'a> {
    pub event: HookEvent,
    /// What a hook's `matcher` is tested against: the tool name, the agent name, the trigger.
    /// `None` for the events that have no matcher input, which run every hook of the event.
    pub matcher: Option<&'a str>,
    pub run: Option<RunId>,
    pub agent: &'a str,
    pub role: Role,
    pub cwd: &'a Path,
    pub transcript_path: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    pub extra: Map<String, Value>,
}

/// The hook that stopped the action, with the reason that goes back to the model.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Blocked {
    pub hook: String,
    pub reason: String,
}

/// One hook that ran, for the `HookFired` message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ran {
    pub name: String,
    pub outcome: HookOutcome,
}

/// Everything the hooks of one event said.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Fired {
    pub ran: Vec<Ran>,
    /// The first hook that blocked; later hooks of the event did not run.
    pub block: Option<Blocked>,
    /// A hook answered `"decision": "ask"`: the user has to confirm even if the mode would not.
    pub ask: bool,
    /// The last `updated_input` a `pre_tool_use` hook returned; it replaced the tool arguments
    /// for the hooks after it as well.
    pub updated_input: Option<Value>,
    /// `[hook <name>]` plus the plain stdout of a hook on `user_prompt_submit` or
    /// `session_start`.
    pub context: Vec<String>,
    /// stderr of hooks that exited 2 on an event that cannot block; `post_tool_use` shows it to
    /// the model.
    pub notes: Vec<String>,
}

/// Runs the hooks of `[hooks]` plus the ones a definition brings. `Default` runs nothing, which
/// is what `--no-hooks` gives.
#[derive(Default)]
pub struct HookRunner {
    enabled: bool,
    global: BTreeMap<String, Vec<HookSpec>>,
    /// The environment hooks start from, before scrubbing.
    env: Vec<(String, String)>,
    pass_env: Vec<String>,
    project_dir: PathBuf,
    session: String,
    /// `logs/hooks/<session>`; every run of a hook leaves a file here.
    log_dir: Option<PathBuf>,
    redactor: Option<Redactor>,
    seq: AtomicU64,
}

impl HookRunner {
    pub fn new(
        hooks: &config::Hooks,
        env: Vec<(String, String)>,
        project_dir: PathBuf,
        session: String,
        log_dir: Option<PathBuf>,
        redactor: Option<Redactor>,
    ) -> Self {
        let global = hooks
            .events
            .iter()
            .map(|(event, list)| {
                let specs = list
                    .iter()
                    .map(|h| HookSpec {
                        matcher: h.matcher.clone(),
                        command: h.command.clone(),
                        timeout_secs: h.timeout,
                    })
                    .collect();
                (event.clone(), specs)
            })
            .collect();
        Self {
            enabled: true,
            global,
            env,
            pass_env: hooks.pass_env.clone(),
            project_dir,
            session,
            log_dir,
            redactor,
            seq: AtomicU64::new(0),
        }
    }

    /// Runs the hooks for `input.event`, one after the other: `[hooks]` first, then the
    /// definition's. A hook that blocks ends the chain. Cancelling `cancel` kills the hook that
    /// is running and skips the rest.
    pub async fn fire(
        &self,
        input: &HookInput<'_>,
        scoped: &HookSet,
        cancel: &CancellationToken,
    ) -> Fired {
        // 01 section 6.3: a hook gets a minute unless it says otherwise, and the user is waiting
        // on `user_prompt_submit`, so that one never gets more than ten seconds.
        const DEFAULT_TIMEOUT_SECS: u32 = 60;
        const PROMPT_TIMEOUT_CAP_SECS: u32 = 10;
        // Exit code recorded for a hook that timed out (what `timeout(1)` uses).
        const TIMED_OUT: i32 = 124;

        let mut fired = Fired::default();
        if !self.enabled {
            return fired;
        }
        let event = input.event;
        // `matcher` against the event's matcher input: no matcher or `*` takes everything,
        // otherwise `a|b` alternatives, each a name or a glob. An event without matcher input
        // runs every hook.
        let wanted = |matcher: Option<&str>| {
            let (Some(m), Some(input)) = (matcher.map(str::trim), input.matcher) else {
                return true;
            };
            m.is_empty()
                || m == "*"
                || m.split('|').map(str::trim).any(|alt| {
                    alt == input
                        || globset::Glob::new(alt)
                            .is_ok_and(|g| g.compile_matcher().is_match(input))
                })
        };
        // A worker's `stop` is delivered as `subagent_stop` (01 section 6.1).
        let alias = (event == HookEvent::SubagentStop && input.role == Role::Worker)
            .then_some("stop");
        let specs: Vec<&HookSpec> = self
            .global
            .get(event.as_str())
            .into_iter()
            .chain(scoped.get(event.as_str()))
            .chain(alias.and_then(|a| scoped.get(a)))
            .flatten()
            .filter(|h| wanted(h.matcher.as_deref()))
            .collect();

        let mut extra = input.extra.clone();
        for spec in specs {
            if cancel.is_cancelled() {
                break;
            }
            // What the log and the notices call a hook: the file name of the command's first word.
            let name = {
                let first = spec.command.split_whitespace().next().unwrap_or(&spec.command);
                Path::new(first)
                    .file_name()
                    .map_or(first, |f| f.to_str().unwrap_or(first))
                    .to_owned()
            };
            let mut secs = spec.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).max(1);
            if event == HookEvent::UserPromptSubmit {
                secs = secs.min(PROMPT_TIMEOUT_CAP_SECS);
            }
            let body = self.stdin(input, &extra);
            let job = exec::Job {
                command: spec.command.clone(),
                cwd: self.project_dir.clone(),
                env: scrubbed_env(
                    &self.env,
                    &self.pass_env,
                    &self.project_dir,
                    &self.session,
                    &input.run.map(|r| r.to_string()).unwrap_or_default(),
                ),
                stdin: body.clone().into_bytes(),
            };
            let _span = tracing::trace_span!(
                target: "aibus_trace", "hook", event = event.as_str(), hook = name.as_str()
            );
            let done = exec::run(job, Duration::from_secs(u64::from(secs)), cancel).await;
            self.log(event, &spec.command, &body, done.as_ref().ok()).await;

            let (code, stdout, stderr) = match done {
                Err(e) => {
                    let message = format!("hook error: {name}");
                    tracing::warn!("{message} (could not start: {e})");
                    if event.can_block() {
                        fired.block = Some(Blocked {
                            hook: name.clone(),
                            reason: format!("hook {name} could not start: {e}"),
                        });
                    }
                    fired.ran.push(Ran {
                        name,
                        outcome: HookOutcome {
                            exit_code: -1,
                            message: Some(message),
                        },
                    });
                    if fired.block.is_some() {
                        break;
                    }
                    continue;
                }
                Ok(d) => match d.ended {
                    exec::Ended::Cancelled => break,
                    exec::Ended::TimedOut => {
                        let message = format!("hook error: {name} (timed out after {secs} s)");
                        tracing::warn!("{message}");
                        if event.can_block() {
                            fired.block = Some(Blocked {
                                hook: name.clone(),
                                reason: format!("hook {name} timed out after {secs} s"),
                            });
                        }
                        fired.ran.push(Ran {
                            name,
                            outcome: HookOutcome {
                                exit_code: TIMED_OUT,
                                message: Some(message),
                            },
                        });
                        if fired.block.is_some() {
                            break;
                        }
                        continue;
                    }
                    exec::Ended::Exited(code) => {
                        (code.unwrap_or(-1), d.stdout, d.stderr)
                    }
                },
            };

            let mut outcome = HookOutcome {
                exit_code: code,
                message: None,
            };
            match code {
                0 => self.accept(event, &name, &stdout, &mut fired, &mut extra),
                2 => {
                    // What goes back to the model is scrubbed; a verdict's `updated_input` is
                    // not, since it is what will run.
                    let reason = match stderr.trim() {
                        "" => format!("blocked by hook {name}"),
                        s => self.redact(s).into_owned(),
                    };
                    outcome.message = Some(reason.clone());
                    if event.can_block() {
                        fired.block = Some(Blocked { hook: name.clone(), reason });
                    } else if !stderr.trim().is_empty() {
                        fired.notes.push(format!("[hook {name}] {reason}"));
                    }
                }
                _ => {
                    tracing::warn!("hook error: {name} (exit {code})");
                    outcome.message = Some(format!("hook error: {name}"));
                }
            }
            if let Some(b) = fired.block.as_ref().filter(|b| b.hook == name) {
                outcome.message = Some(b.reason.clone());
            }
            fired.ran.push(Ran { name, outcome });
            if fired.block.is_some() {
                break;
            }
        }
        fired
    }

    /// Exit 0: one JSON object is a verdict, anything else is context on the two events that
    /// take it.
    fn accept(
        &self,
        event: HookEvent,
        name: &str,
        stdout: &str,
        fired: &mut Fired,
        extra: &mut Map<String, Value>,
    ) {
        let text = stdout.trim();
        if text.is_empty() {
            return;
        }
        if let Ok(Value::Object(verdict)) = serde_json::from_str::<Value>(text) {
            let reason = || {
                verdict.get("reason").and_then(Value::as_str).map_or_else(
                    || format!("blocked by hook {name}"),
                    |r| self.redact(r).into_owned(),
                )
            };
            match verdict.get("decision").and_then(Value::as_str) {
                Some("deny" | "block") if event.can_block() => {
                    fired.block = Some(Blocked {
                        hook: name.to_owned(),
                        reason: reason(),
                    });
                }
                Some("ask") if event == HookEvent::PreToolUse => fired.ask = true,
                _ => {}
            }
            if event == HookEvent::PreToolUse {
                if let Some(input @ Value::Object(_)) = verdict.get("updated_input") {
                    extra.insert("tool_input".into(), input.clone());
                    fired.updated_input = Some(input.clone());
                }
            }
        } else if matches!(event, HookEvent::UserPromptSubmit | HookEvent::SessionStart) {
            fired
                .context
                .push(self.redact(&format!("[hook {name}]\n{text}")).into_owned());
        }
    }

    /// `text` with secrets masked, when the runner was given a redactor.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match &self.redactor {
            Some(r) => r.redact(text),
            None => Cow::Borrowed(text),
        }
    }

    /// The JSON a hook reads on stdin (01 section 6.2), with secrets scrubbed.
    fn stdin(&self, input: &HookInput<'_>, extra: &Map<String, Value>) -> String {
        let mut body = json!({
            "session_id": self.session,
            "run_id": input.run.map(|r| r.to_string()),
            "agent_type": input.agent,
            "role": input.role.as_str(),
            "cwd": input.cwd,
            "transcript_path": input.transcript_path,
            "permission_mode": input.permission_mode.as_str(),
            "hook_event_name": input.event.as_str(),
        });
        if let Some(map) = body.as_object_mut() {
            map.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        if let Some(r) = &self.redactor {
            r.redact_json(&mut body);
        }
        body.to_string()
    }

    /// `logs/hooks/<session>/<event>-<ts>.log`: what the hook was given and what it printed.
    async fn log(&self, event: HookEvent, command: &str, stdin: &str, done: Option<&exec::Done>) {
        let Some(dir) = self.log_dir.clone() else {
            return;
        };
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{}-{at}-{seq}.log", event.as_str()));
        let status = match done.map(|d| d.ended) {
            Some(exec::Ended::Exited(Some(c))) => format!("exit {c}"),
            Some(exec::Ended::Exited(None)) => "killed by a signal".into(),
            Some(exec::Ended::TimedOut) => "timed out".into(),
            Some(exec::Ended::Cancelled) => "cancelled".into(),
            None => "did not start".into(),
        };
        let (out, err, ms) = done.map_or((String::new(), String::new(), 0), |d| {
            (d.stdout.clone(), d.stderr.clone(), d.elapsed.as_millis())
        });
        let text = self
            .redact(&format!(
                "$ {command}\n{status} after {ms} ms\n--- stdin ---\n{stdin}\n--- stdout ---\n{out}\n--- stderr ---\n{err}\n"
            ))
            .into_owned();
        let written = tokio::task::spawn_blocking(move || {
            create_private_dir(&dir)?;
            write_private(path, text)
        })
        .await;
        if let Ok(Err(e)) | Err(e) = written.map_err(std::io::Error::other) {
            tracing::warn!("hook log not written: {e}");
        }
    }
}

