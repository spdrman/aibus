//! `index.sqlite` (07 section 2.3): sessions and runs for `session list` and `session show`, and
//! the `memory_fts` tables. A cache: every row is rebuilt from the session directories, so
//! deleting the file loses nothing.
//!
//! SQLite does the locking between aibus processes (WAL, busy timeout 5 s). The file is opened
//! only when something needs it.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::fs::create_private_dir;
use super::session::{read_runs, SessionMeta};
use super::transcript::RunMeta;

pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, started_at TEXT, ended_at TEXT, cwd TEXT,
    topic TEXT, lead_agent TEXT, outcome TEXT, cost_usd REAL, runs INTEGER);
CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, session_id TEXT, parent_id TEXT, agent TEXT,
    role TEXT, model TEXT, account TEXT, status TEXT, started_at TEXT, ended_at TEXT, turns INTEGER,
    input_tokens INTEGER, output_tokens INTEGER, cost_usd REAL, summary_head TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(path, scope, kind, content,
    tokenize = 'porter unicode61');
CREATE TABLE IF NOT EXISTS memory_files (path TEXT PRIMARY KEY, scope TEXT, kind TEXT, mtime INTEGER,
    hash TEXT);
";

pub(crate) fn io_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(format!("index.sqlite: {e}"))
}

pub struct Index {
    conn: Connection,
}

impl Index {
    /// The connection, for `continuity::recall`, which keeps the memory queries next to the
    /// trait they implement.
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let conn = Connection::open(path).map_err(io_err)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(io_err)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(io_err)?;
        conn.execute_batch(SCHEMA).map_err(io_err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { conn })
    }

    /// Replaces the rows of one session.
    pub fn upsert(&mut self, s: &SessionMeta, runs: &[RunMeta]) -> io::Result<()> {
        let cost: f64 = runs.iter().map(|r| r.cost_usd).sum();
        let tx = self.conn.transaction().map_err(io_err)?;
        tx.execute("DELETE FROM runs WHERE session_id = ?1", [&s.id])
            .map_err(io_err)?;
        tx.execute(
            "INSERT OR REPLACE INTO sessions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                s.id,
                s.started,
                s.ended,
                s.cwd.to_string_lossy(),
                s.topic,
                s.lead,
                s.outcome,
                cost,
                runs.len() as i64
            ],
        )
        .map_err(io_err)?;
        for r in runs {
            tx.execute(
                "INSERT OR REPLACE INTO runs VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    r.id,
                    s.id,
                    r.parent_id,
                    r.agent,
                    r.role,
                    r.model,
                    r.account,
                    r.status,
                    r.started_at,
                    r.ended_at,
                    r.turns,
                    r.input_tokens as i64,
                    r.output_tokens as i64,
                    r.cost_usd,
                    r.summary_head
                ],
            )
            .map_err(io_err)?;
        }
        tx.commit().map_err(io_err)
    }

    pub fn forget(&mut self, id: &str) -> io::Result<()> {
        let tx = self.conn.transaction().map_err(io_err)?;
        tx.execute("DELETE FROM runs WHERE session_id = ?1", [id])
            .map_err(io_err)?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", [id])
            .map_err(io_err)?;
        tx.commit().map_err(io_err)
    }

    /// Drops every session and run row and reads them again from `sessions_dir`. Returns how
    /// many sessions were found.
    pub fn rebuild(&mut self, sessions_dir: &Path) -> io::Result<usize> {
        self.conn
            .execute_batch("DELETE FROM runs; DELETE FROM sessions;")
            .map_err(io_err)?;
        let mut found = 0;
        for entry in fs::read_dir(sessions_dir)?.flatten() {
            if let Ok(meta) = SessionMeta::read(&entry.path()) {
                self.upsert(&meta, &read_runs(&entry.path()))?;
                found += 1;
            }
        }
        Ok(found)
    }

    /// `[{id, started_at, ended_at, topic, runs, outcome, cost_usd}]`, newest first (06
    /// section 4).
    pub fn list(&self) -> io::Result<Vec<Value>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, started_at, ended_at, topic, runs, outcome, cost_usd FROM sessions
                 ORDER BY started_at DESC, id DESC",
            )
            .map_err(io_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "started_at": r.get::<_, Option<String>>(1)?,
                    "ended_at": r.get::<_, Option<String>>(2)?,
                    "topic": r.get::<_, Option<String>>(3)?,
                    "runs": r.get::<_, i64>(4)?,
                    "outcome": r.get::<_, Option<String>>(5)?,
                    "cost_usd": r.get::<_, f64>(6)?,
                }))
            })
            .map_err(io_err)?;
        rows.collect::<Result<_, _>>().map_err(io_err)
    }

    /// `{session, runs: [{id, agent, status, turns, usage}]}` for one session.
    pub fn show(&self, id: &str) -> io::Result<Option<Value>> {
        let Some(session) = self.list()?.into_iter().find(|s| s["id"] == id) else {
            return Ok(None);
        };
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, agent, status, turns, input_tokens, output_tokens, cost_usd FROM runs
                 WHERE session_id = ?1 ORDER BY id",
            )
            .map_err(io_err)?;
        let runs = stmt
            .query_map([id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "agent": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "turns": r.get::<_, i64>(3)?,
                    "usage": {
                        "input_tokens": r.get::<_, i64>(4)?,
                        "output_tokens": r.get::<_, i64>(5)?,
                        "cost_usd": r.get::<_, f64>(6)?,
                    },
                }))
            })
            .map_err(io_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_err)?;
        Ok(Some(json!({ "session": session, "runs": runs })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SessionId;

    /// RUSTSEC-2026-0459 is fixed in the SQLite 3.54.0 that `libsqlite3-sys` 0.39 bundles.
    #[test]
    fn bundled_sqlite_is_at_least_3_54() {
        let conn = Connection::open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .unwrap();
        let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
        assert!(
            parts[..2] >= [3, 54][..],
            "bundled SQLite {version} is older than 3.54.0"
        );
    }

    fn session(id: &str, started: &str) -> SessionMeta {
        let mut m = SessionMeta::new(
            SessionId::new(),
            started.into(),
            Some("t".into()),
            "/p".into(),
            "lead",
        );
        m.id = id.into();
        m
    }

    fn run(id: &str, cost: f64) -> RunMeta {
        RunMeta {
            id: id.into(),
            agent: "explore".into(),
            status: "completed".into(),
            turns: 2,
            input_tokens: 400,
            output_tokens: 20,
            cost_usd: cost,
            ..RunMeta::default()
        }
    }

    #[test]
    fn the_file_is_wal_with_the_07_tables_and_fts5_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/index.sqlite");
        let idx = Index::open(&path).unwrap();
        let mode: String = idx
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let busy: i64 = idx
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
        idx.conn
            .execute(
                "INSERT INTO memory_fts VALUES ('a.md', 'user', 'memory', 'the quick brown foxes')",
                [],
            )
            .unwrap();
        let hit: String = idx
            .conn
            .query_row(
                "SELECT path FROM memory_fts WHERE memory_fts MATCH 'fox'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "a.md", "the porter tokenizer stems foxes to fox");
        for t in ["sessions", "runs", "memory_files"] {
            idx.conn
                .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap();
        }
    }

    #[test]
    fn list_and_show_read_what_upsert_wrote_and_a_second_upsert_replaces_the_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = Index::open(&dir.path().join("i.sqlite")).unwrap();
        idx.upsert(
            &session("S1", "2026-11-01T00:00:00.000Z"),
            &[run("R1", 0.5), run("R2", 0.25)],
        )
        .unwrap();
        idx.upsert(&session("S2", "2026-11-02T00:00:00.000Z"), &[])
            .unwrap();
        let list = idx.list().unwrap();
        assert_eq!(list[0]["id"], "S2");
        assert_eq!(list[1]["runs"], 2);
        assert_eq!(list[1]["cost_usd"], 0.75);
        idx.upsert(
            &session("S1", "2026-11-01T00:00:00.000Z"),
            &[run("R3", 1.0)],
        )
        .unwrap();
        let shown = idx.show("S1").unwrap().unwrap();
        assert_eq!(shown["runs"].as_array().unwrap().len(), 1);
        assert_eq!(shown["runs"][0]["usage"]["input_tokens"], 400);
        assert!(idx.show("nope").unwrap().is_none());
        idx.forget("S1").unwrap();
        assert_eq!(idx.list().unwrap().len(), 1);
    }

    #[test]
    fn rebuild_reads_the_session_directories() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let s1 = sessions.join("S1");
        fs::create_dir_all(s1.join("runs/R1")).unwrap();
        session("S1", "2026-11-01T00:00:00.000Z")
            .write(&s1)
            .unwrap();
        fs::write(
            s1.join("runs/R1/meta.json"),
            serde_json::to_string(&run("R1", 0.5)).unwrap(),
        )
        .unwrap();
        fs::create_dir_all(sessions.join("junk")).unwrap();
        let mut idx = Index::open(&dir.path().join("i.sqlite")).unwrap();
        idx.upsert(&session("GONE", "2026-10-01T00:00:00.000Z"), &[])
            .unwrap();
        assert_eq!(idx.rebuild(&sessions).unwrap(), 1);
        let list = idx.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], "S1");
        assert_eq!(list[0]["runs"], 1);
    }
}
