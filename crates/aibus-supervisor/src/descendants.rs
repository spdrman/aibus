//! Finding and ending what a supervised CLI left running (#338).
//!
//! The process-group kill reaches every descendant that stayed in the group. A descendant that
//! called `setsid` or `setpgrp`, or that double-forked and was adopted by init, is outside it. The
//! host gets no kernel guarantee for those without `unsafe` (a subreaper has to be set in the
//! child between fork and exec) or a delegated cgroup, so this is lineage tracking, the same on
//! Linux and macOS: a sampler reads the process table every [`SAMPLE`] and remembers each process
//! that descends from the CLI, by pid and start time. At the end every remembered process that is
//! still running (same pid, same start time, so not a reused pid), and everything under it, gets
//! SIGKILL, wherever its session, group or parent is by then.
//!
//! The table comes from `ps`, so the supervisor still opens no file and reads no environment (the
//! credentials rule, `tests/credentials.rs`). What this does not catch: a descendant that
//! detaches and loses its parent between two samples, then outlives the CLI. The docs say so.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustix::process::{kill_process, Pid, Signal};

const SAMPLE: Duration = Duration::from_millis(50);
/// Rounds of "look, then kill" at the end: each one finds what the last round's kills forked.
const ROUNDS: usize = 10;

struct Proc {
    pid: u32,
    ppid: u32,
    /// Fixes the process in time: a pid that comes back later has another one.
    start: String,
}

/// Every process of every user, with its parent and start time.
fn table() -> Vec<Proc> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,lstart="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (pid, ppid) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
            // `lstart` is five words, e.g. `Mon Nov 16 10:00:00 2026`.
            let start = f.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, ppid, start })
        })
        .collect()
}

/// The run's processes in `procs`: the leader (while it lives), what the sampler remembered that
/// is still the same process, and everything under any of them.
fn ours(procs: &[Proc], leader: Option<u32>, seen: &HashMap<u32, String>) -> HashSet<u32> {
    let mut set: HashSet<u32> = procs
        .iter()
        .filter(|p| seen.get(&p.pid) == Some(&p.start))
        .map(|p| p.pid)
        .collect();
    set.extend(leader);
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

/// One run's tracker. Dropping it stops the sampler.
pub struct Tracker {
    leader: u32,
    seen: Arc<Mutex<HashMap<u32, String>>>,
    sampler: tokio::task::JoinHandle<()>,
}

impl Tracker {
    /// Must run inside a tokio runtime.
    pub fn start(leader: u32) -> Self {
        let seen = Arc::new(Mutex::new(HashMap::new()));
        let sampler = tokio::spawn({
            let seen = seen.clone();
            async move {
                loop {
                    if let Ok(procs) = tokio::task::spawn_blocking(table).await {
                        let mut seen = seen.lock().unwrap();
                        let now = ours(&procs, Some(leader), &seen);
                        seen.extend(
                            procs
                                .iter()
                                .filter(|p| now.contains(&p.pid) && p.pid != leader)
                                .map(|p| (p.pid, p.start.clone())),
                        );
                    }
                    tokio::time::sleep(SAMPLE).await;
                }
            }
        });
        Self {
            leader,
            seen,
            sampler,
        }
    }

    /// SIGKILLs everything of the run that is still running, whatever session or group it is in.
    /// `leader_alive` is false once the CLI has been collected, because its pid may be reused.
    pub fn kill_all(&self, leader_alive: bool) {
        let mut killed = HashSet::new();
        for _ in 0..ROUNDS {
            let procs = table();
            let seen = self.seen.lock().unwrap().clone();
            let fresh: Vec<u32> = ours(&procs, leader_alive.then_some(self.leader), &seen)
                .into_iter()
                .filter(|p| killed.insert(*p))
                .collect();
            if fresh.is_empty() {
                return;
            }
            for pid in fresh {
                if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                    let _ = kill_process(pid, Signal::KILL);
                }
            }
        }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.sampler.abort();
    }
}
