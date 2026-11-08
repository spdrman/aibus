//! Private files: everything aibus writes under a session is readable by its owner only
//! (07 section 1). Directories get `0700`, files `0600`; other platforms get the defaults.

use std::fs;
use std::io;
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
    use io::Write;
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
