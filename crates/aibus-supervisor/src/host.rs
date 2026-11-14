//! The process host: one attempt from spawn to exit.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use aibus_trace::{AttemptStatus, StoreError, TraceStore};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::claude::Channel;
use crate::{Ending, Limits, Normalizer};

/// Everything needed to start the CLI. `exe` must be absolute (from `cli::discover`), `cwd` an
/// existing directory, and `env` is the whole environment (from `cli::bounded_env`).
#[derive(Clone, Debug)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Written to stdin, which is then closed (the prompt).
    pub stdin: Vec<u8>,
    pub limits: Limits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ending: Ending,
    pub status: AttemptStatus,
}

#[derive(Debug)]
pub enum SupervisorError {
    /// The launch was refused before anything ran.
    Refused(String),
    Spawn(std::io::Error),
    Store(StoreError),
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "launch refused: {why}"),
            Self::Spawn(e) => write!(f, "could not start the CLI: {e}"),
            Self::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<StoreError> for SupervisorError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// What the pipe readers send the loop. The channel is bounded, so a slow loop stops the readers
/// and the full pipe stops the CLI: nothing is dropped to keep up.
enum Piece {
    Line(Channel, Vec<u8>),
    TooLong(Channel, u64),
    /// stdout passed `max_stdout_bytes`; the reader has stopped.
    OutputLimit(u64),
    Eof,
}

/// Splits a pipe into lines of at most `max_line` bytes. A longer line is skipped to its newline
/// and reported by size; stdout stops at `max_total`, stderr past it is drained and only counted.
async fn read_lines(
    mut pipe: impl AsyncRead + Unpin,
    ch: Channel,
    max_line: usize,
    max_total: u64,
    tx: mpsc::Sender<Piece>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut line: Vec<u8> = Vec::new();
    let mut skipping: Option<u64> = None;
    let mut total = 0u64;
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        total += n as u64;
        if total > max_total {
            if ch == Channel::Stdout {
                let _ = tx.send(Piece::OutputLimit(total)).await;
                return;
            }
            continue;
        }
        for chunk in buf[..n].split_inclusive(|b| *b == b'\n') {
            let ends = chunk.ends_with(b"\n");
            let body = if ends {
                &chunk[..chunk.len() - 1]
            } else {
                chunk
            };
            if let Some(skipped) = skipping.as_mut() {
                *skipped += body.len() as u64;
            } else if line.len() + body.len() > max_line {
                skipping = Some((line.len() + body.len()) as u64);
                line = Vec::new();
            } else {
                line.extend_from_slice(body);
            }
            if ends {
                let piece = match skipping.take() {
                    Some(bytes) => Piece::TooLong(ch, bytes),
                    None => Piece::Line(ch, std::mem::take(&mut line)),
                };
                if tx.send(piece).await.is_err() {
                    return;
                }
            }
        }
    }
    let last = match skipping {
        Some(bytes) => Some(Piece::TooLong(ch, bytes)),
        None => (!line.is_empty()).then_some(Piece::Line(ch, line)),
    };
    if let Some(p) = last {
        let _ = tx.send(p).await;
    }
    let _ = tx.send(Piece::Eof).await;
}

/// Sends `sig` to the whole process group the CLI leads.
#[cfg(unix)]
fn signal_group(pid: Option<u32>, sig: rustix::process::Signal) {
    if let Some(pid) = pid
        .and_then(|p| i32::try_from(p).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, sig);
    }
}

/// Runs one attempt: spawns the CLI in its own process group, writes the prompt, feeds every
/// stdout and stderr line through `norm` into `store` as it arrives, and stops it on `cancel`, the
/// wall-time limit or the output limit (SIGTERM to the group, then SIGKILL after `grace`). Returns
/// once the process has exited and `attempt_completed` or `attempt_failed` is in the store.
pub async fn supervise(
    launch: Launch,
    norm: &mut Normalizer,
    store: &mut TraceStore,
    cancel: CancellationToken,
) -> Result<Outcome, SupervisorError> {
    let Launch {
        exe,
        args,
        cwd,
        env,
        stdin,
        limits,
    } = launch;
    if !exe.is_absolute() || !exe.is_file() {
        return Err(SupervisorError::Refused(format!(
            "{} is not an absolute path to a file",
            exe.display()
        )));
    }
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(SupervisorError::Refused(format!(
            "{} is not an existing absolute directory",
            cwd.display()
        )));
    }
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .current_dir(&cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(SupervisorError::Spawn)?;
    let pid = child.id();
    store.append(norm.start(
        SystemTime::now(),
        json!({ "executable": exe.display().to_string(), "pid": pid }),
    ))?;

    if let Some(mut pipe) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = pipe.write_all(&stdin).await;
        });
    }
    let (tx, mut rx) = mpsc::channel::<Piece>(64);
    let mut open_pipes = 0;
    if let Some(out) = child.stdout.take() {
        open_pipes += 1;
        tokio::spawn(read_lines(
            out,
            Channel::Stdout,
            limits.max_line_bytes,
            limits.max_stdout_bytes,
            tx.clone(),
        ));
    }
    if let Some(err) = child.stderr.take() {
        open_pipes += 1;
        tokio::spawn(read_lines(
            err,
            Channel::Stderr,
            limits.max_line_bytes,
            limits.max_stderr_bytes,
            tx.clone(),
        ));
    }
    drop(tx);

    let deadline = tokio::time::Instant::now() + limits.wall_time;
    let mut stopped: Option<Ending> = None;
    let mut kill_at: Option<tokio::time::Instant> = None;
    let mut exit: Option<std::process::ExitStatus> = None;
    let mut drain_until: Option<tokio::time::Instant> = None;
    let (mut window, mut in_window) = (tokio::time::Instant::now(), 0u32);

    loop {
        if exit.is_some() && open_pipes == 0 {
            break;
        }
        tokio::select! {
            piece = rx.recv(), if open_pipes > 0 => {
                let now = SystemTime::now();
                match piece {
                    None => open_pipes = 0,
                    Some(Piece::Eof) => open_pipes -= 1,
                    Some(Piece::Line(Channel::Stdout, l)) => { store.append(norm.stream_line(&l, now))?; }
                    Some(Piece::Line(_, l)) => { store.append(norm.stderr_line(&l, now))?; }
                    Some(Piece::TooLong(ch, bytes)) => { store.append(norm.gap(ch, "line_too_long", bytes, now))?; }
                    Some(Piece::OutputLimit(bytes)) => {
                        open_pipes -= 1;
                        store.append(norm.gap(Channel::Host, "output_limit", bytes, now))?;
                        if stopped.is_none() {
                            stopped = Some(Ending::OutputLimit);
                            kill_at = Some(tokio::time::Instant::now() + limits.grace);
                            #[cfg(unix)]
                            signal_group(pid, rustix::process::Signal::TERM);
                        }
                    }
                }
                // The rate limit: past it, wait for the next second. The readers block on the full
                // channel meanwhile, and the CLI on its full pipe.
                in_window += 1;
                if in_window >= limits.lines_per_second.max(1) {
                    let next = window + Duration::from_secs(1);
                    if next > tokio::time::Instant::now() {
                        tokio::time::sleep_until(next.min(deadline)).await;
                    }
                    window = tokio::time::Instant::now();
                    in_window = 0;
                }
            }
            status = child.wait(), if exit.is_none() => {
                exit = Some(status.map_err(SupervisorError::Spawn)?);
                // A descendant may still hold the pipes; give it a moment, then end the group.
                drain_until = Some(tokio::time::Instant::now() + Duration::from_secs(2));
            }
            _ = cancel.cancelled(), if stopped.is_none() && exit.is_none() => {
                stopped = Some(Ending::Cancelled);
                kill_at = Some(tokio::time::Instant::now() + limits.grace);
                #[cfg(unix)]
                signal_group(pid, rustix::process::Signal::TERM);
            }
            _ = tokio::time::sleep_until(deadline), if stopped.is_none() && exit.is_none() => {
                stopped = Some(Ending::WallTime);
                kill_at = Some(tokio::time::Instant::now() + limits.grace);
                #[cfg(unix)]
                signal_group(pid, rustix::process::Signal::TERM);
            }
            _ = tokio::time::sleep_until(kill_at.unwrap_or(deadline)), if kill_at.is_some() && exit.is_none() => {
                kill_at = None;
                // The documented last resort: the whole group, by force.
                #[cfg(unix)]
                signal_group(pid, rustix::process::Signal::KILL);
                #[cfg(not(unix))]
                let _ = child.start_kill();
            }
            _ = tokio::time::sleep_until(drain_until.unwrap_or(deadline)), if drain_until.is_some() => {
                #[cfg(unix)]
                signal_group(pid, rustix::process::Signal::KILL);
                break;
            }
        }
    }
    // Whatever the CLI left in its group goes with it.
    #[cfg(unix)]
    signal_group(pid, rustix::process::Signal::KILL);

    let ending = stopped.unwrap_or_else(|| match exit.and_then(|s| s.code()) {
        Some(code) => Ending::Exited(code),
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                Ending::Signalled(exit.and_then(|s| s.signal()).unwrap_or(0))
            }
            #[cfg(not(unix))]
            Ending::Signalled(0)
        }
    });
    let written = store.append(norm.finish(ending, started.elapsed(), SystemTime::now()))?;
    let status = written
        .last()
        .and_then(|e| serde_json::from_value(e.data["status"].clone()).ok())
        .unwrap_or(AttemptStatus::Failed);
    Ok(Outcome { ending, status })
}
