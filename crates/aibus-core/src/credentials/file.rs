//! Reading a key from a file (04 section 5): refused unless only the owner can read it.

use std::path::Path;

use crate::error::CredentialError;

/// The file's text without its trailing newline. On Unix the mode must be `0600` or stricter
/// and the owner the current user.
pub(super) fn read_key(path: &Path) -> Result<String, CredentialError> {
    let text = read_private(path)?;
    Ok(text.trim_end_matches(['\n', '\r']).to_owned())
}

/// The static keys of `profile` in an AWS credentials file (`aws_access_key_id`,
/// `aws_secret_access_key` and, when there is one, `aws_session_token`), from a file only the
/// owner can read. `None` when the profile or one of its two keys is missing. No SSO, no
/// `credential_process`: those lines are not read.
pub(super) fn aws_profile(path: &Path, profile: &str) -> Result<Option<(String, String, Option<String>)>, CredentialError> {
    let text = read_private(path)?;
    let (mut inside, mut id, mut secret, mut token) = (false, None, None, None);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(['#', ';'])) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name.trim() == profile;
        } else if let (true, Some((k, v))) = (inside, line.split_once('=')) {
            let v = Some(v.trim().to_owned()).filter(|v| !v.is_empty());
            match k.trim() {
                "aws_access_key_id" => id = v,
                "aws_secret_access_key" => secret = v,
                "aws_session_token" => token = v,
                _ => {}
            }
        }
    }
    Ok(id.zip(secret).map(|(id, secret)| (id, secret, token)))
}

fn read_private(path: &Path) -> Result<String, CredentialError> {
    let unreadable =
        |e: std::io::Error| CredentialError::Unreadable(path.to_owned(), e.kind().to_string());
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(CredentialError::FileMode(path.to_owned(), meta.mode() & 0o777));
        }
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(CredentialError::FileOwner(path.to_owned()));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    std::fs::read_to_string(path).map_err(unreadable)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn key_file(mode: u32, text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, path)
    }

    #[test]
    fn a_0600_file_is_read_without_its_newline() {
        let (_d, p) = key_file(0o600, "sk-file-key\n");
        assert_eq!(read_key(&p).unwrap(), "sk-file-key");
    }

    #[test]
    fn a_stricter_mode_is_fine() {
        let (_d, p) = key_file(0o400, "k");
        assert_eq!(read_key(&p).unwrap(), "k");
    }

    #[test]
    fn group_or_world_access_is_refused_with_the_fix() {
        for mode in [0o640, 0o604, 0o644, 0o660] {
            let (_d, p) = key_file(mode, "k");
            let e = read_key(&p).unwrap_err();
            assert!(matches!(e, CredentialError::FileMode(_, m) if m == mode), "{mode:o}");
            assert!(e.to_string().contains(&format!("chmod 600 {}", p.display())));
        }
    }

    #[test]
    fn a_missing_file_is_unreadable() {
        let e = read_key(Path::new("/nonexistent/key")).unwrap_err();
        assert!(matches!(e, CredentialError::Unreadable(..)));
    }
}
