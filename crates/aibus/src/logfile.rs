//! `aibus.log` with daily rotation, `log_files_kept` files, and a size guard that rotates early
//! (07 section 4). The active file is always `aibus.log`; a rotated one is
//! `aibus.log.<YYYY-MM-DD>` of the day it was written, with `.1`, `.2` ... when a day needed
//! more than one. Files are created 0600. A write that cannot be made is dropped: the log must
//! never be the reason a run fails.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tracing_subscriber::fmt::MakeWriter;

use crate::events::date;

/// 64 MiB, the size at which a day's file is rotated early.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

struct State {
    file: Option<File>,
    /// The UTC day the active file was started on.
    day: String,
    size: u64,
}

pub struct RotatingLog {
    dir: PathBuf,
    /// Files to keep, the active one included.
    keep: usize,
    max_bytes: u64,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    state: Mutex<State>,
}

fn open(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Moves `aibus.log` aside under the name of the day it was started on.
fn rotate(dir: &Path, state: &mut State) {
    state.file = None;
    let base = format!("aibus.log.{}", state.day);
    let mut to = dir.join(&base);
    let mut n = 0;
    while to.exists() {
        n += 1;
        to = dir.join(format!("{base}.{n}"));
    }
    let _ = std::fs::rename(dir.join("aibus.log"), to);
}

impl RotatingLog {
    /// Opens `dir/aibus.log`. A file left over from an earlier day, or a full one, is rotated
    /// first.
    pub fn open(
        dir: &Path,
        keep: u32,
        max_bytes: u64,
        now: impl Fn() -> SystemTime + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let today = date(now());
        let mut state = State {
            file: None,
            day: today.clone(),
            size: 0,
        };
        if let Ok(m) = std::fs::metadata(dir.join("aibus.log")) {
            state.day = m.modified().map_or_else(|_| today.clone(), date);
            if state.day != today || m.len() >= max_bytes {
                rotate(dir, &mut state);
                state.day = today;
            }
        }
        let file = open(&dir.join("aibus.log"))?;
        state.size = file.metadata().map_or(0, |m| m.len());
        state.file = Some(file);
        let log = Self {
            dir: dir.to_owned(),
            keep: keep.max(1) as usize,
            max_bytes,
            now: Box::new(now),
            state: Mutex::new(state),
        };
        log.prune();
        Ok(log)
    }

    /// Removes the oldest rotated files so that at most `keep` files remain with the active one.
    fn prune(&self) {
        let mut rotated: Vec<(String, u32, PathBuf)> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let rest = name.strip_prefix("aibus.log.")?;
                let (day, seq) = rest.split_once('.').unwrap_or((rest, "0"));
                Some((day.to_owned(), seq.parse().ok()?, e.path()))
            })
            .collect();
        rotated.sort();
        let excess = rotated.len().saturating_sub(self.keep - 1);
        for (_, _, path) in rotated.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn write_all(&self, buf: &[u8]) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let today = date((self.now)());
        let full = state.size > 0 && state.size + buf.len() as u64 > self.max_bytes;
        if state.day != today || full {
            rotate(&self.dir, &mut state);
            state.day = today;
            state.size = 0;
            self.prune();
        }
        if state.file.is_none() {
            state.file = open(&self.dir.join("aibus.log")).ok();
        }
        if let Some(f) = state.file.as_mut() {
            if f.write_all(buf).is_ok() {
                state.size += buf.len() as u64;
            }
        }
    }
}

pub struct LogWriter<'a>(&'a RotatingLog);

impl Write for LogWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RotatingLog {
    type Writer = LogWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    const DAY: u64 = 86_400;
    /// 2026-11-08 00:00:00 UTC.
    const START: u64 = 1_794_096_000;

    fn clock(secs: Arc<AtomicU64>) -> impl Fn() -> SystemTime + Send + Sync + 'static {
        move || UNIX_EPOCH + Duration::from_secs(secs.load(Ordering::Relaxed))
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    fn day(n: u64) -> String {
        date(UNIX_EPOCH + Duration::from_secs(START + n * DAY))
    }

    #[test]
    fn a_new_day_rotates_and_only_the_newest_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let secs = Arc::new(AtomicU64::new(START));
        let log = RotatingLog::open(dir.path(), 3, MAX_BYTES, clock(secs.clone())).unwrap();
        for d in 0..5 {
            secs.store(START + d * DAY, Ordering::Relaxed);
            log.write_all(format!("day {d}\n").as_bytes());
        }
        assert_eq!(
            names(dir.path()),
            [
                "aibus.log".to_owned(),
                format!("aibus.log.{}", day(2)),
                format!("aibus.log.{}", day(3)),
            ]
        );
        assert_eq!(std::fs::read_to_string(dir.path().join("aibus.log")).unwrap(), "day 4\n");
        let third = dir.path().join(format!("aibus.log.{}", day(3)));
        assert_eq!(std::fs::read_to_string(third).unwrap(), "day 3\n");
    }

    #[test]
    fn the_size_guard_rotates_early_within_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let secs = Arc::new(AtomicU64::new(START));
        let log = RotatingLog::open(dir.path(), 7, 10, clock(secs)).unwrap();
        for line in ["aaaaaa\n", "bbbbbb\n", "cccccc\n"] {
            log.write_all(line.as_bytes());
        }
        assert_eq!(
            names(dir.path()),
            [
                "aibus.log".to_owned(),
                format!("aibus.log.{}", day(0)),
                format!("aibus.log.{}.1", day(0)),
            ]
        );
        assert_eq!(std::fs::read_to_string(dir.path().join("aibus.log")).unwrap(), "cccccc\n");
    }

    #[test]
    fn a_file_from_an_earlier_day_is_rotated_on_open_and_modes_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let secs = Arc::new(AtomicU64::new(START));
        {
            let log = RotatingLog::open(dir.path(), 7, MAX_BYTES, clock(secs.clone())).unwrap();
            log.write_all(b"old\n");
        }
        // Make the file look a day old instead of waiting for one.
        let file = File::options().write(true).open(dir.path().join("aibus.log")).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(START - DAY)).unwrap();
        drop(file);
        let log = RotatingLog::open(dir.path(), 7, MAX_BYTES, clock(secs)).unwrap();
        log.write_all(b"new\n");
        let n = names(dir.path());
        assert_eq!(n.len(), 2, "{n:?}");
        assert_eq!(std::fs::read_to_string(dir.path().join("aibus.log")).unwrap(), "new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(dir.path().join("aibus.log")).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }
}
