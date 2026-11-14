//! Worktrees for `isolation: worktree` (01 section 1.1, 09 section 7): a checkout of the default
//! branch under the cache directory, never inside the project. A clean one is removed when its
//! run ends; one with changes is kept and reported.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::persist::fs::create_private_dir;

/// A worktree an isolated run works in.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// The checkout's root: `<cache>/worktrees/<session>/<run>`.
    pub path: PathBuf,
    /// The repository it belongs to, which `git worktree remove` runs against.
    pub repo: PathBuf,
    /// Where the run's tools work: `path`, plus the project's own place inside the repository.
    pub cwd: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// `git worktree add` from the repository's default branch into `at` ; one that is there already
/// (a kept one, being resumed) is used as it is.
/// The checkout is detached, so two runs never fight over a branch name. `Err` is why not,
/// e.g. `project` is not inside a repository.
pub fn create(project: &Path, at: &Path) -> Result<WorktreeInfo, String> {
    let repo = PathBuf::from(
        git(project, &["rev-parse", "--show-toplevel"])
            .map_err(|_| format!("{} is not inside a git repository", project.display()))?,
    );
    if !at.exists() {
        // The remote's default branch, else `main` or `master`, else wherever HEAD is.
        let branch = git(&repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .ok()
            .or_else(|| {
                ["main", "master"]
                    .into_iter()
                    .find(|b| git(&repo, &["rev-parse", "--verify", "--quiet", b]).is_ok())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "HEAD".to_owned());
        if let Some(parent) = at.parent() {
            create_private_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        git(&repo, &["worktree", "add", "--detach", &at.to_string_lossy(), &branch])?;
    }
    let inside = project
        .canonicalize()
        .ok()
        .zip(repo.canonicalize().ok())
        .and_then(|(p, r)| p.strip_prefix(r).ok().map(Path::to_path_buf))
        .unwrap_or_default();
    Ok(WorktreeInfo {
        path: at.to_path_buf(),
        repo,
        cwd: at.join(inside),
    })
}

/// Whether the checkout holds anything worth keeping: uncommitted or untracked files, or
/// commits no branch has.
fn dirty(path: &Path) -> bool {
    let changed = git(path, &["status", "--porcelain"]).map_or(true, |s| !s.is_empty());
    let unique = git(path, &["rev-list", "HEAD", "--not", "--branches", "--remotes"])
        .map_or(true, |s| !s.is_empty());
    changed || unique
}

/// Removes the worktree when it is clean. `Some` is what was kept: it had changes, or git
/// would not remove it.
pub fn release(info: WorktreeInfo) -> Option<WorktreeInfo> {
    if !info.path.exists() {
        return None;
    }
    if dirty(&info.path) {
        return Some(info);
    }
    let path = info.path.to_string_lossy();
    match git(&info.repo, &["worktree", "remove", "--force", &path]) {
        Ok(_) => None,
        Err(_) => Some(info),
    }
}

/// Session end: releases every worktree left under `session_dir` (a run that was killed never
/// got to) and returns the ones that stay.
pub fn sweep(session_dir: &Path) -> Vec<WorktreeInfo> {
    let mut kept = Vec::new();
    for entry in std::fs::read_dir(session_dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(common) = git(&path, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) else {
            continue;
        };
        let repo = Path::new(&common).parent().map_or_else(|| path.clone(), Path::to_path_buf);
        let info = WorktreeInfo {
            cwd: path.clone(),
            path,
            repo,
        };
        kept.extend(release(info));
    }
    // Nothing of the session's directory is left when nothing was kept.
    let _ = std::fs::remove_dir(session_dir);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        sh(d.path(), &["add", "."]);
        sh(d.path(), &["commit", "-q", "-m", "first"]);
        d
    }

    #[test]
    fn a_clean_worktree_is_removed_and_a_dirty_one_kept() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();

        let clean = create(project.path(), &cache.path().join("s/r1")).unwrap();
        assert!(clean.cwd.join("a.txt").is_file());
        assert!(release(clean.clone()).is_none());
        assert!(!clean.path.exists());

        let dirty = create(project.path(), &cache.path().join("s/r2")).unwrap();
        std::fs::write(dirty.cwd.join("b.txt"), "new\n").unwrap();
        let kept = release(dirty.clone()).expect("kept");
        assert_eq!(kept, dirty);
        assert!(dirty.path.join("b.txt").is_file());
        // The project itself never saw the file.
        assert!(!project.path().join("b.txt").exists());
    }

    #[test]
    fn a_commit_no_branch_has_keeps_the_worktree() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("c.txt"), "c\n").unwrap();
        sh(&wt.path, &["add", "."]);
        sh(&wt.path, &["commit", "-q", "-m", "work"]);
        assert!(release(wt).is_some());
    }

    #[test]
    fn outside_a_repository_it_fails_with_a_message() {
        let plain = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let err = create(plain.path(), &cache.path().join("s/r")).unwrap_err();
        assert!(err.contains("not inside a git repository"), "{err}");
    }

    #[test]
    fn a_subdirectory_project_gets_the_same_place_in_the_worktree() {
        let project = repo();
        std::fs::create_dir(project.path().join("sub")).unwrap();
        std::fs::write(project.path().join("sub/x"), "x").unwrap();
        sh(project.path(), &["add", "."]);
        sh(project.path(), &["commit", "-q", "-m", "sub"]);
        let cache = tempfile::tempdir().unwrap();
        let wt = create(&project.path().join("sub"), &cache.path().join("s/r")).unwrap();
        assert!(wt.cwd.ends_with("s/r/sub"), "{:?}", wt.cwd);
        assert!(wt.cwd.join("x").is_file());
    }

    #[test]
    fn a_sweep_removes_the_clean_ones_and_reports_the_dirty() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let session = cache.path().join("s");
        let a = create(project.path(), &session.join("a")).unwrap();
        let b = create(project.path(), &session.join("b")).unwrap();
        std::fs::write(b.cwd.join("dirt"), "d").unwrap();
        let kept = sweep(&session);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, b.path);
        assert!(!a.path.exists());
    }
}
