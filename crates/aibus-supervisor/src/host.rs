//! The process host: one attempt from spawn to exit.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aibus_trace::{AttemptStatus, Batch, StoreError, TraceEvent, TraceStore};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::claude::Channel;
#[cfg(unix)]
use crate::descendants::Tracker;
use crate::{Ending, Engine, Limits};

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
    /// Linux only: a program that speaks the subreaper protocol (`reaper`), usually the `aibus`
    /// binary itself. The CLI then runs under it, so the kernel hands every orphaned descendant to
    /// it and it ends them all (D-352). `None`, and on other systems, keeps the #338 sweep only.
    pub helper: Option<PathBuf>,
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
/// and reported by size; stdout stops at `max_total`, stderr past it is drained and dropped (the
/// dropped byte count goes in `dropped`, reported once by `supervise`).
async fn read_lines(
    mut pipe: impl AsyncRead + Unpin,
    ch: Channel,
    max_line: usize,
    max_total: u64,
    dropped: Arc<AtomicU64>,
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
        // stderr keeps the head of the read that crosses the cap, up to the cap; only the rest
        // (and everything after) is dropped and counted.
        let mut keep = n;
        if total > max_total {
            if ch == Channel::Stdout {
                let _ = tx.send(Piece::OutputLimit(total)).await;
                return;
            }
            keep = max_total.saturating_sub(total - n as u64) as usize;
            dropped.fetch_add((n - keep) as u64, Ordering::Relaxed);
        }
        for chunk in buf[..keep].split_inclusive(|b| *b == b'\n') {
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

/// Appends `batch` and tells `norm` the last id the store holds, so the store's own events (its
/// gaps) never tie with the normaliser's next id. A refusal ends the run with the store error;
/// the store restores its state first, so nothing is dropped without a gap.
fn put(
    store: &mut TraceStore,
    batch: Batch,
    norm: &mut dyn Engine,
) -> Result<Vec<TraceEvent>, StoreError> {
    let out = store.append(batch)?;
    if let Some(id) = store.last_id() {
        norm.observe(id);
    }
    Ok(out)
}

/// Runs one attempt: spawns the CLI in its own process group, writes the prompt, feeds every
/// stdout and stderr line through `norm` into `store` as it arrives, and stops it on `cancel`, the
/// wall-time limit or the output limit (SIGTERM to the group, then SIGKILL after `grace`). Returns
/// once the process has exited and `attempt_completed` or `attempt_failed` is in the store.
pub async fn supervise(
    launch: Launch,
    norm: &mut dyn Engine,
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
        helper,
    } = launch;
    let _ = &helper;
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
    /// Ends the CLI's whole group when `supervise` returns early (a store error), so the CLI's
    /// tool processes are not orphaned. `kill_on_drop` only reaches the leader.
    struct GroupGuard(Option<u32>, #[cfg(unix)] Option<Tracker>);
    impl Drop for GroupGuard {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                signal_group(self.0, rustix::process::Signal::KILL);
                if let Some(t) = &self.1 {
                    t.kill_all();
                }
            }
        }
    }
    let mut guard = GroupGuard(
        pid,
        #[cfg(unix)]
        pid.map(Tracker::start),
    );
    if let Some(id) = store.last_id() {
        // Seed from the run, so this writer sorts after whatever is already in it.
        norm.observe(id);
    }
    put(
        store,
        norm.start(
            SystemTime::now(),
            json!({ "executable": exe.display().to_string(), "pid": pid }),
        ),
        norm,
    )?;

    if let Some(mut pipe) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = pipe.write_all(&stdin).await;
        });
    }
    let (tx, mut rx) = mpsc::channel::<Piece>(64);
    let mut open_pipes = 0;
    let stderr_dropped = Arc::new(AtomicU64::new(0));
    if let Some(out) = child.stdout.take() {
        open_pipes += 1;
        tokio::spawn(read_lines(
            out,
            Channel::Stdout,
            limits.max_line_bytes,
            limits.max_stdout_bytes,
            Arc::default(),
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
            stderr_dropped.clone(),
            tx.clone(),
        ));
    }
    drop(tx);

    let deadline = tokio::time::Instant::now() + limits.wall_time;
    let mut stopped: Option<Ending> = None;
    let mut kill_at: Option<tokio::time::Instant> = None;
    let mut exit: Option<std::process::ExitStatus> = None;
    // After exit: the drain ends when nothing has been read for `drain_stall`, or `drain_max` after
    // the exit whatever keeps arriving. Both are deadlines, so the drain always ends.
    // Set when the drain was cut off: true if it hit `drain_max` (lines were still arriving).
    let mut drained: Option<bool> = None;
    // Which bound `cap_at` is: `grace` when a stopped run's drain is cut to the grace period.
    let mut cap_name = "drain_max";
    let mut stall_at: Option<tokio::time::Instant> = None;
    let mut cap_at: Option<tokio::time::Instant> = None;
    let (mut window, mut in_window) = (tokio::time::Instant::now(), 0u32);
    // Set while the rate limit holds reading back. The wait is an arm of its own, so the exit, a
    // cancel and the deadlines are all still watched meanwhile (#336).
    let mut throttled_until: Option<tokio::time::Instant> = None;

    loop {
        if exit.is_some() && open_pipes == 0 {
            break;
        }
        // The bound is checked on every pass too, so a steady flow cannot starve its timer arm.
        if cap_at.is_some_and(|c| tokio::time::Instant::now() >= c) {
            drained = Some(true);
            break;
        }
        // An exit that has already happened is acted on before a cancel that is ready at the
        // same moment, so `select!` picking at random cannot change how the drain is labelled
        // (#336).
        if exit.is_none() {
            if let Some(status) = child.try_wait().map_err(SupervisorError::Spawn)? {
                exit = Some(status);
                // The pipes may still hold output, or a descendant may hold them open. Keep reading
                // while lines arrive; end the group when they stop or the bound is reached.
                let now = tokio::time::Instant::now();
                stall_at = Some(now + limits.drain_stall);
                // A run that was already being stopped (cancel, wall time, output limit) has had its
                // chance: its drain is capped at the grace period, not the full bound.
                let cap = if stopped.is_some() && limits.grace < limits.drain_max {
                    cap_name = "grace";
                    limits.grace
                } else {
                    limits.drain_max
                };
                cap_at = Some(now + cap);
                continue;
            }
        }
        tokio::select! {
            piece = rx.recv(), if open_pipes > 0 && throttled_until.is_none() => {
                let now = SystemTime::now();
                match piece {
                    None => open_pipes = 0,
                    Some(Piece::Eof) => open_pipes -= 1,
                    Some(Piece::Line(Channel::Stdout, l)) => { put(store, norm.stream_line(&l, now), norm)?; }
                    Some(Piece::Line(_, l)) => { put(store, norm.stderr_line(&l, now), norm)?; }
                    Some(Piece::TooLong(ch, bytes)) => { put(store, norm.gap(ch, "line_too_long", bytes, now), norm)?; }
                    Some(Piece::OutputLimit(bytes)) => {
                        open_pipes -= 1;
                        put(store, norm.gap(Channel::Host, "output_limit", bytes, now), norm)?;
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
                        throttled_until = Some(next.min(deadline));
                    } else {
                        window = tokio::time::Instant::now();
                        in_window = 0;
                    }
                }
                if exit.is_some() {
                    stall_at = Some(tokio::time::Instant::now() + limits.drain_stall);
                }
            }
            _ = tokio::time::sleep_until(throttled_until.unwrap_or(deadline)), if throttled_until.is_some() => {
                throttled_until = None;
                window = tokio::time::Instant::now();
                in_window = 0;
            }
            // The exit itself is handled at the top of the loop, where it is checked before any
            // cancel: waking here is enough.
            _ = child.wait(), if exit.is_none() => continue,
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
            // Cancel and the wall deadline end the drain too: the run reports how it was stopped,
            // and anything already read but not recorded is a gap.
            _ = cancel.cancelled(), if stopped.is_none() && exit.is_some() => {
                stopped = Some(Ending::Cancelled);
                drained = Some(false);
                break;
            }
            _ = tokio::time::sleep_until(deadline), if stopped.is_none() && exit.is_some() => {
                stopped = Some(Ending::WallTime);
                drained = Some(false);
                break;
            }
            _ = tokio::time::sleep_until(stall_at.unwrap_or(deadline)), if stall_at.is_some() => {
                // Slow storing, not silence: lines are waiting, so the stall has not happened.
                if !rx.is_empty() {
                    stall_at = Some(tokio::time::Instant::now() + limits.drain_stall);
                    continue;
                }
                drained = Some(false);
                break;
            }
            _ = tokio::time::sleep_until(cap_at.unwrap_or(deadline)), if cap_at.is_some() => {
                drained = Some(true);
                break;
            }
        }
    }
    // Whatever the CLI left in its group goes with it, and so does what left the group (#338).
    #[cfg(unix)]
    {
        signal_group(pid, rustix::process::Signal::KILL);
        if let Some(t) = &guard.1 {
            t.kill_all();
        }
    }
    if let Some(hit_bound) = drained {
        // Count what was already read off the pipes but not yet recorded. What is still in the
        // kernel pipe is not counted (the host does not read it), so the count is a lower bound.
        rx.close();
        let (mut lines, mut bytes) = (0u64, 0u64);
        while let Ok(piece) = rx.try_recv() {
            match piece {
                Piece::Line(_, l) => {
                    lines += 1;
                    bytes += l.len() as u64;
                }
                Piece::TooLong(_, b) => {
                    lines += 1;
                    bytes += b;
                }
                _ => {}
            }
        }
        // A quiet stall with nothing queued means a descendant held the pipes and nothing was
        // lost. Anything queued, or lines still arriving at the bound, is reported.
        if hit_bound || lines > 0 {
            let mut gap = norm.gap(Channel::Host, "unread_after_exit", bytes, SystemTime::now());
            gap.events[0].data["lines"] = json!(lines);
            gap.events[0].data["bound"] = json!(if hit_bound { cap_name } else { "drain_stall" });
            put(store, gap, norm)?;
        }
    }
    // Written once, here, however the read ended: a drain cut off before the pipe closed still
    // reports what was dropped so far (a lower bound, like the unread count).
    let dropped = stderr_dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        put(
            store,
            norm.gap(Channel::Stderr, "stderr_limit", dropped, SystemTime::now()),
            norm,
        )?;
    }
    guard.0 = None;

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
    let written = put(
        store,
        norm.finish(ending, started.elapsed(), SystemTime::now()),
        norm,
    )?;
    let status = written
        .last()
        .and_then(|e| serde_json::from_value(e.data["status"].clone()).ok())
        .unwrap_or(AttemptStatus::Failed);
    Ok(Outcome { ending, status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Normalizer;
    use aibus_trace::{AttemptRef, PayloadPolicy, StoreLimits};
    use std::time::Duration;

    #[test]
    fn put_keeps_the_normaliser_after_the_stores_own_gap_328() {
        let tmp = tempfile::tempdir().unwrap();
        let limits = StoreLimits {
            soft_max_bytes: 0,
            ..StoreLimits::default()
        };
        let (mut store, _) = TraceStore::open(
            &tmp.path().join("run"),
            aibus_core::Redactor::new(),
            PayloadPolicy::MetadataOnly,
            limits,
        )
        .unwrap();
        let mut norm = Normalizer::new(AttemptRef {
            workflow_run_id: uuid::Uuid::from_u128(1),
            attempt_id: uuid::Uuid::from_u128(2),
            parent_attempt_id: None,
            provider_extension: "org.aibus.test".into(),
        });
        // In the past, so the store's gap (stamped with its own clock) lands in a later millisecond.
        let then = SystemTime::now() - Duration::from_secs(5);
        put(&mut store, norm.start(then, json!({})), &mut norm).unwrap();
        // Past the soft limit a stderr warning is dropped and the store writes its own gap.
        put(&mut store, norm.stderr_line(b"dropped", then), &mut norm).unwrap();
        let gap = store.last_id().unwrap();
        let gap_ms = gap.as_bytes()[..6]
            .iter()
            .fold(0u64, |a, x| a << 8 | u64::from(*x));
        let next = norm.stderr_line(
            b"next",
            SystemTime::UNIX_EPOCH + Duration::from_millis(gap_ms),
        );
        // Time and counter, not the hash tail, put the next event after the gap.
        assert!(next.events[0].event_id.as_bytes()[..8] > gap.as_bytes()[..8]);
    }
}
