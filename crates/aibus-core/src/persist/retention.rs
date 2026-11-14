//! Retention (07 section 4): sessions older than `session_retention_days` go, with their hook
//! logs and their index rows, unless `session.json` says `"pinned": true`. The model cache
//! expires after 7 days. Memory is never touched.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::index::Index;
use super::paths::Paths;
use super::session::SessionMeta;

const DAY: Duration = Duration::from_secs(86_400);
pub const MODEL_CACHE_TTL: Duration = Duration::from_secs(7 * 86_400);

/// `30d`, `12h` or `2w`.
pub fn parse_age(text: &str) -> Result<Duration, String> {
    let bad = || format!("{text:?} is not an age: use a number and d, h or w (30d)");
    let (digits, unit) = text.split_at(text.len().saturating_sub(1));
    let n: u64 = digits.parse().map_err(|_| bad())?;
    let secs = match unit {
        "h" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return Err(bad()),
    };
    Ok(Duration::from_secs(n.saturating_mul(secs)))
}

/// Removes every unpinned session started more than `older_than` before `now`, and returns
/// their ids. Directories that are not session ids are left alone.
pub fn prune(paths: &Paths, now: SystemTime, older_than: Duration) -> io::Result<Vec<String>> {
    let sessions = paths.state.join("sessions");
    let index_path = paths.data.join("index.sqlite");
    let mut index = if index_path.is_file() {
        Some(Index::open(&index_path)?)
    } else {
        None
    };
    let mut removed = Vec::new();
    let entries = match fs::read_dir(&sessions) {
        Ok(e) => Some(e),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    for entry in entries.into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // A session id is a ULID, so its name says when it started.
        let Some(started) = ulid::Ulid::from_string(&name).ok().map(|u| u.datetime()) else {
            continue;
        };
        let old = now
            .duration_since(started)
            .is_ok_and(|age| age > older_than);
        let pinned = SessionMeta::read(&entry.path()).is_ok_and(|m| m.pinned);
        if !old || pinned {
            continue;
        }
        fs::remove_dir_all(entry.path())?;
        match fs::remove_dir_all(paths.state.join("logs/hooks").join(&name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        if let Some(i) = &mut index {
            i.forget(&name)?;
        }
        removed.push(name);
    }
    expire_model_cache(paths, now)?;
    removed.sort();
    Ok(removed)
}

/// `cache/models/<provider>.json` files older than 7 days; `account test` writes them again.
fn expire_model_cache(paths: &Paths, now: SystemTime) -> io::Result<()> {
    let Ok(entries) = fs::read_dir(paths.cache.join("models")) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let age = e
            .metadata()?
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok());
        if age.is_some_and(|a| a > MODEL_CACHE_TTL) {
            fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// The startup prune: runs `prune` unless the last one was less than a day ago. `None` when
/// it did not run.
pub fn prune_if_due(
    paths: &Paths,
    now: SystemTime,
    older_than: Duration,
) -> io::Result<Option<Vec<String>>> {
    let stamp = paths.state.join("last-prune");
    let last = fs::read_to_string(&stamp)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .map(|s| UNIX_EPOCH + Duration::from_secs(s));
    if last.is_some_and(|l| now.duration_since(l).is_ok_and(|d| d < DAY)) {
        return Ok(None);
    }
    let removed = prune(paths, now, older_than)?;
    let secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    super::fs::create_private_dir(&paths.state)?;
    super::fs::write_atomic(&stamp, secs.to_string())?;
    Ok(Some(removed))
}

/// Worktrees `isolation: worktree` left behind: `cache/worktrees/<session>/<run>` (07 section 4
/// removes them at run end when clean, so what is here is not).
pub fn worktrees(paths: &Paths, session: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(paths.cache.join("worktrees").join(session))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SessionId;
    use std::path::Path;

    fn paths(root: &Path) -> Paths {
        Paths {
            config: root.join("c"),
            data: root.join("d"),
            state: root.join("s"),
            cache: root.join("k"),
            config_override: None,
        }
    }

    fn make(p: &Paths, age_days: u64, pinned: bool, now: SystemTime) -> String {
        let at = now - Duration::from_secs(age_days * 86_400);
        let ms = at.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let id = SessionId(ulid::Ulid::from_parts(ms, 1)).to_string();
        let dir = p.state.join("sessions").join(&id);
        fs::create_dir_all(dir.join("runs")).unwrap();
        let mut m = SessionMeta::new(
            SessionId::new(),
            "2026-01-01T00:00:00.000Z".into(),
            None,
            "/p".into(),
            "lead",
        );
        m.id = id.clone();
        m.pinned = pinned;
        m.write(&dir).unwrap();
        fs::create_dir_all(p.state.join("logs/hooks").join(&id)).unwrap();
        fs::write(
            p.state.join("logs/hooks").join(&id).join("stop-1-0.log"),
            "x",
        )
        .unwrap();
        id
    }

    #[test]
    fn old_unpinned_sessions_go_with_their_hook_logs_and_index_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        let now = SystemTime::now();
        let old = make(&p, 31, false, now);
        let pinned = make(&p, 90, true, now);
        let fresh = make(&p, 29, false, now);
        let mut idx = Index::open(&p.data.join("index.sqlite")).unwrap();
        idx.rebuild(&p.state.join("sessions")).unwrap();
        assert_eq!(idx.list().unwrap().len(), 3);
        fs::create_dir_all(p.state.join("sessions/not-a-session")).unwrap();

        let gone = prune(&p, now, parse_age("30d").unwrap()).unwrap();
        assert_eq!(gone, std::slice::from_ref(&old));
        assert!(!p.state.join("sessions").join(&old).exists());
        assert!(!p.state.join("logs/hooks").join(&old).exists());
        assert!(p.state.join("sessions").join(&pinned).is_dir());
        assert!(p.state.join("logs/hooks").join(&pinned).is_dir());
        assert!(p.state.join("sessions").join(&fresh).is_dir());
        assert!(p.state.join("sessions/not-a-session").is_dir());
        let ids: Vec<_> = idx
            .list()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect();
        assert!(!ids.contains(&old) && ids.len() == 2);
    }

    #[test]
    fn the_startup_prune_runs_once_a_day() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        let now = SystemTime::now();
        let old = make(&p, 40, false, now);
        let age = Duration::from_secs(30 * 86_400);
        assert_eq!(prune_if_due(&p, now, age).unwrap(), Some(vec![old]));
        let again = make(&p, 40, false, now);
        assert_eq!(
            prune_if_due(&p, now + Duration::from_secs(3_600), age).unwrap(),
            None
        );
        assert!(p.state.join("sessions").join(&again).is_dir());
        let later = now + DAY + Duration::from_secs(1);
        assert_eq!(prune_if_due(&p, later, age).unwrap(), Some(vec![again]));
    }

    #[test]
    fn the_model_cache_expires_after_7_days_and_memory_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(p.cache.join("models")).unwrap();
        fs::write(p.cache.join("models/anthropic.json"), "{}").unwrap();
        fs::create_dir_all(p.data.join("agent-memory/lead")).unwrap();
        fs::write(p.data.join("agent-memory/lead/MEMORY.md"), "keep").unwrap();
        let now = SystemTime::now();
        prune(&p, now + Duration::from_secs(6 * 86_400), DAY).unwrap();
        assert!(p.cache.join("models/anthropic.json").exists());
        prune(&p, now + Duration::from_secs(8 * 86_400), DAY).unwrap();
        assert!(!p.cache.join("models/anthropic.json").exists());
        assert!(p.data.join("agent-memory/lead/MEMORY.md").exists());
    }

    #[test]
    fn ages_parse_and_garbage_does_not() {
        assert_eq!(parse_age("30d").unwrap(), Duration::from_secs(30 * 86_400));
        assert_eq!(parse_age("12h").unwrap(), Duration::from_secs(12 * 3_600));
        assert_eq!(parse_age("2w").unwrap(), Duration::from_secs(14 * 86_400));
        assert!(parse_age("soon").is_err() && parse_age("d").is_err() && parse_age("").is_err());
    }

    #[test]
    fn leftover_worktrees_are_listed_per_session() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(p.cache.join("worktrees/S1/R2")).unwrap();
        fs::create_dir_all(p.cache.join("worktrees/S1/R1")).unwrap();
        assert_eq!(
            worktrees(&p, "S1"),
            [
                p.cache.join("worktrees/S1/R1"),
                p.cache.join("worktrees/S1/R2")
            ]
        );
        assert!(worktrees(&p, "S2").is_empty());
    }
}
