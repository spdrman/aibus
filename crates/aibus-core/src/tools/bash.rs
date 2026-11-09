//! The `bash` tool (09 section 3): `sh -c` on the blocking pool, in its own process group so a
//! cancel reaches grandchildren, with a timeout, a tail-kept output cap and a scrubbed
//! environment. Which commands may start is decided before this runs (`ToolExecutor`); this
//! file only runs what it is given.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::lock;
use super::sandbox::Sandbox;
use crate::agent::PermissionMode;
use crate::config::SandboxKind;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

/// How often a running command is checked for exit, timeout and cancel.
const POLL: Duration = Duration::from_millis(10);
/// How long the pipes get to drain after the shell itself has exited, before a descendant that
/// still holds them is killed.
const DRAIN_GRACE: Duration = Duration::from_millis(250);
/// Room kept inside the cap for the `[... N bytes truncated ...]` line.
const MARKER_ROOM: usize = 64;

/// The environment a command (a `bash` call or a hook) starts from: `env` without the
/// `*_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD` and `AIBUS_ACCOUNT_*` variables, unless
/// `pass_env` names them (09 section 1), plus the three `AIBUS_*` variables.
pub(crate) fn scrubbed_env(
    env: &[(String, String)],
    pass_env: &[String],
    project_dir: &Path,
    session: &str,
    run: &str,
) -> Vec<(String, String)> {
    let secret = |name: &str| {
        let upper = name.to_ascii_uppercase();
        upper.starts_with("AIBUS_ACCOUNT_")
            || ["KEY", "TOKEN", "SECRET", "PASSWORD"]
                .iter()
                .any(|s| upper == *s || upper.ends_with(&format!("_{s}")))
    };
    let mut out: Vec<(String, String)> = env
        .iter()
        .filter(|(k, _)| pass_env.contains(k) || !secret(k))
        .cloned()
        .collect();
    out.push((
        "AIBUS_PROJECT_DIR".into(),
        project_dir.to_string_lossy().into_owned(),
    ));
    out.push(("AIBUS_SESSION_ID".into(), session.to_owned()));
    out.push(("AIBUS_RUN_ID".into(), run.to_owned()));
    out
}

/// The last `cap` bytes of what a command printed.
pub(crate) struct Tail {
    buf: Vec<u8>,
    cap: usize,
    dropped: u64,
}

impl Tail {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap: cap.saturating_sub(MARKER_ROOM),
            dropped: 0,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.buf.len() > self.cap.saturating_mul(2).max(self.cap + 4096) {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.buf.len() > self.cap {
            let mut cut = self.buf.len() - self.cap;
            // Do not start in the middle of a UTF-8 sequence.
            while cut < self.buf.len() && (self.buf[cut] & 0xC0) == 0x80 {
                cut += 1;
            }
            self.buf.drain(..cut);
            self.dropped += cut as u64;
        }
    }

    /// The text, behind a head marker when anything was cut. Never longer than the cap the
    /// tail was made with.
    pub(crate) fn text(&mut self) -> (String, bool) {
        self.trim();
        let body = String::from_utf8_lossy(&self.buf);
        if self.dropped == 0 {
            (body.into_owned(), false)
        } else {
            (
                format!("[... {} bytes truncated ...]\n{body}", self.dropped),
                true,
            )
        }
    }
}

enum End {
    Exited(Option<i32>),
    Timeout,
    Stopped,
}

struct Exec {
    /// The shell (or the sandbox wrapped around it), with its arguments and working directory
    /// set. The environment and the pipes are added when it starts.
    cmd: Command,
    env: Vec<(String, String)>,
    timeout: Option<Duration>,
    /// Kill what the shell left behind once it exits (a foreground call), instead of leaving
    /// it running (a background job).
    reap: bool,
}

/// Runs `x` to the end, writing output into `out`. `stop` is polled while it runs; when it
/// turns true the whole process group is killed.
fn exec(
    mut x: Exec,
    out: &Arc<Mutex<Tail>>,
    stop: &impl Fn() -> bool,
) -> std::io::Result<End> {
    let cmd = &mut x.cmd;
    cmd.env_clear()
        .envs(x.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    let pgid = child.id();

    let pipes: [Option<Box<dyn std::io::Read + Send>>; 2] = [
        child.stdout.take().map(|p| Box::new(p) as _),
        child.stderr.take().map(|p| Box::new(p) as _),
    ];
    let readers: Vec<_> = pipes
        .into_iter()
        .flatten()
        .map(|mut pipe| {
            let out = Arc::clone(out);
            thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    lock(&out).push(&chunk[..n]);
                }
            })
        })
        .collect();

    let started = Instant::now();
    let end = loop {
        if let Some(status) = child.try_wait()? {
            break End::Exited(status.code());
        }
        if stop() {
            kill_group(pgid);
            child.wait()?;
            break End::Stopped;
        }
        if x.timeout.is_some_and(|t| started.elapsed() >= t) {
            kill_group(pgid);
            child.wait()?;
            break End::Timeout;
        }
        thread::sleep(POLL);
    };

    // A descendant that outlives the shell keeps the pipes open. Foreground calls give it a
    // moment and then kill it; background jobs wait for it, or for `stop`.
    let drained = Instant::now();
    while readers.iter().any(|r| !r.is_finished()) {
        if (x.reap && drained.elapsed() >= DRAIN_GRACE) || stop() {
            kill_group(pgid);
            break;
        }
        thread::sleep(POLL);
    }
    for r in readers {
        let _ = r.join();
    }
    Ok(end)
}

/// `kill -KILL -- -<pgid>` through the shell's builtin, since this crate forbids `unsafe` and
/// so cannot call `killpg`.
pub(crate) fn kill_group(pgid: u32) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -s KILL -- -{pgid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Sets the flag when the call's future is dropped, which is how the executor ends a call on
/// cancel, so the blocking thread notices even if nobody cancelled the token.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct Job {
    out: Arc<Mutex<Tail>>,
    stop: Arc<AtomicBool>,
    /// `None` while running, then the exit code (`-1` when killed or never started).
    exit: Arc<Mutex<Option<i32>>>,
}

/// A background job as `Bash::job` reports it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct JobView {
    pub running: bool,
    pub exit_code: Option<i32>,
    pub output: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashInput {
    command: String,
    timeout: Option<u32>,
    #[serde(default)]
    background: bool,
}

pub struct Bash {
    timeout_secs: u32,
    pass_env: Vec<String>,
    /// The environment commands start from, before scrubbing.
    env: Vec<(String, String)>,
    project_dir: PathBuf,
    session: String,
    sandbox: Option<Sandbox>,
    jobs: Mutex<(u32, HashMap<String, Job>)>,
}

impl Bash {
    /// `timeout_secs` is `tools.bash_timeout_secs`. `env` is the environment commands inherit
    /// (normally `std::env::vars()`); anything secret-shaped is dropped from it per call unless
    /// named in `pass_env`.
    pub fn new(
        timeout_secs: u32,
        pass_env: Vec<String>,
        env: Vec<(String, String)>,
        project_dir: PathBuf,
        session: String,
    ) -> Self {
        Self {
            timeout_secs,
            pass_env,
            env,
            project_dir,
            session,
            sandbox: None,
            jobs: Mutex::new((0, HashMap::new())),
        }
    }

    /// Runs every command inside `sandbox` (`[tools] sandbox`). A kind that is configured but
    /// whose program is missing refuses the call; it never falls back to running outside.
    pub fn with_sandbox(mut self, sandbox: Sandbox) -> Self {
        self.sandbox = (sandbox.kind != SandboxKind::None).then_some(sandbox);
        self
    }

    /// `sh -c script` in `cwd`, wrapped in the sandbox when there is one. stderr is folded into
    /// stdout inside the shell, so the two come out in the order the command wrote them instead
    /// of whichever reader thread got scheduled first.
    fn command(&self, script: &str, cwd: &Path) -> Result<Command, ToolError> {
        let script = format!("exec 2>&1\n{script}");
        let var = |name: &str| self.env.iter().find(|(k, _)| k == name).map(|(_, v)| PathBuf::from(v));
        let mut cmd = match &self.sandbox {
            None => {
                let mut c = Command::new("sh");
                c.arg("-c").arg(script);
                c
            }
            Some(s) => {
                let path = var("PATH").unwrap_or_default();
                if !s.available(&path.to_string_lossy()) {
                    return Err(ToolError::Denied {
                        reason: format!(
                            "[tools] sandbox is {} but it is not on PATH; refusing to run outside it",
                            s.kind.as_str()
                        ),
                    });
                }
                let tmp = var("TMPDIR").unwrap_or_else(|| PathBuf::from("/tmp"));
                s.command(&script, &self.project_dir, cwd, &tmp, var("HOME").as_deref())
            }
        };
        cmd.current_dir(cwd);
        Ok(cmd)
    }

    /// State of a background job started by this tool.
    pub fn job(&self, handle: &str) -> Option<JobView> {
        let jobs = lock(&self.jobs);
        let job = jobs.1.get(handle)?;
        let exit_code = *lock(&job.exit);
        let output = lock(&job.out).text().0;
        Some(JobView {
            running: exit_code.is_none(),
            exit_code,
            output,
        })
    }

    /// Kills a background job's process group. Returns false for an unknown handle.
    pub fn stop_job(&self, handle: &str) -> bool {
        lock(&self.jobs)
            .1
            .get(handle)
            .map(|j| j.stop.store(true, Ordering::SeqCst))
            .is_some()
    }
}

impl Drop for Bash {
    fn drop(&mut self) {
        for job in lock(&self.jobs).1.values() {
            job.stop.store(true, Ordering::SeqCst);
        }
    }
}

#[async_trait::async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command with `sh -c` in the project directory. Output is \
                capped to its last 256 KiB. `timeout` is in seconds. With `background` the \
                command keeps running and a handle comes back."
                .into(),
            schema: json!({"type":"object","required":["command"],"additionalProperties":false,"properties":{
                "command":{"type":"string","minLength":1},
                "timeout":{"type":"integer","minimum":1},
                "background":{"type":"boolean"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, mode: PermissionMode) -> Need {
        if mode == PermissionMode::Bypass {
            Need::Never
        } else {
            Need::Ask
        }
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: BashInput =
            serde_json::from_value(v).map_err(|e| ToolError::Invalid(e.to_string()))?;
        if i.command.trim().is_empty() {
            return Err(ToolError::Invalid("empty command".into()));
        }
        if i.timeout == Some(0) {
            return Err(ToolError::Invalid("timeout must be at least 1 second".into()));
        }
        let env = scrubbed_env(
            &self.env,
            &self.pass_env,
            &self.project_dir,
            &self.session,
            &cx.run.to_string(),
        );
        let cmd = self.command(&i.command, &cx.cwd)?;
        let cap = cx.limits.bash_max_output_bytes;
        let out = Arc::new(Mutex::new(Tail::new(cap)));
        let stop = Arc::new(AtomicBool::new(false));

        if i.background {
            let exit = Arc::new(Mutex::new(None));
            let handle = {
                let mut jobs = lock(&self.jobs);
                jobs.0 += 1;
                let handle = format!("bg-{}", jobs.0);
                jobs.1.insert(
                    handle.clone(),
                    Job {
                        out: Arc::clone(&out),
                        stop: Arc::clone(&stop),
                        exit: Arc::clone(&exit),
                    },
                );
                handle
            };
            let timeout = i.timeout.map(|s| Duration::from_secs(u64::from(s)));
            tokio::task::spawn_blocking(move || {
                let x = Exec {
                    cmd,
                    env,
                    timeout,
                    reap: false,
                };
                let code = match exec(x, &out, &|| stop.load(Ordering::SeqCst)) {
                    Ok(End::Exited(code)) => code.unwrap_or(-1),
                    _ => -1,
                };
                *lock(&exit) = Some(code);
            });
            return Ok(ToolOutput {
                content: json!({ "handle": handle }).to_string(),
                ..Default::default()
            });
        }

        let timeout = Duration::from_secs(u64::from(i.timeout.unwrap_or(self.timeout_secs)));
        let guard = StopOnDrop(Arc::clone(&stop));
        let cancel = cx.cancel.clone();
        let tail = Arc::clone(&out);
        let end = tokio::task::spawn_blocking(move || {
            let x = Exec {
                cmd,
                env,
                timeout: Some(timeout),
                reap: true,
            };
            exec(x, &tail, &|| cancel.is_cancelled() || stop.load(Ordering::SeqCst))
        })
        .await
        .map_err(|e| ToolError::Io(std::io::Error::other(e)))??;
        drop(guard);

        let (mut content, truncated) = lock(&out).text();
        match end {
            End::Stopped => return Err(ToolError::Cancelled),
            End::Timeout => return Err(ToolError::Timeout),
            End::Exited(Some(0)) => {}
            End::Exited(Some(code)) => content.push_str(&format!("\n[exit status {code}]")),
            End::Exited(None) => content.push_str("\n[killed by a signal]"),
        }
        Ok(ToolOutput {
            content,
            truncated,
            ..Default::default()
        })
    }
}
