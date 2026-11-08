//! Which handoff is the latest (07 section 5): the newest file by name under
//! `thoughts/shared/handoffs/`, whichever topic it is in. Names start with a timestamp
//! (`2026-11-07_23-52_review.yaml`), so the last name in order is the newest.

use std::fs;
use std::path::{Path, PathBuf};

pub fn latest(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|topic| topic.path().is_dir())
        .flat_map(|topic| fs::read_dir(topic.path()).into_iter().flatten().flatten())
        .map(|f| f.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "yaml"))
        .max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_name_wins_across_topics() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "general/2026-11-07_23-52_review.yaml",
            "auth/2026-11-08_09-00_login.yaml",
            "auth/2026-11-06_09-00_old.yaml",
            "auth/notes.txt",
        ] {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        assert_eq!(
            latest(dir.path()).unwrap(),
            dir.path().join("auth/2026-11-08_09-00_login.yaml")
        );
        assert!(latest(&dir.path().join("missing")).is_none());
    }
}
