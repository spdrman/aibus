//! The Linux subreaper helper (#352, D-352).
//!
//! `supervise` starts `<helper> MARKER <cli> <args...>` instead of the CLI. The helper marks itself
//! a child subreaper (`PR_SET_CHILD_SUBREAPER`), so the kernel hands every process that loses its
//! parent anywhere under the CLI (a `setsid -f` daemon whose middle process exits at once
//! included) to the helper instead of init. That makes the tree complete at every moment: whatever
//! is left of the CLI's descendants is reachable from the helper, whatever session or group it is
//! in. The helper ends all of it when the CLI exits, when the supervisor asks, and when the
//! supervisor is gone.
//!
//! Protocol, all in the process model (no file, no socket, no environment variable of its own):
//!
//! - The program is an absolute path; the helper adds nothing to the environment, the directory or
//!   the three standard streams, which the CLI inherits as they are.
//! - Stdin must not be a terminal. The mode is not a command: a terminal gets exit status 2.
//! - SIGTERM (also SIGINT and SIGHUP): the helper sends SIGTERM to the CLI's process group, and
//!   goes on running until the CLI exits.
//! - SIGUSR1: the whole tree is ended now, no matter what the CLI is doing. The supervisor sends it
//!   when the grace period is over, and the kernel sends it (`PR_SET_PDEATHSIG`) when the
//!   supervisor dies. The helper also looks at its parent pid on every turn, in case the signal
//!   was lost to the race at start-up.
//! - The CLI's exit status passes through: its exit code is the helper's, and a CLI that died of a
//!   signal makes the helper die of the same signal. A tree ended by force dies of SIGKILL.
//!
//! Ending the tree is the #338 sweep's algorithm (stop what the table shows under the helper,
//! look again until nothing new turns up, then kill), but started from the helper's own pid, so
//! nothing has to be remembered. The table comes from `ps`, so the helper opens no file and reads
//! no environment (`tests/credentials.rs`). Without a usable `ps` the helper can still reap and
//! forward signals but cannot enumerate, and it warns once.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{
    getpid, getppid, kill_process, kill_process_group, set_child_subreaper,
    set_parent_process_death_signal, wait, Pid, Signal, WaitOptions,
};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1};

/// The first argument that selects helper mode. Internal: not a documented command, not a clap
/// subcommand, and never reachable through a CLI's flag allowlist (those lists are for the vendor
/// CLIs the host launches, and the helper is started by the host itself).
pub const MARKER: &str = "--aibus-internal-subreaper";

/// How long the helper waits for the kernel to finish with what it killed.
const REAP_LIMIT: Duration = Duration::from_secs(5);

/// Runs the helper; `args` is what follows [`MARKER`]: the CLI's absolute path and its arguments.
/// Returns the exit code, or does not return when the CLI's signal has to be repeated.
pub fn run(args: Vec<OsString>) -> u8 {
    let mut args = args.into_iter();
    let Some(program) = args
        .next()
        .filter(|p| std::path::Path::new(p).is_absolute())
    else {
        eprintln!("aibus: internal mode, not a command");
        return 2;
    };
    if std::io::stdin().is_terminal() {
        eprintln!("aibus: internal mode, not a command");
        return 2;
    }

    let (term, hard) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    for sig in [SIGTERM, SIGINT, SIGHUP] {
        if signal_hook::flag::register(sig, term.clone()).is_err() {
            eprintln!("aibus: cannot handle signals");
            return 70;
        }
    }
    if signal_hook::flag::register(SIGUSR1, hard.clone()).is_err() {
        eprintln!("aibus: cannot handle signals");
        return 70;
    }
    // The supervisor's death ends the tree. The signal is sent to this process only, so it is
    // handled like a request, not delivered as a kill; the parent check covers the start-up race.
    let parent = getppid();
    if set_parent_process_death_signal(Some(Signal::USR1)).is_err() || getppid() != parent {
        hard.store(true, Ordering::Relaxed);
    }
    let me = getpid();
    if let Err(e) = set_child_subreaper(Some(me)) {
        eprintln!("aibus: cannot become a subreaper: {e}");
        return 70;
    }

    let child = match Command::new(&program).args(args).process_group(0).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aibus: cannot start {}: {e}", program.to_string_lossy());
            return if e.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            };
        }
    };
    let cli = Pid::from_raw(i32::try_from(child.id()).unwrap_or(0));
    // Dropping `Child` does not wait or kill, and waiting is done here for every child at once.
    drop(child);

    let mut status = None;
    loop {
        if term.swap(false, Ordering::Relaxed) {
            if let Some(cli) = cli {
                let _ = kill_process_group(cli, Signal::TERM);
            }
        }
        // Everything that ended: the CLI, and the orphans that were handed to this process.
        while let Ok(Some((pid, st))) = wait(WaitOptions::NOHANG) {
            if Some(pid) == cli {
                status = Some(st);
            }
        }
        if status.is_some() || hard.load(Ordering::Relaxed) || getppid() != parent {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    crate::descendants::kill_under(me.as_raw_pid() as u32);
    if let Some(cli) = cli {
        // The group may hold what `ps` could not show (no `ps`), and does no harm otherwise.
        let _ = kill_process_group(cli, Signal::KILL);
    }
    let start = Instant::now();
    loop {
        match wait(WaitOptions::NOHANG) {
            Err(Errno::CHILD) => break,
            Ok(Some((pid, st))) => {
                if Some(pid) == cli {
                    status = Some(st);
                }
            }
            _ if start.elapsed() > REAP_LIMIT => break,
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }

    let Some(st) = status else {
        // Ended by force with the CLI's own status unknown.
        let _ = kill_process(me, Signal::KILL);
        return 137;
    };
    if let Some(code) = st.exit_status() {
        return u8::try_from(code).unwrap_or(1);
    }
    let sig = st.terminating_signal().unwrap_or(9);
    // The CLI's signal is repeated on this process, default action restored first (the runtime
    // and this helper have handlers of their own for some), so the supervisor sees the signal the
    // CLI died of. SIGKILL cannot be emulated, it is sent.
    if sig == 9 {
        let _ = kill_process(me, Signal::KILL);
    } else {
        let _ = signal_hook::low_level::emulate_default_handler(sig);
    }
    u8::try_from(128 + sig).unwrap_or(1)
}
