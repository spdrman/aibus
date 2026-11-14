//! Fixtures that build state: temp XDG directories and the reference session of 10 section 9.

use std::path::{Path, PathBuf};

use aibus_core::persist::paths::Paths;
use aibus_core::*;
use aibus_graph::Graph;
use aibus_provider::fake::FAKE_KEY;
use aibus_provider::StreamChunk;
use base64::Engine as _;
use tempfile::TempDir;

use crate::tui::{run_id, NOW};

/// A throw-away `$HOME` with the four XDG directories under it. Nothing reads the process
/// environment: `paths` is resolved from the temp tree, so tests can run side by side.
///
/// Dropping it greps every file below for the fake key (and any key added with `forbid`) and
/// panics when one is there, unless the test is already panicking. That is 04 section 7
/// condition 1, applied after every test that holds one.
pub struct Xdg {
    dir: TempDir,
    pub paths: Paths,
    forbidden: Vec<String>,
    /// A log capture for the test unless it already runs one, so the log grep of 10 section 3
    /// is on for every test that makes a directory.
    _logs: Option<crate::LogCapture>,
}

/// `.path()` and the rest of `tempfile::TempDir`, so a test that only needs a directory can
/// use this one.
impl std::ops::Deref for Xdg {
    type Target = TempDir;
    fn deref(&self) -> &TempDir {
        &self.dir
    }
}

impl AsRef<Path> for Xdg {
    fn as_ref(&self) -> &Path {
        self.dir.path()
    }
}

pub fn xdg_temp() -> Xdg {
    let dir = TempDir::new().expect("a temp dir");
    let home = dir.path().join("home").display().to_string();
    let paths = Paths::resolve(|k| (k == "HOME").then(|| home.clone())).expect("HOME is set");
    for p in [&paths.config, &paths.data, &paths.state, &paths.cache] {
        std::fs::create_dir_all(p).expect("an XDG directory");
    }
    Xdg {
        dir,
        paths,
        forbidden: std::iter::once(FAKE_KEY)
            .chain(crate::CREDENTIAL_KINDS.iter().map(|k| k.1))
            .map(str::to_owned)
            .collect(),
        _logs: crate::capture_unless_live(),
    }
}

impl Xdg {
    /// The temp root; `HOME` is `root()/home`.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Adds another key (an account's own) to the ones that must never reach a file.
    pub fn forbid(&mut self, secret: &str) {
        self.forbidden.push(secret.to_owned());
    }

    /// Panics, naming the file, if any forbidden key is in a file below the root. Files that
    /// end in `.zst` are decompressed first, since transcripts are stored that way.
    pub fn assert_no_secret_on_disk(&self) {
        let mut todo: Vec<PathBuf> = vec![self.dir.path().to_path_buf()];
        while let Some(p) = todo.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    todo.extend(rd.flatten().map(|e| e.path()));
                }
                continue;
            }
            let Ok(mut bytes) = std::fs::read(&p) else {
                continue;
            };
            if p.extension().is_some_and(|e| e == "zst") {
                if let Ok(plain) = zstd::decode_all(bytes.as_slice()) {
                    bytes = plain;
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            for secret in &self.forbidden {
                let b64 = base64::engine::general_purpose::STANDARD.encode(secret);
                assert!(
                    !text.contains(secret.as_str()) && !text.contains(&b64),
                    "a secret reached {} (as it is or in base64)",
                    p.display()
                );
            }
        }
    }
}

impl Drop for Xdg {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.assert_no_secret_on_disk();
        }
    }
}

fn line(i: usize) -> String {
    format!("line {i:06} the quick brown fox jumps over the lazy dog and keeps going a while\n")
}

fn stream(run: RunId, text: String) -> Msg {
    Msg::Stream {
        run,
        chunks: vec![StreamChunk::TextDelta(text)],
    }
}

/// The reference session of 03 section 11: eight runs, 100,000 transcript lines (about 7 MB)
/// on the first, 500 lines on each of the others, and 2,000 log rows. Run ids, the clock and
/// every text are fixed, so two calls leave the same snapshot hash. Generic over the backend
/// so the conformance and bench code can build it on each.
pub fn big_session<G: Graph>(mk: fn() -> G) -> (App<G>, Vec<RunId>) {
    let mut app = App::new(mk(), || NOW);
    let runs: Vec<RunId> = (1..=8).map(run_id).collect();
    for r in &runs {
        app.step(Msg::RunStarted {
            run: *r,
            agent: "worker".parse().unwrap(),
            role: Role::Worker,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("a".into()),
        })
        .unwrap();
    }
    let mut chunk = String::new();
    for i in 0..100_000 {
        chunk.push_str(&line(i));
        if chunk.len() >= 4096 {
            app.step(stream(runs[0], std::mem::take(&mut chunk)))
                .unwrap();
        }
    }
    app.step(stream(runs[0], chunk)).unwrap();
    for r in &runs[1..] {
        app.step(stream(*r, line(0).repeat(500))).unwrap();
    }
    for i in 0..2000 {
        app.step(Msg::IoError {
            context: format!("ctx {i}"),
            error: "e".into(),
        })
        .unwrap();
    }
    (app, runs)
}
