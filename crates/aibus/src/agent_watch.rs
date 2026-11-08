//! Watching the agent directories (`agents.reload_on_change`): a change to a definition file
//! sends `Msg::AgentsReloaded` into the loop. Events are batched, since an editor save is
//! several of them.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use aibus_core::Msg;
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// Quiet time after the last event before the directories are read again.
const SETTLE: Duration = Duration::from_millis(200);

/// Dropping it stops the watch and its thread.
pub struct AgentWatch {
    _watcher: notify::RecommendedWatcher,
}

/// Watches the directories in `dirs` that exist. After each batch of changes `reload` reads the
/// definitions again; `None` (a failed read) keeps what is loaded. The sender is held weakly, so
/// the watch never keeps the loop's channel open.
pub fn start(
    dirs: &[PathBuf],
    tx: &mpsc::Sender<Msg>,
    reload: impl Fn() -> Option<Msg> + Send + 'static,
) -> notify::Result<AgentWatch> {
    let (events, changes) = std_mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |e: notify::Result<notify::Event>| {
        if e.is_ok_and(|e| !matches!(e.kind, EventKind::Access(_))) {
            let _ = events.send(());
        }
    })?;
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    let tx = tx.downgrade();
    std::thread::spawn(move || {
        // Ends when the watcher (and with it the event sender) is dropped.
        while changes.recv().is_ok() {
            while changes.recv_timeout(SETTLE).is_ok() {}
            let Some(msg) = reload() else { continue };
            let Some(tx) = tx.upgrade() else { break };
            if tx.blocking_send(msg).is_err() {
                break;
            }
        }
    });
    Ok(AgentWatch { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_core::Msg;
    use std::time::Instant;

    fn reloaded() -> Msg {
        Msg::AgentsReloaded {
            defs: Vec::new(),
            warnings: vec!["seen".into()],
        }
    }

    fn wait_for_one(rx: &mut mpsc::Receiver<Msg>, within: Duration) -> Option<Msg> {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if let Ok(m) = rx.try_recv() {
                return Some(m);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn a_new_file_in_a_watched_directory_sends_agents_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let _watch = start(&[dir.path().to_owned()], &tx, || Some(reloaded())).unwrap();
        std::fs::write(dir.path().join("a.md"), "---\nname: a\n---\n").unwrap();
        let msg = wait_for_one(&mut rx, Duration::from_secs(10)).expect("no AgentsReloaded within 10s");
        assert_eq!(msg.name(), "AgentsReloaded");
    }

    #[test]
    fn a_failed_reload_sends_nothing_and_a_missing_directory_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let dirs = [dir.path().join("not-there"), dir.path().to_owned()];
        let _watch = start(&dirs, &tx, || None).unwrap();
        std::fs::write(dir.path().join("a.md"), "x").unwrap();
        assert!(wait_for_one(&mut rx, Duration::from_secs(1)).is_none());
    }
}
