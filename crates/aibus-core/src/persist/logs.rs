//! `logs/aibus.log` (07 section 4): rotated daily, or early once it passes the size guard, with
//! only the newest `log_files_kept` files left. Files and directory stay private.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::fs::create_private_dir;
use super::time::date;

pub const NAME: &str = "aibus.log";
/// Each file is capped here; one that would pass it rotates early.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

struct Open {
    file: File,
    day: u64,
    size: u64,
}

pub struct RotatingLog {
    dir: PathBuf,
    keep: usize,
    max_bytes: u64,
    open: Mutex<Open>,
}

fn day_of(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() / 86_400
}

fn create(path: &Path) -> io::Result<File> {
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

impl RotatingLog {
    /// Opens `<dir>/aibus.log`. A file left from an earlier day is rotated first.
    pub fn open(dir: &Path, keep: usize, max_bytes: u64, now: SystemTime) -> io::Result<Self> {
        create_private_dir(dir)?;
        let path = dir.join(NAME);
        let (day, size) = match fs::metadata(&path) {
            Ok(m) => (m.modified().map_or(day_of(now), day_of), m.len()),
            Err(_) => (day_of(now), 0),
        };
        let this = Self {
            dir: dir.to_owned(),
            keep,
            max_bytes,
            open: Mutex::new(Open {
                file: create(&path)?,
                day,
                size,
            }),
        };
        if day != day_of(now) {
            this.roll(&mut this.open.lock().unwrap_or_else(|e| e.into_inner()), now)?;
        }
        Ok(this)
    }

    /// Moves the current file to `aibus.log.<date>` (`.1`, `.2` when that name is taken), starts a
    /// new one and deletes the oldest files beyond `keep`.
    fn roll(&self, open: &mut Open, now: SystemTime) -> io::Result<()> {
        let stamp = date(UNIX_EPOCH + Duration::from_secs(open.day * 86_400));
        let mut target = self.dir.join(format!("{NAME}.{stamp}"));
        for n in 1.. {
            if !target.exists() {
                break;
            }
            target = self.dir.join(format!("{NAME}.{stamp}.{n}"));
        }
        open.file.flush()?;
        fs::rename(self.dir.join(NAME), target)?;
        open.file = create(&self.dir.join(NAME))?;
        open.day = day_of(now);
        open.size = 0;

        let prefix = format!("{NAME}.");
        let mut old: Vec<String> = fs::read_dir(&self.dir)?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        old.sort_unstable_by(|a, b| b.cmp(a));
        for name in old.into_iter().skip(self.keep.saturating_sub(1)) {
            let _ = fs::remove_file(self.dir.join(name));
        }
        Ok(())
    }

    pub fn write_at(&self, buf: &[u8], now: SystemTime) -> io::Result<usize> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if day_of(now) != open.day || open.size + buf.len() as u64 > self.max_bytes {
            self.roll(&mut open, now)?;
        }
        open.file.write_all(buf)?;
        open.size += buf.len() as u64;
        Ok(buf.len())
    }
}

impl Write for &RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_at(buf, SystemTime::now())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(day * 86_400 + 60)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut n: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        n.sort();
        n
    }

    #[test]
    fn a_new_day_rotates_and_only_the_newest_files_stay() {
        let tmp = tempfile::tempdir().unwrap();
        let log = RotatingLog::open(tmp.path(), 3, MAX_BYTES, at(20_000)).unwrap();
        for day in 20_000..20_005 {
            log.write_at(format!("day {day}\n").as_bytes(), at(day)).unwrap();
        }
        let files = names(tmp.path());
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(files[0], "aibus.log");
        assert_eq!(fs::read_to_string(tmp.path().join("aibus.log")).unwrap(), "day 20004\n");
        // The two newest rotated files remain.
        assert!(files[1] < files[2]);
        let newest = &files[2];
        assert_eq!(
            fs::read_to_string(tmp.path().join(newest)).unwrap(),
            "day 20003\n"
        );
    }

    #[test]
    fn the_size_guard_rotates_early_and_files_are_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("logs");
        let log = RotatingLog::open(&dir, 7, 10, at(20_000)).unwrap();
        log.write_at(b"123456", at(20_000)).unwrap();
        log.write_at(b"789012", at(20_000)).unwrap();
        let files = names(&dir);
        assert_eq!(files.len(), 2, "{files:?}");
        assert_eq!(fs::read_to_string(dir.join("aibus.log")).unwrap(), "789012");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(dir.join("aibus.log")), 0o600);
            assert_eq!(mode(dir.clone()), 0o700);
        }
    }

    #[test]
    fn a_file_from_an_earlier_day_is_rotated_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("aibus.log"), "old\n").unwrap();
        // The file's mtime is now; ask for "tomorrow" so it counts as yesterday's.
        let tomorrow = SystemTime::now() + Duration::from_secs(2 * 86_400);
        let log = RotatingLog::open(tmp.path(), 7, MAX_BYTES, tomorrow).unwrap();
        log.write_at(b"new\n", tomorrow).unwrap();
        assert_eq!(names(tmp.path()).len(), 2);
        assert_eq!(fs::read_to_string(tmp.path().join("aibus.log")).unwrap(), "new\n");
    }
}
