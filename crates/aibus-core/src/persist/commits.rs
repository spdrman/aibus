//! The commit log (07 section 2.1): `commits.jsonl.zst`, one line per commit that changed an
//! input, rotated after `ROTATE_AT` lines into `commits.0001.jsonl.zst` and so on.
//!
//! A line carries the changed inputs with their values (a `Commit` alone names nodes by index
//! and could not rebuild anything), so the last snapshot plus the lines after its time is the
//! whole session.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aibus_graph::{GraphTime, NodeKey, Value};
use serde::{Deserialize, Serialize};

use super::frames::{read_lines, FrameWriter};

pub const ROTATE_AT: usize = 50_000;
pub const CURRENT: &str = "commits.jsonl.zst";

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CommitRecord {
    pub time: GraphTime,
    pub intent: String,
    pub changed: Vec<(NodeKey, Value)>,
    /// RFC 3339.
    pub at: String,
}

pub struct CommitLog {
    dir: PathBuf,
    writer: FrameWriter,
    lines: usize,
    rotate_at: usize,
}

fn rotated(dir: &Path, n: usize) -> PathBuf {
    dir.join(format!("commits.{n:04}.jsonl.zst"))
}

/// Rotated files in order, then the current one.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = (1..)
        .map(|n| rotated(dir, n))
        .take_while(|p| p.is_file())
        .collect();
    let current = dir.join(CURRENT);
    if current.is_file() {
        found.push(current);
    }
    found
}

impl CommitLog {
    /// Appends to what is already in `dir` (a resumed session).
    pub fn open(dir: &Path, rotate_at: usize, now: Instant) -> io::Result<Self> {
        let current = dir.join(CURRENT);
        let lines = if current.is_file() {
            read_lines(&current)?.len()
        } else {
            0
        };
        Ok(Self {
            dir: dir.to_owned(),
            writer: FrameWriter::open(&current, now)?,
            lines,
            rotate_at,
        })
    }

    pub fn append(&mut self, rec: &CommitRecord, now: Instant) -> io::Result<()> {
        if self.lines >= self.rotate_at {
            self.writer.flush(now)?;
            let next = files(&self.dir).len();
            fs::rename(self.dir.join(CURRENT), rotated(&self.dir, next))?;
            self.writer = FrameWriter::open(&self.dir.join(CURRENT), now)?;
            self.lines = 0;
        }
        let line = serde_json::to_string(rec).map_err(io::Error::other)?;
        self.lines += 1;
        self.writer.append(&line, now)
    }

    pub fn flush_if_stale(&mut self, now: Instant) -> io::Result<()> {
        self.writer.flush_if_stale(now)
    }

    pub fn flush(&mut self, now: Instant) -> io::Result<()> {
        self.writer.flush(now)
    }
}

/// Every logged commit of the session in `dir`, oldest first.
pub fn read_all(dir: &Path) -> io::Result<Vec<CommitRecord>> {
    let mut out = Vec::new();
    for file in files(dir) {
        for line in read_lines(&file)? {
            out.push(serde_json::from_str(&line).map_err(io::Error::other)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(time: u64) -> CommitRecord {
        CommitRecord {
            time,
            intent: "Tick".into(),
            changed: vec![("eng:tick".into(), Value::I64(time as i64))],
            at: "2026-11-08T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn the_log_rotates_after_rotate_at_lines_and_reads_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let t = Instant::now();
        let mut log = CommitLog::open(dir.path(), 10, t).unwrap();
        for i in 1..=25 {
            log.append(&rec(i), t).unwrap();
        }
        log.flush(t).unwrap();
        assert!(dir.path().join("commits.0001.jsonl.zst").is_file());
        assert!(dir.path().join("commits.0002.jsonl.zst").is_file());
        assert!(!dir.path().join("commits.0003.jsonl.zst").exists());
        let all = read_all(dir.path()).unwrap();
        assert_eq!(all.iter().map(|r| r.time).collect::<Vec<_>>(), (1..=25).collect::<Vec<_>>());
        assert_eq!(all[24], rec(25));
    }

    #[test]
    fn reopening_continues_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let t = Instant::now();
        let mut log = CommitLog::open(dir.path(), 4, t).unwrap();
        for i in 1..=3 {
            log.append(&rec(i), t).unwrap();
        }
        log.flush(t).unwrap();
        let mut log = CommitLog::open(dir.path(), 4, t).unwrap();
        log.append(&rec(4), t).unwrap();
        log.append(&rec(5), t).unwrap();
        log.flush(t).unwrap();
        assert!(dir.path().join("commits.0001.jsonl.zst").is_file());
        assert_eq!(read_all(dir.path()).unwrap().len(), 5);
    }
}
