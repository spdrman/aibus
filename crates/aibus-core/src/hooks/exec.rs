//! Running one hook command: `sh -c` in its own process group with the JSON on stdin, stdout and
//! stderr captured separately (each capped at 1 MiB, keeping the tail), and a stop flag that the
//! async side raises on a timeout or a cancel. The deadline itself is tokio's clock, so a test
//! with a paused clock sees a 60 s timeout without waiting for it.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::tools::{kill_group, Tail};

pub(super) struct Job {
    pub command: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub stdin: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Ended {
    /// The shell's own exit code; `None` when a signal killed it.
    Exited(Option<i32>),
    TimedOut,
    Cancelled,
}

pub(super) struct Done {
    pub ended: Ended,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

/// Runs `job` to its end, or until `timeout` passes on tokio's clock, or until `cancel`.
pub(super) async fn run(
    job: Job,
    timeout: Duration,
    cancel: &CancellationToken,
) -> std::io::Result<Done> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let task = tokio::task::spawn_blocking(move || blocking(&job, &flag));
    tokio::pin!(task);
    let mut why = None;
    let joined = tokio::select! {
        r = &mut task => r,
        () = sleep(timeout) => {
            why = Some(Ended::TimedOut);
            stop.store(true, Ordering::SeqCst);
            task.await
        }
        () = cancel.cancelled() => {
            why = Some(Ended::Cancelled);
            stop.store(true, Ordering::SeqCst);
            task.await
        }
    };
    let mut done = joined.map_err(std::io::Error::other)??;
    if let Some(why) = why {
        done.ended = why;
    }
    Ok(done)
}

fn blocking(job: &Job, stop: &AtomicBool) -> std::io::Result<Done> {
    // How much of each stream is kept (the tail, 1 MiB).
    const CAPTURE_CAP: usize = 1024 * 1024;
    // How often the running hook is checked for exit and stop.
    const POLL: Duration = Duration::from_millis(10);
    // How long the pipes get to drain after the shell has exited before a descendant that still
    // holds them is killed.
    const DRAIN_GRACE: Duration = Duration::from_millis(250);

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&job.command)
        .current_dir(&job.cwd)
        .env_clear()
        .envs(job.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let started = Instant::now();
    let mut child = cmd.spawn()?;
    let pgid = child.id();

    // The hook may never read its stdin, so the write gets a thread of its own. A broken pipe
    // just means it did not want the JSON.
    let writer = child.stdin.take().map(|mut stdin| {
        let body = job.stdin.clone();
        thread::spawn(move || {
            let _ = stdin.write_all(&body);
        })
    });
    let stdout = Arc::new(Mutex::new(Tail::new(CAPTURE_CAP)));
    let stderr = Arc::new(Mutex::new(Tail::new(CAPTURE_CAP)));
    let mut readers = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        readers.push(drain(move |b| pipe.read(b), Arc::clone(&stdout)));
    }
    if let Some(mut pipe) = child.stderr.take() {
        readers.push(drain(move |b| pipe.read(b), Arc::clone(&stderr)));
    }

    let ended = loop {
        if let Some(status) = child.try_wait()? {
            break Ended::Exited(status.code());
        }
        if stop.load(Ordering::SeqCst) {
            kill_group(pgid);
            child.wait()?;
            break Ended::Cancelled;
        }
        thread::sleep(POLL);
    };
    let drained = Instant::now();
    while readers.iter().any(|r| !r.is_finished()) {
        if drained.elapsed() >= DRAIN_GRACE || stop.load(Ordering::SeqCst) {
            kill_group(pgid);
            break;
        }
        thread::sleep(POLL);
    }
    for r in readers.into_iter().chain(writer) {
        let _ = r.join();
    }
    let text = |t: &Mutex<Tail>| {
        t.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .text()
            .0
    };
    Ok(Done {
        ended,
        stdout: text(&stdout),
        stderr: text(&stderr),
        elapsed: started.elapsed(),
    })
}

fn drain(
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize> + Send + 'static,
    into: Arc<Mutex<Tail>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = read(&mut chunk) {
            if n == 0 {
                break;
            }
            into.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(&chunk[..n]);
        }
    })
}
