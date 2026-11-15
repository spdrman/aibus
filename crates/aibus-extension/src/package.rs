//! Reading a package: a directory somebody else made. Every name, size and file type is hostile
//! until checked, and the bytes are read once, hashed once and kept, so what is hashed is what is
//! installed (no check-then-use gap).

use std::collections::BTreeMap;
use std::path::Path;

use crate::manifest::{Manifest, ManifestError};

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
    /// `sha256-tree-v1:` and 64 hex digits (see `tree_hash`).
    pub hash: String,
}

/// The hash of a package: sha256 over the sorted lines `<path>\0<sha256 of the file>\n`.
pub fn tree_hash(files: &BTreeMap<String, Vec<u8>>) -> String {
    todo!("{files:?}")
}

impl Package {
    /// From files already in memory: checks every name and size, parses the manifest, hashes.
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self, PackageError> {
        todo!("{files:?}")
    }

    /// Writes the files under `dir` (which must not exist yet), `0600` files and `0700` directories.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        todo!("{dir:?}")
    }
}

/// Reads a package directory without following a link anywhere.
pub fn read(dir: &Path) -> Result<Package, PackageError> {
    todo!("{dir:?}")
}
