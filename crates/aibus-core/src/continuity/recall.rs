//! Recall (01 section 8.3, 07 section 2.3): a full-text index over memory files, learnings,
//! ledgers and handoffs. `MemoryIndex` is a trait so an embedding backend can sit next to the
//! FTS5 one `persist::index::Index` provides (10 section 3).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::params;
use serde::Serialize;

use super::Layout;
use crate::agent::MemoryScope;
use crate::persist::fs::content_hash;
use crate::persist::index::{io_err, Index};

/// Bytes of a file that go into the index.
const MAX_INDEXED: usize = 1 << 20;

/// One file the index covers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub scope: MemoryScope,
    /// `memory`, `learnings`, `ledger` or `handoff`.
    pub kind: &'static str,
}

/// What `sync` found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize)]
pub struct Synced {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

/// `[{path, score, snippet}]` (06 section 4); a higher score is a better match.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Hit {
    pub path: String,
    pub score: f64,
    pub snippet: String,
}

pub trait MemoryIndex {
    /// Brings the index in line with the files in scope: new and changed ones are read (a file
    /// whose mtime moved but whose hash did not is left alone), gone ones are dropped.
    fn sync(&mut self, layout: &Layout) -> io::Result<Synced>;

    /// The `k` best matches for `query` among the files in scope, best first.
    fn recall(&self, layout: &Layout, query: &str, k: usize) -> io::Result<Vec<Hit>>;
}

fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        // `file_type` does not follow a symlink, so a link never leads out of the tree.
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() {
            walk(&p, ext, out);
        } else if t.is_file() && p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

/// Every file the index covers for `layout`'s scopes: `agent-memory/**/*.md` (with
/// `LEARNINGS.md`), and for the project scope the ledgers and the handoffs.
pub fn sources(layout: &Layout) -> Vec<Source> {
    let mut out = Vec::new();
    let mut add = |dir: PathBuf, ext, scope, kind: Option<&'static str>| {
        let mut found = Vec::new();
        walk(&dir, ext, &mut found);
        found.sort();
        for path in found {
            let kind = kind.unwrap_or(if path.file_name().is_some_and(|n| n == "LEARNINGS.md") {
                "learnings"
            } else {
                "memory"
            });
            out.push(Source { path, scope, kind });
        }
    };
    for scope in &layout.scopes {
        match scope {
            MemoryScope::User => add(layout.data.join("agent-memory"), "md", *scope, None),
            MemoryScope::Project => {
                add(
                    layout.project.join(".aibus/agent-memory"),
                    "md",
                    *scope,
                    None,
                );
                add(layout.ledgers(), "md", *scope, Some("ledger"));
                add(layout.handoffs(), "yaml", *scope, Some("handoff"));
            }
            MemoryScope::Local => {
                add(
                    layout.project.join(".aibus/agent-memory-local"),
                    "md",
                    *scope,
                    None,
                );
            }
        }
    }
    out
}

/// `scope IN (...) AND (user, or under this project)` with `?1` the length of the project
/// prefix in characters and `?2` the prefix. The scope names are constants, never user text.
fn domain(layout: &Layout) -> String {
    let scopes: Vec<String> = layout
        .scopes
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect();
    format!(
        "scope IN ({}) AND (scope = 'user' OR substr(path, 1, ?1) = ?2)",
        scopes.join(", ")
    )
}

fn prefix(layout: &Layout) -> (i64, String) {
    let p = format!(
        "{}/",
        layout.project.to_string_lossy().trim_end_matches('/')
    );
    (p.chars().count() as i64, p)
}

fn mtime_ms(path: &Path) -> io::Result<i64> {
    let t = fs::metadata(path)?.modified()?;
    Ok(t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64))
}

/// FTS5 `MATCH` text for free text: each word quoted, any of them may match, content only.
fn match_text(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect();
    (!words.is_empty()).then(|| format!("content : ({})", words.join(" OR ")))
}

impl MemoryIndex for Index {
    fn sync(&mut self, layout: &Layout) -> io::Result<Synced> {
        let (n, pre) = prefix(layout);
        let found = sources(layout);
        let mut stats = Synced::default();
        if layout.scopes.is_empty() {
            return Ok(stats);
        }
        let tx = self.conn_mut().transaction().map_err(io_err)?;
        let mut known: HashMap<String, (i64, String)> = HashMap::new();
        {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT path, mtime, hash FROM memory_files WHERE {}",
                    domain(layout)
                ))
                .map_err(io_err)?;
            let rows = stmt
                .query_map(params![n, pre], |r| {
                    Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
                })
                .map_err(io_err)?;
            for row in rows {
                let (path, state) = row.map_err(io_err)?;
                known.insert(path, state);
            }
        }
        let live: HashSet<String> = found
            .iter()
            .map(|s| s.path.to_string_lossy().into_owned())
            .collect();
        for path in known.keys().filter(|p| !live.contains(*p)) {
            tx.execute("DELETE FROM memory_fts WHERE path = ?1", [path])
                .map_err(io_err)?;
            tx.execute("DELETE FROM memory_files WHERE path = ?1", [path])
                .map_err(io_err)?;
            stats.removed += 1;
        }
        for s in &found {
            let path = s.path.to_string_lossy().into_owned();
            let Ok(mtime) = mtime_ms(&s.path) else {
                continue;
            };
            let old = known.get(&path);
            if old.is_some_and(|(m, _)| *m == mtime) {
                stats.unchanged += 1;
                continue;
            }
            let Ok(bytes) = fs::read(&s.path) else {
                continue;
            };
            let hash = content_hash(&bytes);
            if old.is_some_and(|(_, h)| *h == hash) {
                tx.execute(
                    "UPDATE memory_files SET mtime = ?2 WHERE path = ?1",
                    params![path, mtime],
                )
                .map_err(io_err)?;
                stats.unchanged += 1;
                continue;
            }
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_INDEXED)]).into_owned();
            tx.execute("DELETE FROM memory_fts WHERE path = ?1", [&path])
                .map_err(io_err)?;
            tx.execute(
                "INSERT INTO memory_fts (path, scope, kind, content) VALUES (?1, ?2, ?3, ?4)",
                params![path, s.scope.as_str(), s.kind, text],
            )
            .map_err(io_err)?;
            tx.execute(
                "INSERT OR REPLACE INTO memory_files (path, scope, kind, mtime, hash) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![path, s.scope.as_str(), s.kind, mtime, hash],
            )
            .map_err(io_err)?;
            if old.is_some() {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
        }
        tx.commit().map_err(io_err)?;
        Ok(stats)
    }

    fn recall(&self, layout: &Layout, query: &str, k: usize) -> io::Result<Vec<Hit>> {
        let (Some(text), false) = (match_text(query), layout.scopes.is_empty() || k == 0) else {
            return Ok(Vec::new());
        };
        let (n, pre) = prefix(layout);
        let sql = format!(
            "SELECT path, -bm25(memory_fts), snippet(memory_fts, 3, '[', ']', '...', 16) \
             FROM memory_fts WHERE memory_fts MATCH ?3 AND {} ORDER BY bm25(memory_fts) LIMIT ?4",
            domain(layout)
        );
        let mut stmt = self.conn().prepare(&sql).map_err(io_err)?;
        let rows = stmt
            .query_map(params![n, pre, text, k as i64], |r| {
                Ok(Hit {
                    path: r.get(0)?,
                    score: r.get(1)?,
                    snippet: r
                        .get::<_, String>(2)?
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                })
            })
            .map_err(io_err)?;
        rows.map(|r| r.map_err(io_err)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    struct Fixture {
        _dir: tempfile::TempDir,
        layout: Layout,
        index: Index,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (project, data) = (dir.path().join("proj"), dir.path().join("data"));
        fs::create_dir_all(&project).unwrap();
        let mut cfg = Config::default();
        cfg.memory.scopes = vec!["user".into(), "project".into(), "local".into()];
        let layout = Layout::new(&project, &cfg, &data, Some("auth")).unwrap();
        let index = Index::open(&data.join("index.sqlite")).unwrap();
        Fixture {
            _dir: dir,
            layout,
            index,
        }
    }

    fn put(path: PathBuf, text: &str) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn everything_in_scope_is_indexed_and_ranked() {
        let mut f = fixture();
        let l = &f.layout;
        put(
            l.data.join("agent-memory/lead/LEARNINGS.md"),
            "- [fact, high] the cache key is the file hash\n",
        );
        put(
            l.project.join(".aibus/agent-memory/lead/MEMORY.md"),
            "notes about databases\n",
        );
        put(
            l.project.join(".aibus/agent-memory-local/lead/me.md"),
            "my private cache notes\n",
        );
        put(
            l.ledger(),
            "# Continuity: auth\n\n## Goal\nspeed up the cache\n",
        );
        put(
            l.handoff_dir().join("2026-11-08_09-15_x.yaml"),
            "goal: warm the cache\nnow: measure\n",
        );
        put(
            l.project.join("README.md"),
            "cache in the readme, which is not indexed\n",
        );
        let synced = f.index.sync(&f.layout).unwrap();
        assert_eq!(
            synced,
            Synced {
                added: 5,
                ..Synced::default()
            }
        );

        let hits = f.index.recall(&f.layout, "cache", 10).unwrap();
        let names: Vec<_> = hits
            .iter()
            .map(|h| {
                Path::new(&h.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(hits.len(), 4, "{names:?}");
        assert!(
            !names.contains(&"README.md".to_owned()) && !names.contains(&"MEMORY.md".to_owned())
        );
        assert!(
            hits.iter()
                .all(|h| h.score > 0.0 && h.snippet.to_lowercase().contains("[cache]")),
            "{hits:?}"
        );
        assert_eq!(f.index.recall(&f.layout, "cache", 2).unwrap().len(), 2);
        // Best first.
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn stemming_and_awkward_queries() {
        let mut f = fixture();
        put(
            f.layout.data.join("agent-memory/lead/a.md"),
            "the tests were running slowly\n",
        );
        f.index.sync(&f.layout).unwrap();
        assert_eq!(f.index.recall(&f.layout, "run test", 5).unwrap().len(), 1);
        for q in [
            "",
            "   ",
            "\"unbalanced",
            "a AND OR (",
            "NEAR(x",
            "c++ -d *",
            "content:x",
            "🙂",
        ] {
            assert!(f.index.recall(&f.layout, q, 5).is_ok(), "{q:?}");
        }
        assert!(f.index.recall(&f.layout, "slowly", 0).unwrap().is_empty());
    }

    #[test]
    fn a_sync_reads_only_what_changed() {
        let mut f = fixture();
        let a = put(f.layout.data.join("agent-memory/lead/a.md"), "alpha one\n");
        let b = put(f.layout.data.join("agent-memory/lead/b.md"), "beta two\n");
        f.index.sync(&f.layout).unwrap();
        assert_eq!(
            f.index.sync(&f.layout).unwrap(),
            Synced {
                unchanged: 2,
                ..Synced::default()
            }
        );

        // Same bytes, new mtime: untouched.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&a, "alpha one\n").unwrap();
        assert_eq!(
            f.index.sync(&f.layout).unwrap(),
            Synced {
                unchanged: 2,
                ..Synced::default()
            }
        );

        // New bytes: re-read, and the old words are gone.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&a, "gamma three\n").unwrap();
        fs::remove_file(&b).unwrap();
        assert_eq!(
            f.index.sync(&f.layout).unwrap(),
            Synced {
                updated: 1,
                removed: 1,
                unchanged: 0,
                added: 0
            }
        );
        assert!(f.index.recall(&f.layout, "alpha", 5).unwrap().is_empty());
        assert!(f.index.recall(&f.layout, "beta", 5).unwrap().is_empty());
        assert_eq!(f.index.recall(&f.layout, "gamma", 5).unwrap().len(), 1);
    }

    #[test]
    fn another_project_and_unconfigured_scopes_stay_out() {
        let mut f = fixture();
        put(
            f.layout.project.join(".aibus/agent-memory/lead/m.md"),
            "shared word\n",
        );
        put(
            f.layout.data.join("agent-memory/lead/u.md"),
            "shared word\n",
        );
        f.index.sync(&f.layout).unwrap();

        // The same index file, seen from a sibling project with the project scope only.
        let mut cfg = Config::default();
        cfg.memory.scopes = vec!["project".into()];
        let other = f.layout.project.with_file_name("proj2");
        fs::create_dir_all(&other).unwrap();
        let l2 = Layout::new(&other, &cfg, &f.layout.data, Some("x")).unwrap();
        assert!(f.index.recall(&l2, "shared", 5).unwrap().is_empty());
        // Syncing from there neither removes nor adds the first project's rows.
        assert_eq!(f.index.sync(&l2).unwrap(), Synced::default());
        assert_eq!(f.index.recall(&f.layout, "shared", 5).unwrap().len(), 2);

        // `/work/proj` is not a prefix of `/work/proj2/...`.
        put(other.join(".aibus/agent-memory/lead/o.md"), "shared word\n");
        f.index.sync(&l2).unwrap();
        let from_first = f.index.recall(&f.layout, "shared", 10).unwrap();
        assert_eq!(from_first.len(), 2, "{from_first:?}");
        assert_eq!(f.index.recall(&l2, "shared", 10).unwrap().len(), 1);
    }

    #[test]
    fn the_index_is_a_cache_that_a_rebuild_restores() {
        let mut f = fixture();
        put(
            f.layout.data.join("agent-memory/lead/a.md"),
            "remember the milk\n",
        );
        f.index.sync(&f.layout).unwrap();
        let path = f.layout.index();
        // The index is not read again from here; the files are.
        drop(f.index);
        fs::remove_file(&path).unwrap();
        let mut again = Index::open(&path).unwrap();
        assert!(again.recall(&f.layout, "milk", 5).unwrap().is_empty());
        assert_eq!(again.sync(&f.layout).unwrap().added, 1);
        assert_eq!(again.recall(&f.layout, "milk", 5).unwrap().len(), 1);
    }
}
