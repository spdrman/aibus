//! Finding and ending what a supervised CLI left running (#338).
//!
//! The process-group kill reaches every descendant that stayed in the group. A descendant that
//! called `setsid` or `setpgrp`, or that double-forked and was adopted by init, is outside it. The
//! host gets no kernel guarantee for those without `unsafe` (a subreaper has to be set in the
//! child between fork and exec) or a cgroup, so this is lineage tracking, the same on Linux and
//! macOS: a sampler reads the process table every 50 ms (plus the time `ps` takes, about 70 ms in
//! all) and remembers each process that descends from the CLI, the CLI included, by pid and start
//! time. At the end every remembered process that is still running (same pid, same start time, so
//! not a reused pid), and everything under it, is stopped, re-scanned until nothing new turns up,
//! and then killed, wherever its session, group or parent is by then.
//!
//! This is a sweep, not containment. A standard daemon double fork, where the middle process exits
//! at once, is usually not caught, and a process that is trying to escape is not stopped. The table
//! comes from `ps`, so the supervisor still opens no file and reads no environment (the
//! credentials rule, `tests/credentials.rs`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Once};

use rustix::process::{kill_process, Pid, Signal};

struct Proc {
    pid: u32,
    ppid: u32,
    /// Fixes the process in time: a pid that comes back later has another one.
    start: String,
}

/// Every process of every user, with its parent and start time. Warns once when `ps` cannot give
/// that (missing, or a busybox `ps` without `-ax` and `-o lstart`).
fn table() -> Vec<Proc> {
    static WARN: Once = Once::new();
    let procs: Vec<Proc> = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,lstart="])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    let mut f = l.split_whitespace();
                    let (pid, ppid) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
                    // `lstart` is five words, e.g. `Mon Nov 16 10:00:00 2026`.
                    let start = f.collect::<Vec<_>>().join(" ");
                    Some(Proc { pid, ppid, start })
                })
                .collect()
        })
        .unwrap_or_default();
    if procs.is_empty() {
        WARN.call_once(|| {
            eprintln!(
                "aibus: `ps -ax -o pid=,ppid=,lstart=` gave nothing (missing, or a busybox ps), \
                 so descendants that left the process group are not tracked (#338)"
            );
        });
    }
    procs
}

/// The run's processes in `procs`: what was remembered (the CLI first) that is still the same
/// process, and everything under any of them.
fn ours(procs: &[Proc], seen: &HashMap<u32, String>) -> HashSet<u32> {
    let mut set: HashSet<u32> = procs
        .iter()
        .filter(|p| seen.get(&p.pid) == Some(&p.start))
        .map(|p| p.pid)
        .collect();
    loop {
        let more: Vec<u32> = procs
            .iter()
            .filter(|p| set.contains(&p.ppid))
            .map(|p| p.pid)
            .collect();
        let before = set.len();
        set.extend(more);
        if set.len() == before {
            break;
        }
    }
    set.retain(|&p| p > 1 && p != std::process::id());
    set
}

fn signal(pids: impl IntoIterator<Item = u32>, sig: Signal) {
    for pid in pids {
        if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
            let _ = kill_process(pid, sig);
        }
    }
}

/// One run's tracker. Dropping it stops the sampler.
pub struct Tracker {
    seen: Arc<Mutex<HashMap<u32, String>>>,
    sampler: tokio::task::JoinHandle<()>,
}

impl Tracker {
    /// Must run inside a tokio runtime. Reads the process table once, here, to fix the CLI's start
    /// time, so its pid cannot later be taken for a new process that reuses it.
    pub fn start(leader: u32) -> Self {
        let seen: HashMap<u32, String> = table()
            .into_iter()
            .filter(|p| p.pid == leader)
            .map(|p| (p.pid, p.start))
            .collect();
        let seen = Arc::new(Mutex::new(seen));
        let sampler = tokio::spawn({
            let seen = seen.clone();
            async move {
                loop {
                    if let Ok(procs) = tokio::task::spawn_blocking(table).await {
                        let mut seen = seen.lock().unwrap();
                        let now = ours(&procs, &seen);
                        seen.extend(
                            procs
                                .iter()
                                .filter(|p| now.contains(&p.pid))
                                .map(|p| (p.pid, p.start.clone())),
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        });
        Self { seen, sampler }
    }

    /// Ends everything of the run that is still running, whatever session or group it is in. It
    /// stops what it finds and looks again until nothing new turns up (a stopped process cannot
    /// fork, so the tree stops growing), then kills. The `ps` runs on the calling thread, so on a
    /// current-thread runtime this holds everything else for the time it takes (tens of
    /// milliseconds, a few times over).
    pub fn kill_all(&self) {
        let mut frozen = HashSet::new();
        for _ in 0..10 {
            let seen = self.seen.lock().unwrap().clone();
            let fresh: Vec<u32> = ours(&table(), &seen)
                .into_iter()
                .filter(|p| frozen.insert(*p))
                .collect();
            if fresh.is_empty() {
                break;
            }
            signal(fresh, Signal::STOP);
        }
        signal(frozen, Signal::KILL);
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.sampler.abort();
    }
}
