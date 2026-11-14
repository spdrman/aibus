//! `.aibus` bundles (#271): a set of workflows in one file, for packaging and sharing.
//!
//! A bundle is a plain, uncompressed tar (POSIX ustar) with a `.aibus` extension, the way a
//! `.docx` is a zip. The layout is fixed and nothing else is read:
//!
//! ```text
//! manifest.toml            format = 1, name, description, workflows = [...], agents = [...]
//! workflows/<name>.toml    one per workflow the manifest lists
//! agents/<name>.md         optional, one per agent the manifest lists
//! README or README.md      optional
//! ```
//!
//! A bundle is input from someone else, so it is read in memory, never unpacked to disk, with
//! hard limits, and refused whole at the first thing that is not exactly that layout.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{valid_name, validate, Workflow, MAX_STEPS};
use crate::agent::{def, DefSource, PermissionMode, Scope};
use crate::ids::AgentName;
use crate::persist::fs::{create_private_dir, write_atomic};
use crate::Redactor;

/// The file extension, without the dot. A bundle is `<name>.aibus`, never a bare `.aibus` (that
/// is the project directory's name).
pub const EXTENSION: &str = "aibus";

/// The `format` a manifest has to say.
pub const FORMAT: u32 = 1;

/// The most a bundle file may be, headers and padding included.
pub const MAX_BUNDLE_BYTES: usize = 1024 * 1024;

/// The most entries (files and the two directories) a bundle may hold.
pub const MAX_ENTRIES: usize = 64;

/// The most any one file inside a bundle may be: the `workflow_create` cap.
pub const MAX_FILE_BYTES: usize = super::MAX_BYTES;

/// `manifest.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub workflows: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

/// What a bundle holds once every check has passed. Texts are the exact bytes of the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub manifest: Manifest,
    /// `(name, text)` in the manifest's order.
    pub workflows: Vec<(String, String)>,
    /// `(name, text)` in the manifest's order.
    pub agents: Vec<(String, String)>,
    /// `(file name, text)`.
    pub readme: Option<(String, String)>,
}

/// Why a bundle was refused. Nothing of a refused bundle is listed, run or written.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    #[error("the bundle is {0} bytes, the most is {MAX_BUNDLE_BYTES}")]
    TooBig(u64),
    #[error("the bundle has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("the bundle is cut short or not a tar file: {0}")]
    Truncated(String),
    #[error("a tar header is not valid: {0}")]
    BadHeader(String),
    #[error("{0} is a link; a bundle holds plain files only")]
    Link(String),
    #[error("{0} is a device, fifo or other special file; a bundle holds plain files only")]
    Special(String),
    #[error("{0} is an absolute path")]
    Absolute(String),
    #[error("{0} climbs out with `..`")]
    Traversal(String),
    #[error("{0} is not part of the bundle layout (manifest.toml, workflows/<name>.toml, agents/<name>.md, README)")]
    Outside(String),
    #[error("{0} is in the bundle twice")]
    Duplicate(String),
    #[error("{0} is {1} bytes, the most for one file is {MAX_FILE_BYTES}")]
    EntryTooBig(String, u64),
    #[error("`{0}` is not a name: lowercase letters, digits, - and _, starting with a letter or digit, 64 at most")]
    BadName(String),
    #[error("{0} is not UTF-8 text")]
    NotUtf8(String),
    #[error("{0} contains something that looks like a credential")]
    Credential(String),
    #[error("manifest.toml: {0}")]
    Manifest(String),
    #[error("workflows/{0}.toml: {1}")]
    Workflow(String, String),
    #[error("agents/{0}.md: {1}")]
    Agent(String, String),
    #[error("{0}: {1}")]
    Io(String, String),
}

/// A tar block.
const BLOCK: usize = 512;

/// What one header says, once it has passed the header checks.
enum Kind {
    File,
    Dir,
}

/// Up to the first NUL. Bytes after it are refused: a name field that says one thing to this
/// reader and another to a tool that reads the whole field is a trick, not a tar.
fn field(raw: &[u8], what: &str) -> Result<Vec<u8>, BundleError> {
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    if raw[end..].iter().any(|b| *b != 0) {
        return Err(BundleError::BadHeader(format!(
            "the {what} field has bytes after its end"
        )));
    }
    Ok(raw[..end].to_vec())
}

/// An octal number field: leading spaces, digits, then NUL or space to the end. The base-256 form
/// (high bit set) exists only for sizes no bundle can have, so it is refused.
fn octal(raw: &[u8], what: &str) -> Result<u64, BundleError> {
    let bad = || BundleError::BadHeader(format!("the {what} field is not an octal number"));
    if raw.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(BundleError::BadHeader(format!(
            "the {what} field is in base-256 form"
        )));
    }
    let start = raw.iter().position(|b| *b != b' ').unwrap_or(raw.len());
    let digits = raw[start..]
        .iter()
        .take_while(|b| (b'0'..=b'7').contains(*b))
        .count();
    if digits == 0 || raw[start + digits..].iter().any(|b| *b != 0 && *b != b' ') {
        return Err(bad());
    }
    raw[start..start + digits]
        .iter()
        .try_fold(0u64, |n, d| {
            n.checked_mul(8)?.checked_add(u64::from(d - b'0'))
        })
        .ok_or_else(bad)
}

/// Where an entry sits in the layout: the key that makes two spellings of one thing a duplicate,
/// or the refusal. `path` has passed the absolute and `..` checks.
fn place(path: &str, kind: &Kind) -> Result<String, BundleError> {
    let outside = || BundleError::Outside(path.to_owned());
    if let Kind::Dir = kind {
        return match path.trim_end_matches('/') {
            d @ ("workflows" | "agents") => Ok(format!("{d}/")),
            _ => Err(outside()),
        };
    }
    match path {
        "manifest.toml" => return Ok(path.to_owned()),
        "README" | "README.md" => return Ok("README".to_owned()),
        _ => {}
    }
    let named = |dir: &str, ext: &str| -> Option<Result<String, BundleError>> {
        let stem = path.strip_prefix(dir)?.strip_suffix(ext)?;
        if stem.contains('/') {
            return Some(Err(outside()));
        }
        Some(if valid_name(stem) {
            Ok(path.to_owned())
        } else {
            Err(BundleError::BadName(stem.to_owned()))
        })
    };
    named("workflows/", ".toml")
        .or_else(|| named("agents/", ".md"))
        .unwrap_or_else(|| Err(outside()))
}

/// The entries of the tar, with every header check: `(path, data)` of each file in archive order.
fn entries(bytes: &[u8]) -> Result<Vec<(String, &[u8])>, BundleError> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(BundleError::TooBig(bytes.len() as u64));
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut at = 0usize;
    loop {
        let Some(h) = bytes.get(at..at + BLOCK) else {
            return Err(BundleError::Truncated(if at == 0 {
                "empty".into()
            } else {
                "it ends before the end-of-archive marker".into()
            }));
        };
        if h.iter().all(|b| *b == 0) {
            // The end: a second zero block, then nothing but zero padding.
            let Some(next) = bytes.get(at + BLOCK..at + 2 * BLOCK) else {
                return Err(BundleError::Truncated(
                    "the end-of-archive marker is one block short".into(),
                ));
            };
            if next.iter().any(|b| *b != 0) || bytes[at + 2 * BLOCK..].iter().any(|b| *b != 0) {
                return Err(BundleError::BadHeader(
                    "there is data after the end-of-archive marker".into(),
                ));
            }
            return Ok(out);
        }
        if seen.len() >= MAX_ENTRIES {
            return Err(BundleError::TooManyEntries);
        }
        let stored = octal(&h[148..156], "checksum")?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, b)| u64::from(if (148..156).contains(&i) { b' ' } else { *b }))
            .sum();
        if stored != sum {
            return Err(BundleError::BadHeader(
                "a header checksum does not match".into(),
            ));
        }
        if &h[257..263] != b"ustar\0" && &h[257..265] != b"ustar  \0" {
            return Err(BundleError::BadHeader("not a ustar header".into()));
        }
        let (name, prefix) = (field(&h[..100], "name")?, field(&h[345..500], "prefix")?);
        let raw = if prefix.is_empty() {
            name
        } else {
            [prefix, b"/".to_vec(), name].concat()
        };
        let path = String::from_utf8(raw).map_err(|e| {
            BundleError::NotUtf8(String::from_utf8_lossy(e.as_bytes()).into_owned())
        })?;
        let kind = match h[156] {
            b'0' | 0 => Kind::File,
            b'5' => Kind::Dir,
            b'1' | b'2' => return Err(BundleError::Link(path)),
            b'3' | b'4' | b'6' | b'7' => return Err(BundleError::Special(path)),
            other => {
                return Err(BundleError::BadHeader(format!(
                    "{path}: entry type {:?} is not a plain file",
                    other as char
                )))
            }
        };
        if path.is_empty() {
            return Err(BundleError::BadHeader("an entry has no name".into()));
        }
        if path.starts_with('/') {
            return Err(BundleError::Absolute(path));
        }
        if path.split('/').any(|c| c == "..") {
            return Err(BundleError::Traversal(path));
        }
        let key = place(&path, &kind)?;
        if !seen.insert(key) {
            return Err(BundleError::Duplicate(path));
        }
        let size = octal(&h[124..136], "size")?;
        if size > MAX_FILE_BYTES as u64 {
            return Err(BundleError::EntryTooBig(path, size));
        }
        let size = size as usize;
        if let Kind::Dir = kind {
            if size != 0 {
                return Err(BundleError::BadHeader(format!(
                    "{path} is a directory with data"
                )));
            }
            at += BLOCK;
            continue;
        }
        let data = bytes
            .get(at + BLOCK..at + BLOCK + size)
            .ok_or_else(|| BundleError::Truncated(format!("{path} is cut short")))?;
        at += BLOCK + size.div_ceil(BLOCK) * BLOCK;
        out.push((path, data));
    }
}

/// The manifest's list against what is in the bundle: both ways, no name twice.
fn matches(listed: &[String], present: &[(String, String)], what: &str) -> Result<(), BundleError> {
    let mut seen = BTreeSet::new();
    for n in listed {
        if !valid_name(n) {
            return Err(BundleError::BadName(n.clone()));
        }
        if !seen.insert(n.as_str()) {
            return Err(BundleError::Duplicate(format!(
                "{n} in the manifest's {what}"
            )));
        }
        if !present.iter().any(|(p, _)| p == n) {
            return Err(BundleError::Manifest(format!(
                "{what} lists {n}, which is not in the bundle"
            )));
        }
    }
    if let Some((p, _)) = present.iter().find(|(p, _)| !seen.contains(p.as_str())) {
        return Err(BundleError::Manifest(format!(
            "{p} is in the bundle and not in {what}"
        )));
    }
    Ok(())
}

/// Reads a bundle from its bytes. Every limit and refusal of the module comment applies, and
/// nothing is written anywhere.
pub fn read(bytes: &[u8], redactor: &Redactor) -> Result<Bundle, BundleError> {
    let mut manifest = None;
    let (mut workflows, mut agents, mut readme) = (Vec::new(), Vec::new(), None);
    for (path, data) in entries(bytes)? {
        let text = std::str::from_utf8(data).map_err(|_| BundleError::NotUtf8(path.clone()))?;
        if redactor.redact(text) != text {
            return Err(BundleError::Credential(path));
        }
        let text = text.to_owned();
        if path == "manifest.toml" {
            manifest = Some(text);
        } else if path.starts_with("README") {
            readme = Some((path, text));
        } else if let Some(n) = path
            .strip_prefix("workflows/")
            .and_then(|p| p.strip_suffix(".toml"))
        {
            workflows.push((n.to_owned(), text));
        } else if let Some(n) = path
            .strip_prefix("agents/")
            .and_then(|p| p.strip_suffix(".md"))
        {
            agents.push((n.to_owned(), text));
        }
    }
    let manifest =
        manifest.ok_or_else(|| BundleError::Manifest("there is no manifest.toml".into()))?;
    let manifest: Manifest =
        toml::from_str(&manifest).map_err(|e| BundleError::Manifest(e.message().to_owned()))?;
    if manifest.format != FORMAT {
        return Err(BundleError::Manifest(format!(
            "format {} is not one this aibus reads (it reads {FORMAT})",
            manifest.format
        )));
    }
    if !valid_name(&manifest.name) {
        return Err(BundleError::BadName(manifest.name));
    }
    if manifest.workflows.is_empty() {
        return Err(BundleError::Manifest("it lists no workflows".into()));
    }
    matches(&manifest.workflows, &workflows, "workflows")?;
    matches(&manifest.agents, &agents, "agents")?;
    for (name, text) in &workflows {
        let wf = Workflow::parse(name, text)
            .map_err(|e| BundleError::Workflow(name.clone(), e.to_string()))?;
        if wf.name != *name {
            return Err(BundleError::Workflow(
                name.clone(),
                format!(
                    "its name field says {:?}, so it would run under another name",
                    wf.name
                ),
            ));
        }
        if wf.steps.len() > MAX_STEPS {
            return Err(BundleError::Workflow(
                name.clone(),
                format!("{} steps, the most is {MAX_STEPS}", wf.steps.len()),
            ));
        }
    }
    for (name, text) in &agents {
        let source = DefSource {
            path: None,
            scope: Scope::Project,
        };
        let (d, _) = def::parse(text, source)
            .map_err(|e| BundleError::Agent(name.clone(), e.to_string()))?;
        if d.name.as_str() != name {
            return Err(BundleError::Agent(
                name.clone(),
                format!("the definition is named {}", d.name),
            ));
        }
    }
    let order = |list: &[String], mut found: Vec<(String, String)>| {
        list.iter()
            .filter_map(|n| {
                found
                    .iter()
                    .position(|(p, _)| p == n)
                    .map(|i| found.swap_remove(i))
            })
            .collect()
    };
    Ok(Bundle {
        workflows: order(&manifest.workflows, workflows),
        agents: order(&manifest.agents, agents),
        manifest,
        readme,
    })
}

/// Reads the bundle at `path`: a regular file of at most `MAX_BUNDLE_BYTES`, then `read`.
pub fn read_file(path: &Path, redactor: &Redactor) -> Result<Bundle, BundleError> {
    let shown = path.display().to_string();
    let io = |e: std::io::Error| BundleError::Io(shown.clone(), e.to_string());
    let file = std::fs::File::open(path).map_err(io)?;
    let md = file.metadata().map_err(io)?;
    if !md.is_file() {
        return Err(BundleError::Io(shown, "not a regular file".into()));
    }
    if md.len() > MAX_BUNDLE_BYTES as u64 {
        return Err(BundleError::TooBig(md.len()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BUNDLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    read(&bytes, redactor)
}

/// One ustar entry with fixed metadata.
fn put(out: &mut Vec<u8>, path: &str, data: &[u8]) {
    let mut h = [0u8; BLOCK];
    h[..path.len()].copy_from_slice(path.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
}

/// Builds a bundle: `manifest.toml`, then each workflow and agent, then the README, as ustar
/// entries with fixed metadata (mode 0644, owner 0, time 0), so the same input gives the same
/// bytes. The result is read back with `read` before it is returned, so `pack` never makes a
/// bundle that `install` would refuse for its shape.
pub fn pack(
    name: &str,
    description: &str,
    workflows: &[(String, String)],
    agents: &[(String, String)],
    readme: Option<(&str, &str)>,
    redactor: &Redactor,
) -> Result<Vec<u8>, BundleError> {
    for n in std::iter::once(name).chain(workflows.iter().chain(agents).map(|(n, _)| n.as_str())) {
        if !valid_name(n) {
            return Err(BundleError::BadName(n.to_owned()));
        }
    }
    if readme.is_some_and(|(f, _)| f != "README" && f != "README.md") {
        return Err(BundleError::Outside(
            readme.map(|(f, _)| f.to_owned()).unwrap_or_default(),
        ));
    }
    let manifest = Manifest {
        format: FORMAT,
        name: name.to_owned(),
        description: description.to_owned(),
        workflows: workflows.iter().map(|(n, _)| n.clone()).collect(),
        agents: agents.iter().map(|(n, _)| n.clone()).collect(),
    };
    let manifest = toml::to_string(&manifest).map_err(|e| BundleError::Manifest(e.to_string()))?;
    let mut out = Vec::new();
    put(&mut out, "manifest.toml", manifest.as_bytes());
    for (n, text) in workflows {
        put(&mut out, &format!("workflows/{n}.toml"), text.as_bytes());
    }
    for (n, text) in agents {
        put(&mut out, &format!("agents/{n}.md"), text.as_bytes());
    }
    if let Some((f, text)) = readme {
        put(&mut out, f, text.as_bytes());
    }
    out.extend_from_slice(&[0u8; 2 * BLOCK]);
    read(&out, redactor)?;
    Ok(out)
}

/// Where `install` writes and what it may write.
pub struct Install<'a> {
    /// The scope's `workflows` directory (`<project>/.aibus/workflows` or `<config>/workflows`).
    pub workflows_dir: PathBuf,
    /// The scope's `agents` directory.
    pub agents_dir: PathBuf,
    /// The agents a step may name, besides the bundle's own.
    pub roster: &'a [AgentName],
    /// Replace files that are already there.
    pub force: bool,
    /// Trust the bundle's gates (and agent hooks): the `--allow gates` of a write.
    pub allow_gates: bool,
}

/// Why `install` wrote nothing.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("workflows/{0}.toml does not validate: {1}")]
    Invalid(String, String),
    #[error("workflow {0}, step {1} has a gate, a shell command aibus runs as you ({2}); install with --allow gates to trust it")]
    Gate(String, usize, String),
    #[error("agent {0} has hooks or a permission mode, which run or allow commands as you; install with --allow gates to trust it")]
    AgentPowers(String),
    #[error("{} exist(s), install with --force to replace", .0.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    Exists(Vec<PathBuf>),
    #[error("{0} is not a plain file or directory, refusing to write through it")]
    NotPlain(PathBuf),
    #[error("{0}")]
    Io(String),
}

/// Unpacks a bundle into loose files, through the checks `workflow_create` makes (name, 64 KiB,
/// 32 steps, `validate` against the roster, credentials, the gate rule) plus the bundle's own.
/// Every check runs before anything touches the disk; then the files are written atomically with
/// mode 0600, byte for byte as they are in the bundle. Returns the paths written.
pub fn install(
    bytes: &[u8],
    to: &Install<'_>,
    redactor: &Redactor,
) -> Result<Vec<PathBuf>, InstallError> {
    let bundle = read(bytes, redactor)?;
    let mut roster = to.roster.to_vec();
    roster.extend(
        bundle
            .agents
            .iter()
            .filter_map(|(n, _)| AgentName::new(n).ok()),
    );
    for (name, text) in &bundle.workflows {
        let found = validate(text, name, &roster);
        if !found.ok {
            let lines: Vec<String> = found
                .errors
                .iter()
                .map(|p| {
                    format!(
                        "{}{}",
                        p.step.map_or(String::new(), |n| format!("step {n}: ")),
                        p.message
                    )
                })
                .collect();
            return Err(InstallError::Invalid(name.clone(), lines.join("; ")));
        }
        if !to.allow_gates {
            let wf = Workflow::parse(name, text)
                .map_err(|e| InstallError::Invalid(name.clone(), e.to_string()))?;
            if let Some((i, step)) = wf.steps.iter().enumerate().find(|(_, s)| s.gate.is_some()) {
                let command = step.gate.as_deref().unwrap_or_default().trim().to_owned();
                return Err(InstallError::Gate(name.clone(), i + 1, command));
            }
        }
    }
    for (name, text) in &bundle.agents {
        let source = DefSource {
            path: None,
            scope: Scope::Project,
        };
        let (d, _) = def::parse(text, source)
            .map_err(|e| InstallError::Invalid(name.clone(), e.to_string()))?;
        if !to.allow_gates && (!d.hooks.is_empty() || d.permission_mode != PermissionMode::Default)
        {
            return Err(InstallError::AgentPowers(name.clone()));
        }
    }

    let mut files: Vec<(PathBuf, &str)> = Vec::new();
    files.extend(
        bundle
            .workflows
            .iter()
            .map(|(n, t)| (to.workflows_dir.join(format!("{n}.toml")), t.as_str())),
    );
    files.extend(
        bundle
            .agents
            .iter()
            .map(|(n, t)| (to.agents_dir.join(format!("{n}.md")), t.as_str())),
    );
    let dirs: Vec<&Path> = std::iter::once(to.workflows_dir.as_path())
        .chain((!bundle.agents.is_empty()).then_some(to.agents_dir.as_path()))
        .collect();
    for dir in &dirs {
        match std::fs::symlink_metadata(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(InstallError::Io(format!("{}: {e}", dir.display()))),
            Ok(md) if !md.is_dir() => return Err(InstallError::NotPlain(dir.to_path_buf())),
            Ok(_) => {}
        }
    }
    let mut taken = Vec::new();
    for (path, _) in &files {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(InstallError::Io(format!("{}: {e}", path.display()))),
            Ok(md) if md.file_type().is_symlink() || !md.is_file() => {
                return Err(InstallError::NotPlain(path.clone()))
            }
            Ok(_) if !to.force => taken.push(path.clone()),
            Ok(_) => {}
        }
    }
    if !taken.is_empty() {
        return Err(InstallError::Exists(taken));
    }

    for dir in dirs {
        create_private_dir(dir).map_err(|e| InstallError::Io(format!("{}: {e}", dir.display())))?;
    }
    let mut wrote = Vec::new();
    for (path, text) in files {
        write_atomic(&path, text)
            .map_err(|e| InstallError::Io(format!("{}: {e}", path.display())))?;
        wrote.push(path);
    }
    Ok(wrote)
}
