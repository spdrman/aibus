//! Private files: everything aibus writes under a session is readable by its owner only
//! (07 section 1). Directories get `0700`, files `0600`; other platforms get the defaults.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// `create_dir_all` with mode `0700` on every directory it creates.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Writes `data` to `path` with mode `0600`, replacing what was there. A file that already
/// existed with a wider mode is narrowed too.
pub fn write_private(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path.as_ref())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(data.as_ref())
}

/// Writes `data` next to `path`, syncs it, and renames it over `path`, so a reader (or a crash)
/// sees the old file or the new one and never half of either. Mode `0600`.
pub fn write_atomic(path: &Path, data: impl AsRef<[u8]>) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?
        .to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let written = (|| {
        let mut file = open_private(&tmp, true)?;
        file.write_all(data.as_ref())?;
        file.sync_all()
    })();
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // The rename itself is only durable once the directory is synced; best effort.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Replaces `path` while holding an exclusive `fs4` lock on it, for the files two aibus
/// processes may write at once (ledgers, handoffs; 07 section 5). The lock is held for this one
/// write.
pub fn write_locked(path: &Path, data: impl AsRef<[u8]>) -> io::Result<()> {
    use fs4::fs_std::FileExt;
    let mut file = open_private(path, false)?;
    file.lock_exclusive()?;
    let done = (|| {
        file.set_len(0)?;
        file.write_all(data.as_ref())?;
        file.sync_all()
    })();
    FileExt::unlock(&file)?;
    done
}

/// Reads `path` while holding a shared lock, so a writer in another process is never seen
/// half way through its write. `None` for a file that does not exist.
pub fn read_locked(path: &Path) -> io::Result<Option<String>> {
    use fs4::fs_std::FileExt;
    use std::io::Read;
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    FileExt::lock_shared(&file)?;
    let mut text = String::new();
    let done = file.read_to_string(&mut text);
    FileExt::unlock(&file)?;
    done.map(|_| Some(text))
}

/// Read, change and write back under one exclusive lock, for the files two processes may
/// change at once (a ledger section, `LEARNINGS.md`). `change` gets the current text (empty for
/// a file that did not exist) and returns the new one; its error leaves the file alone.
pub fn update_locked(
    path: &Path,
    change: impl FnOnce(&str) -> io::Result<String>,
) -> io::Result<()> {
    use fs4::fs_std::FileExt;
    use std::io::{Read, Seek, SeekFrom};
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    FileExt::lock_exclusive(&file)?;
    let done = (|| {
        let mut old = String::new();
        file.read_to_string(&mut old)?;
        let new = change(&old)?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(new.as_bytes())?;
        file.sync_all()
    })();
    FileExt::unlock(&file)?;
    done
}

/// FNV-1a over `bytes` as 16 hex digits. Stable across Rust versions and platforms, which
/// `DefaultHasher` is not, so a hash stored in `index.sqlite` still means something next year.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn open_private(path: &Path, truncate: bool) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn dirs_are_0700_at_every_level_and_files_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a/b/c");
        create_private_dir(&deep).unwrap();
        for d in ["a", "a/b", "a/b/c"] {
            assert_eq!(mode(&tmp.path().join(d)), 0o700, "{d}");
        }
        let f = deep.join("session.json");
        write_private(&f, "{}").unwrap();
        assert_eq!(mode(&f), 0o600);
    }

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temp_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("session.json");
        write_atomic(&f, "one").unwrap();
        write_atomic(&f, "two").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "two");
        assert_eq!(mode(&f), 0o600);
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn locked_writers_in_two_threads_never_interleave() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("ledger.md");
        let writers: Vec<_> = ["a", "b"]
            .map(|c| {
                let f = f.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        write_locked(&f, c.repeat(4096)).unwrap();
                    }
                })
            })
            .into();
        for w in writers {
            w.join().unwrap();
        }
        let text = fs::read_to_string(&f).unwrap();
        assert_eq!(text.len(), 4096);
        assert!(text.chars().all(|c| c == text.chars().next().unwrap()));
        assert_eq!(mode(&f), 0o600);
    }

    #[test]
    fn a_wider_existing_file_is_narrowed() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("x");
        fs::write(&f, "old contents").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&f, "new").unwrap();
        assert_eq!(mode(&f), 0o600);
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
    }
}
