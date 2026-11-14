//! Append-only zstd line files (07 section 2.1): `commits.jsonl.zst` and `msgs.jsonl.zst`.
//!
//! Lines are buffered and written as one zstd frame every `FLUSH_LINES` lines or `FLUSH_EVERY`.
//! Frames concatenate, so the file is valid after every flush and a crash loses at most the
//! lines still buffered.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub const FLUSH_LINES: usize = 100;
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);
pub const LEVEL: i32 = 3;

pub struct FrameWriter {
    file: File,
    pending: Vec<u8>,
    lines: usize,
    last_flush: Instant,
}

impl FrameWriter {
    /// Opens `path` for appending (mode `0600`); the clock for the 2 s rule starts at `now`.
    pub fn open(path: &Path, now: Instant) -> io::Result<Self> {
        let mut opts = std::fs::OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        Ok(Self {
            file: opts.open(path)?,
            pending: Vec::new(),
            lines: 0,
            last_flush: now,
        })
    }

    pub fn append(&mut self, line: &str, now: Instant) -> io::Result<()> {
        self.pending.extend_from_slice(line.as_bytes());
        self.pending.push(b'\n');
        self.lines += 1;
        if self.lines >= FLUSH_LINES {
            return self.flush(now);
        }
        self.flush_if_stale(now)
    }

    /// Flushes when something has been waiting for `FLUSH_EVERY`; the driver calls it on idle.
    pub fn flush_if_stale(&mut self, now: Instant) -> io::Result<()> {
        if self.lines > 0 && now.duration_since(self.last_flush) >= FLUSH_EVERY {
            return self.flush(now);
        }
        Ok(())
    }

    pub fn flush(&mut self, now: Instant) -> io::Result<()> {
        self.last_flush = now;
        if self.lines == 0 {
            return Ok(());
        }
        let frame = zstd::bulk::compress(&self.pending, LEVEL)?;
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.pending.clear();
        self.lines = 0;
        Ok(())
    }
}

/// Every complete line of a frame file. A frame cut short by a crash ends the read there:
/// what decoded before it still counts.
pub fn read_lines(path: &Path) -> io::Result<Vec<String>> {
    let mut decoder = zstd::stream::read::Decoder::new(File::open(path)?)?;
    let mut raw = Vec::new();
    let _ = decoder.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw);
    let complete = text.rfind('\n').map_or(0, |i| i + 1);
    Ok(text[..complete].lines().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_flush_every_100_or_after_2_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.jsonl.zst");
        let t0 = Instant::now();
        let mut w = FrameWriter::open(&path, t0).unwrap();
        for i in 0..99 {
            w.append(&format!("{{\"i\":{i}}}"), t0).unwrap();
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            0,
            "99 lines stay buffered"
        );
        w.append("{\"i\":99}", t0).unwrap();
        assert_eq!(read_lines(&path).unwrap().len(), 100);

        w.append("{\"i\":100}", t0).unwrap();
        w.flush_if_stale(t0 + Duration::from_millis(1999)).unwrap();
        assert_eq!(read_lines(&path).unwrap().len(), 100);
        w.flush_if_stale(t0 + Duration::from_secs(2)).unwrap();
        let lines = read_lines(&path).unwrap();
        assert_eq!(lines.len(), 101);
        assert_eq!(lines[100], "{\"i\":100}");
    }

    #[test]
    fn a_frame_cut_short_keeps_the_frames_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.jsonl.zst");
        let t0 = Instant::now();
        let mut w = FrameWriter::open(&path, t0).unwrap();
        w.append("one", t0).unwrap();
        w.flush(t0).unwrap();
        let whole = std::fs::metadata(&path).unwrap().len();
        w.append("two", t0).unwrap();
        w.flush(t0).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(whole + 3).unwrap();
        assert_eq!(read_lines(&path).unwrap(), ["one"]);
    }
}
