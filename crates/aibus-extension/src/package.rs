//! Reading a package: a directory somebody else made. Every name, size and file type is hostile
//! until checked, and the bytes are read once, hashed once and kept, so what is hashed is what is
//! installed (no check-then-use gap).

use std::collections::BTreeMap;
use std::path::Path;

use crate::manifest::{Manifest, ManifestError, MANIFEST_FILE};

pub const MAX_FILES: usize = 256;
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_DEPTH: usize = 6;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error("the package has no aibus-extension.toml")]
    NoManifest,
    #[error("{0} is a link; packages hold plain files and directories only")]
    Link(String),
    #[error("{0} is not a plain file or directory")]
    Special(String),
    #[error("a name in the package is not allowed: {0}")]
    BadName(String),
    #[error("{0} and {1} differ only by case")]
    CaseCollision(String, String),
    #[error("more than {MAX_FILES} files")]
    TooManyFiles,
    #[error("{0} is over {MAX_FILE_BYTES} bytes")]
    FileTooBig(String),
    #[error("the package is over {MAX_TOTAL_BYTES} bytes")]
    TooBig,
    #[error("{0} is nested deeper than {MAX_DEPTH} directories")]
    TooDeep(String),
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("io: {0}")]
    Io(String),
}

/// A package: its manifest, every file's bytes by relative path, and its content hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub manifest: Manifest,
    pub files: BTreeMap<String, Vec<u8>>,
    /// `sha256-tree-v1:` and 64 hex digits: sha256 over the sorted lines `<path> NUL <sha256 of the
    /// file> LF`.
    pub hash: String,
}

fn check_name(path: &str) -> Result<(), PackageError> {
    let bad = || {
        PackageError::BadName(
            path.chars()
                .take(80)
                .flat_map(char::escape_default)
                .collect(),
        )
    };
    if path.is_empty() || path.len() > 200 {
        return Err(bad());
    }
    let part = regex::Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9._-]{0,99}$").expect("valid regex");
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|p| !part.is_match(p)) {
        return Err(bad());
    }
    if parts.len() - 1 > MAX_DEPTH {
        return Err(PackageError::TooDeep(path.to_owned()));
    }
    Ok(())
}

impl Package {
    /// From files already in memory: checks every name and size, parses the manifest, hashes.
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self, PackageError> {
        let mut seen: BTreeMap<String, &String> = BTreeMap::new();
        let mut total = 0u64;
        if files.len() > MAX_FILES {
            return Err(PackageError::TooManyFiles);
        }
        for (path, bytes) in &files {
            check_name(path)?;
            if let Some(other) = seen.insert(path.to_lowercase(), path) {
                return Err(PackageError::CaseCollision(other.clone(), path.clone()));
            }
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(PackageError::FileTooBig(path.clone()));
            }
            total += bytes.len() as u64;
            if total > MAX_TOTAL_BYTES {
                return Err(PackageError::TooBig);
            }
        }
        let raw = files.get(MANIFEST_FILE).ok_or(PackageError::NoManifest)?;
        let text = std::str::from_utf8(raw)
            .map_err(|_| ManifestError::Syntax("the manifest is not UTF-8".into()))?;
        let manifest = Manifest::parse(text)?;
        for f in &manifest.fixtures.files {
            if !files.contains_key(f) {
                return Err(ManifestError::Invalid {
                    field: "fixtures.files".into(),
                    why: format!("{f} is not in the package"),
                }
                .into());
            }
        }
        // The hash of a package: sha256 over the sorted lines `<path> NUL <sha256 of the file> LF`.
        let mut listing = Vec::new();
        for (path, bytes) in &files {
            listing.extend_from_slice(path.as_bytes());
            listing.push(0);
            listing.extend_from_slice(aibus_trace::sha256_hex(bytes).as_bytes());
            listing.push(b'\n');
        }
        let hash = format!("sha256-tree-v1:{}", aibus_trace::sha256_hex(&listing));
        Ok(Self {
            manifest,
            files,
            hash,
        })
    }

    /// Writes the files under `dir` (which must not exist yet), `0600` files and `0700` directories.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let private_dir = |d: &Path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(d)
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir(d)
            }
        };
        private_dir(dir)?;
        for (path, bytes) in &self.files {
            let target = dir.join(path);
            if let Some(parent) = target.parent().filter(|p| !p.exists()) {
                let mut missing = vec![parent.to_path_buf()];
                let mut up = parent.parent();
                while let Some(p) = up.filter(|p| !p.exists()) {
                    missing.push(p.to_path_buf());
                    up = p.parent();
                }
                for d in missing.iter().rev() {
                    private_dir(d)?;
                }
            }
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(&target)?.write_all(bytes)?;
        }
        Ok(())
    }
}

fn io(e: &std::io::Error) -> PackageError {
    PackageError::Io(e.to_string())
}

fn walk(
    dir: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
    total: &mut u64,
) -> Result<(), PackageError> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| io(&e))? {
        let entry = entry.map_err(|e| io(&e))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            PackageError::BadName(
                name.to_string_lossy()
                    .chars()
                    .flat_map(char::escape_default)
                    .collect(),
            )
        })?;
        names.push(name.to_owned());
    }
    names.sort();
    for name in names {
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        check_name(&rel)?;
        let path = dir.join(&name);
        let kind = std::fs::symlink_metadata(&path)
            .map_err(|e| io(&e))?
            .file_type();
        if kind.is_symlink() {
            return Err(PackageError::Link(rel));
        } else if kind.is_dir() {
            walk(&path, &rel, files, total)?;
        } else if kind.is_file() {
            if files.len() >= MAX_FILES {
                return Err(PackageError::TooManyFiles);
            }
            // One regular file, opened without following a link and read up to the cap.
            let bytes = {
                use std::io::Read;
                let mut opts = std::fs::OpenOptions::new();
                opts.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.custom_flags(
                        (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
                    );
                }
                let file = opts.open(&path).map_err(|e| {
                    // O_NOFOLLOW on a link that was swapped in after the lstat.
                    if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
                        PackageError::Link(rel.clone())
                    } else {
                        io(&e)
                    }
                })?;
                let meta = file.metadata().map_err(|e| io(&e))?;
                if !meta.is_file() {
                    return Err(PackageError::Special(rel));
                }
                if meta.len() > MAX_FILE_BYTES {
                    return Err(PackageError::FileTooBig(rel));
                }
                let mut bytes = Vec::new();
                file.take(MAX_FILE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| io(&e))?;
                if bytes.len() as u64 > MAX_FILE_BYTES {
                    return Err(PackageError::FileTooBig(rel));
                }
                bytes
            };
            *total += bytes.len() as u64;
            if *total > MAX_TOTAL_BYTES {
                return Err(PackageError::TooBig);
            }
            files.insert(rel, bytes);
        } else {
            return Err(PackageError::Special(rel));
        }
    }
    Ok(())
}

/// Reads a package directory without following a link anywhere.
pub fn read(dir: &Path) -> Result<Package, PackageError> {
    let shown = dir.display().to_string();
    let kind = std::fs::symlink_metadata(dir)
        .map_err(|_| PackageError::NotADirectory(shown.clone()))?
        .file_type();
    if kind.is_symlink() {
        return Err(PackageError::Link(shown));
    }
    if !kind.is_dir() {
        return Err(PackageError::NotADirectory(shown));
    }
    let (mut files, mut total) = (BTreeMap::new(), 0u64);
    walk(dir, "", &mut files, &mut total)?;
    Package::from_files(files)
}
