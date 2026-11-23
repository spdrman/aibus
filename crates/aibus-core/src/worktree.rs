//! Worktrees for `isolation: worktree` (01 section 1.1, 09 section 7): a checkout of the default
//! branch under the cache directory, never inside the project. A clean one is removed when its
//! run ends; one with changes is kept and reported.
//!
//! A2.5 (#290): each worktree is one write attempt. Its base commit is pinned under
//! `refs/aibus/base/<id>` before the checkout exists, so it survives an interrupted attempt and
//! the diff always has a fixed left side. Every operation that changes the repository (adding a
//! worktree, removing one, merging) holds one exclusive lock on `<git-dir>/aibus-repo.lock`.
//! `review` shows what an attempt changed, and nothing reaches the project's branch except
//! `approve`, which the operator calls with the hash of the patch they looked at.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

/// What an attempt changed against its base commit, as the operator reviews it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Review {
    /// The commit the attempt started from.
    pub base: String,
    /// Changed paths, new files included.
    pub files: Vec<String>,
    /// `git diff --stat`.
    pub stat: String,
    /// The full patch (binary files included), as the bytes git wrote: a text file that is not
    /// UTF-8 stays as it is, so what `reject` saves hashes to `patch_sha256`.
    pub patch: Vec<u8>,
    /// SHA-256 of the patch bytes: what `approve` and `reject` must be given back.
    pub patch_sha256: String,
}

/// What `approve` did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Merge {
    /// Merged: the merge commit on the project's branch.
    Merged(String),
    /// The merge conflicted in these files, was aborted, and left the project as it was. The
    /// attempt stays, on the branch `aibus/<id>`.
    Conflict(Vec<String>),
}

/// Every git call goes through here with the hooks directory pointed at nothing and no
/// filesystem monitor: a hook (`post-index-change` on a `status`, `reference-transaction` on an
/// `update-ref`, and the rest) or a monitor named in the config would otherwise run files from the
/// attempt's own checkout, as the operator, through a relative `core.hooksPath`.
fn git_raw(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_raw(dir, args).map(|o| String::from_utf8_lossy(&o).trim().to_owned())
}

/// The attempt's id: the last part of its path. It becomes a ref and a branch name, so it is
/// held to a short, plain alphabet.
fn id_of(at: &Path) -> Result<String, String> {
    let id = at.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let plain = id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if id.is_empty() || id.len() > 100 || !plain || id.starts_with(['-', '.']) || id.contains("..") {
        return Err(format!("{id:?} is not a usable attempt id"));
    }
    Ok(id)
}

/// Holds the repository's lock until the returned file is dropped. Callers that already hold it
/// call the `_held` functions instead: a second lock from the same process would wait forever.
fn lock(repo: &Path) -> Result<std::fs::File, String> {
    use fs4::fs_std::FileExt;
    let common = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(Path::new(&common).join("aibus-repo.lock"))
        .map_err(|e| format!("repository lock: {e}"))?;
    file.lock_exclusive().map_err(|e| format!("repository lock: {e}"))?;
    Ok(file)
}

/// `git worktree add` from the repository's default branch into `at` ; one that is there already
/// (a kept one, being resumed) is used as it is.
/// The checkout is detached, so two runs never fight over a branch name, and its base commit is
/// recorded. `Err` is why not, e.g. `project` is not inside a repository, `at` is inside the
/// repository, or `at` is there but is not a worktree of it.
pub fn create(project: &Path, at: &Path) -> Result<WorktreeInfo, String> {
    let id = id_of(at)?;
    if !at.is_absolute() {
        return Err(format!("{} is not an absolute path", at.display()));
    }
    let repo = PathBuf::from(
        git(project, &["rev-parse", "--show-toplevel"])
            .map_err(|_| format!("{} is not inside a git repository", project.display()))?,
    );
    let _lock = lock(&repo)?;
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
        let base = git(&repo, &["rev-parse", "--verify", &format!("{branch}^{{commit}}")])?;
        if let Some(parent) = at.parent() {
            create_private_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            let inside = parent.canonicalize().ok().zip(repo.canonicalize().ok());
            if inside.is_some_and(|(p, r)| p.starts_with(r)) {
                return Err(format!("{} is inside the repository", at.display()));
            }
        }
        // Pin the base first: an attempt cut off half way still has it. A worktree whose
        // directory was lost is forgotten here, so its name is free again.
        let base_ref = format!("refs/aibus/base/{id}");
        let _ = git(&repo, &["worktree", "prune"]);
        git(&repo, &["update-ref", &base_ref, &base])?;
        if let Err(e) = git(&repo, &["worktree", "add", "--detach", "--", &at.to_string_lossy(), &base]) {
            let _ = git(&repo, &["update-ref", "-d", &base_ref]);
            return Err(e);
        }
    } else {
        let theirs = git(at, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        let ours = git(&repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        let top = git(at, &["rev-parse", "--show-toplevel"]).ok().and_then(|t| Path::new(&t).canonicalize().ok());
        if theirs.is_err() || theirs != ours || top != at.canonicalize().ok() {
            return Err(format!("{} exists and is not a worktree of {}", at.display(), repo.display()));
        }
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

/// `release` while the repository lock is held. No `--force`: git itself refuses to remove a
/// checkout with modified or untracked files, a second check behind `dirty`.
fn release_held(info: WorktreeInfo) -> Option<WorktreeInfo> {
    if !info.path.exists() {
        return None;
    }
    if dirty(&info.path) {
        return Some(info);
    }
    let path = info.path.to_string_lossy();
    match git(&info.repo, &["worktree", "remove", "--", &path]) {
        Ok(_) => {
            if let Ok(id) = id_of(&info.path) {
                let _ = git(&info.repo, &["update-ref", "-d", &format!("refs/aibus/base/{id}")]);
            }
            None
        }
        Err(_) => Some(info),
    }
}

/// Removes the worktree when it is clean. `Some` is what was kept: it had changes, or git
/// would not remove it.
pub fn release(info: WorktreeInfo) -> Option<WorktreeInfo> {
    match lock(&info.repo) {
        Ok(_held) => release_held(info),
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

/// What the attempt changed against its base. It stages everything in the attempt's own
/// worktree (never the project's) so new files show up, and leaves the commit to `approve`.
pub fn review(info: &WorktreeInfo) -> Result<Review, String> {
    let id = id_of(&info.path)?;
    let base = git(&info.repo, &["rev-parse", "--verify", &format!("refs/aibus/base/{id}^{{commit}}")])
        .map_err(|_| format!("attempt {id} has no recorded base commit"))?;
    git(&info.path, &["add", "-A"])?;
    let diff = |extra: &[&str]| {
        let mut args = vec!["diff", "--cached", "--no-ext-diff", "--no-textconv", "--no-color"];
        args.extend_from_slice(extra);
        args.extend([base.as_str(), "--"]);
        git_raw(&info.path, &args)
    };
    let patch = diff(&["--binary"])?;
    let files = String::from_utf8_lossy(&diff(&["--name-only", "-z"])?)
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(Review {
        patch_sha256: format!("{:x}", Sha256::digest(&patch)),
        patch,
        stat: String::from_utf8_lossy(&diff(&["--stat"])?).trim().to_owned(),
        files,
        base,
    })
}

/// Writes the decision record `<git-dir>/aibus/attempts/<id>.json` (and `<id>.patch` when given).
fn record(info: &WorktreeInfo, r: &Review, decision: &str, by: &str, merged: Option<&str>, patch: bool) -> Result<(), String> {
    let id = id_of(&info.path)?;
    let common = git(&info.repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let dir = Path::new(&common).join("aibus/attempts");
    create_private_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json = serde_json::json!({
        "id": id, "decision": decision, "by": by, "base": r.base,
        "patch_sha256": r.patch_sha256, "files": r.files, "merge_commit": merged,
    });
    let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    if patch {
        let at = dir.join(format!("{id}.patch"));
        std::fs::write(&at, &r.patch).map_err(|e| format!("{}: {e}", at.display()))?;
    }
    let at = dir.join(format!("{id}.json"));
    std::fs::write(&at, text).map_err(|e| format!("{}: {e}", at.display()))
}

/// Merges the attempt into the branch the project has checked out, as the operator's decision.
/// `reviewed` is the `patch_sha256` they looked at: if the attempt changed since, nothing is
/// merged. A project with uncommitted changes, or no branch checked out, is not merged into.
/// A conflict is aborted and reported, never left half merged.
pub fn approve(info: &WorktreeInfo, reviewed: &str, by: &str) -> Result<Merge, String> {
    let _lock = lock(&info.repo)?;
    let r = review(info)?;
    if r.patch_sha256 != reviewed {
        return Err("the attempt changed since it was reviewed, review it again".to_owned());
    }
    if r.files.is_empty() {
        return Err("the attempt changed nothing, so there is nothing to merge".to_owned());
    }
    if git(&info.repo, &["symbolic-ref", "--quiet", "HEAD"]).is_err() {
        return Err("the repository has no branch checked out, nothing was merged".to_owned());
    }
    if !git(&info.repo, &["status", "--porcelain"])?.is_empty() {
        return Err("the repository has uncommitted changes, nothing was merged".to_owned());
    }
    let id = id_of(&info.path)?;
    let branch = format!("aibus/{id}");
    // No hook runs (see `git_raw`); `--no-verify` stays as a second layer for the three it covers.
    let agent = ["-c", "user.name=aibus", "-c", "user.email=aibus@localhost", "-c", "commit.gpgsign=false"];
    let mut commit = agent.to_vec();
    commit.extend(["commit", "-q", "--no-verify", "-m"]);
    let message = format!("{id}: attempt approved by {by}");
    commit.push(&message);
    git(&info.path, &["switch", "-q", "-c", &branch])?;
    if git(&info.path, &["diff", "--cached", "--quiet"]).is_err() {
        git(&info.path, &commit)?;
    }
    let configured = git(&info.repo, &["config", "user.email"]).is_ok_and(|s| !s.is_empty());
    let mut merge = vec![];
    if !configured {
        merge.extend(&agent[..4]);
    }
    merge.extend(["merge", "--no-ff", "--no-edit", "--no-verify", "-m"]);
    let message = format!("Merge attempt {id}, approved by {by}");
    merge.extend([message.as_str(), branch.as_str()]);
    if let Err(e) = git(&info.repo, &merge) {
        let conflicts = git(&info.repo, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let _ = git(&info.repo, &["merge", "--abort"]);
        let files: Vec<String> = conflicts.lines().map(str::to_owned).collect();
        return if files.is_empty() { Err(format!("merge failed: {e}")) } else { Ok(Merge::Conflict(files)) };
    }
    let head = git(&info.repo, &["rev-parse", "HEAD"])?;
    record(info, &r, "approved", by, Some(&head), false)?;
    // The work is on the project's branch now, so the checkout is clean and goes.
    release_held(info.clone());
    Ok(Merge::Merged(head))
}

/// Turns the attempt down. Its patch is saved next to the decision record first, so removing the
/// checkout loses nothing, and the project's branches are not touched.
pub fn reject(info: &WorktreeInfo, reviewed: &str, by: &str) -> Result<(), String> {
    let _lock = lock(&info.repo)?;
    let r = review(info)?;
    if r.patch_sha256 != reviewed {
        return Err("the attempt changed since it was reviewed, review it again".to_owned());
    }
    record(info, &r, "rejected", by, None, true)?;
    git(&info.repo, &["worktree", "remove", "--force", "--", &info.path.to_string_lossy()])?;
    let id = id_of(&info.path)?;
    let _ = git(&info.repo, &["update-ref", "-d", &format!("refs/aibus/base/{id}")]);
    Ok(())
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

    fn head(dir: &Path) -> String {
        git(dir, &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn two_attempts_get_their_own_trees_and_never_touch_the_project() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let (a, b) = std::thread::scope(|s| {
            let a = s.spawn(|| create(project.path(), &cache.path().join("s/a")).unwrap());
            let b = s.spawn(|| create(project.path(), &cache.path().join("s/b")).unwrap());
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_ne!(a.path, b.path);
        std::fs::write(a.cwd.join("a.txt"), "from a\n").unwrap();
        std::fs::write(b.cwd.join("a.txt"), "from b\n").unwrap();
        assert_eq!(std::fs::read_to_string(a.cwd.join("a.txt")).unwrap(), "from a\n");
        assert_eq!(std::fs::read_to_string(b.cwd.join("a.txt")).unwrap(), "from b\n");
        assert_eq!(std::fs::read_to_string(project.path().join("a.txt")).unwrap(), "one\n");
        // Each has its own pinned base.
        for w in [&a, &b] {
            assert_eq!(review(w).unwrap().base, head(project.path()));
        }
    }

    #[test]
    fn a_path_inside_the_repository_or_a_foreign_directory_is_refused() {
        let project = repo();
        let err = create(project.path(), &project.path().join("inner")).unwrap_err();
        assert!(err.contains("inside the repository"), "{err}");
        let other = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let at = cache.path().join("s/x");
        std::fs::create_dir_all(&at).unwrap();
        let err = create(project.path(), &at).unwrap_err();
        assert!(err.contains("not a worktree"), "{err}");
        for bad in ["-rf", "..", "a b", ".hidden"] {
            assert!(create(project.path(), &other.path().join(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_review_lists_changed_and_new_files_with_a_patch_hash() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(wt.cwd.join("new.txt"), "n\n").unwrap();
        let r = review(&wt).unwrap();
        assert_eq!(r.files, ["a.txt", "new.txt"]);
        assert!(String::from_utf8_lossy(&r.patch).contains("+two") && r.stat.contains("2 files changed"), "{}", r.stat);
        assert_eq!(r, review(&wt).unwrap());
        std::fs::write(wt.cwd.join("new.txt"), "changed\n").unwrap();
        assert_ne!(review(&wt).unwrap().patch_sha256, r.patch_sha256);
    }

    #[test]
    fn nothing_is_merged_until_approve_and_approve_needs_the_reviewed_hash() {
        let project = repo();
        let before = head(project.path());
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("b.txt"), "agent says done\n").unwrap();
        let r = review(&wt).unwrap();
        // A finished attempt, a release and a review change nothing in the project.
        assert_eq!(head(project.path()), before);
        assert!(!project.path().join("b.txt").exists());
        assert!(release(wt.clone()).is_some());

        let err = approve(&wt, "0000", "rom").unwrap_err();
        assert!(err.contains("changed since"), "{err}");
        assert_eq!(head(project.path()), before);

        let Merge::Merged(commit) = approve(&wt, &r.patch_sha256, "rom").unwrap() else {
            panic!("expected a merge")
        };
        assert_eq!(head(project.path()), commit);
        assert!(project.path().join("b.txt").is_file());
        assert_eq!(git(project.path(), &["rev-list", "--parents", "-n1", "HEAD"]).unwrap().split(' ').count(), 3);
        // Checkout gone, record written.
        assert!(!wt.path.exists());
        let rec = git(project.path(), &["rev-parse", "--git-dir"]).unwrap();
        let rec = project.path().join(rec).join("aibus/attempts/r.json");
        assert!(std::fs::read_to_string(rec).unwrap().contains("\"approved\""));
    }

    #[test]
    fn reject_keeps_the_patch_and_leaves_the_project_alone() {
        let project = repo();
        let before = head(project.path());
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("b.txt"), "no\n").unwrap();
        let r = review(&wt).unwrap();
        assert!(reject(&wt, "bad", "rom").is_err());
        assert!(wt.path.exists());
        reject(&wt, &r.patch_sha256, "rom").unwrap();
        assert!(!wt.path.exists());
        assert_eq!(head(project.path()), before);
        assert!(git(project.path(), &["status", "--porcelain"]).unwrap().is_empty());
        let dir = project.path().join(".git/aibus/attempts");
        assert_eq!(std::fs::read(dir.join("r.patch")).unwrap(), r.patch);
        assert!(std::fs::read_to_string(dir.join("r.json")).unwrap().contains("\"rejected\""));
    }

    #[test]
    fn a_conflict_is_aborted_and_the_project_stays_as_it_was() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("a.txt"), "agent\n").unwrap();
        let r = review(&wt).unwrap();
        std::fs::write(project.path().join("a.txt"), "someone else\n").unwrap();
        sh(project.path(), &["commit", "-qam", "moved on"]);
        let tip = head(project.path());
        let out = approve(&wt, &r.patch_sha256, "rom").unwrap();
        assert_eq!(out, Merge::Conflict(vec!["a.txt".to_owned()]));
        assert_eq!(head(project.path()), tip);
        assert!(git(project.path(), &["status", "--porcelain"]).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(project.path().join("a.txt")).unwrap(), "someone else\n");
        // The attempt is kept, on its branch.
        assert!(wt.path.exists());
        git(project.path(), &["rev-parse", "--verify", "aibus/r"]).unwrap();
    }

    #[test]
    fn approve_never_merges_into_a_project_with_uncommitted_work() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("b.txt"), "b\n").unwrap();
        let r = review(&wt).unwrap();
        std::fs::write(project.path().join("a.txt"), "my edit\n").unwrap();
        let err = approve(&wt, &r.patch_sha256, "rom").unwrap_err();
        assert!(err.contains("uncommitted"), "{err}");
        assert_eq!(std::fs::read_to_string(project.path().join("a.txt")).unwrap(), "my edit\n");
        assert!(!project.path().join("b.txt").exists());
    }

    #[test]
    fn cleanup_keeps_uncommitted_work_even_when_a_tracked_file_is_edited() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("a.txt"), "half done\n").unwrap();
        assert!(release(wt.clone()).is_some());
        assert_eq!(std::fs::read_to_string(wt.cwd.join("a.txt")).unwrap(), "half done\n");
    }

    #[test]
    fn an_interrupted_attempt_is_swept_without_losing_work_and_a_lost_directory_is_forgotten() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let session = cache.path().join("s");
        // Killed with work in the tree: no release ever ran.
        let killed = create(project.path(), &session.join("killed")).unwrap();
        std::fs::write(killed.cwd.join("wip.txt"), "wip\n").unwrap();
        // Killed with the directory half gone: its registration is stale.
        let lost = create(project.path(), &session.join("lost")).unwrap();
        std::fs::remove_dir_all(&lost.path).unwrap();
        let kept = sweep(&session);
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(killed.cwd.join("wip.txt")).unwrap(), "wip\n");
        // The same name can be used again.
        let again = create(project.path(), &session.join("lost")).unwrap();
        assert!(again.cwd.join("a.txt").is_file());
    }

    #[test]
    fn a_stale_base_ref_from_a_cut_off_attempt_is_replaced() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        git(project.path(), &["update-ref", "refs/aibus/base/ghost", "HEAD"]).unwrap();
        let wt = create(project.path(), &cache.path().join("s/ghost")).unwrap();
        assert_eq!(review(&wt).unwrap().base, head(project.path()));
    }

    #[test]
    fn reject_saves_the_exact_patch_even_for_a_text_file_that_is_not_utf8() {
        let project = repo();
        let cache = tempfile::tempdir().unwrap();
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        // Latin-1 text: no NUL byte, so git diffs it as text, but it is not UTF-8.
        std::fs::write(wt.cwd.join("latin.txt"), b"caf\xe9\n").unwrap();
        let r = review(&wt).unwrap();
        assert!(std::str::from_utf8(&r.patch).is_err());
        reject(&wt, &r.patch_sha256, "rom").unwrap();
        let saved = std::fs::read(project.path().join(".git/aibus/attempts/r.patch")).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&saved)), r.patch_sha256);
    }

    #[cfg(unix)]
    #[test]
    fn no_hook_or_monitor_runs_for_create_review_approve_reject_or_release() {
        use std::os::unix::fs::PermissionsExt;
        let project = repo();
        let marks = tempfile::tempdir().unwrap();
        let hooks = project.path().join(".githooks");
        std::fs::create_dir(&hooks).unwrap();
        // Every hook in githooks(5), plus the monitor.
        for name in [
            "applypatch-msg", "pre-applypatch", "post-applypatch", "pre-commit", "pre-merge-commit",
            "prepare-commit-msg", "commit-msg", "post-commit", "pre-rebase", "post-checkout",
            "post-merge", "pre-push", "pre-receive", "update", "proc-receive", "post-receive",
            "post-update", "reference-transaction", "push-to-checkout", "pre-auto-gc",
            "post-rewrite", "sendemail-validate", "fsmonitor-watchman", "p4-changelist",
            "p4-prepare-changelist", "p4-post-changelist", "p4-pre-submit", "post-index-change",
        ] {
            let at = hooks.join(name);
            std::fs::write(&at, format!("#!/bin/sh\ntouch '{}/{name}'\n", marks.path().display())).unwrap();
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        sh(project.path(), &["add", "."]);
        sh(project.path(), &["commit", "-q", "-m", "hooks"]);
        // A relative path: inside the attempt's checkout it names the attempt's own files.
        sh(project.path(), &["config", "core.hooksPath", ".githooks"]);
        sh(project.path(), &["config", "core.fsmonitor", ".githooks/fsmonitor-watchman"]);
        let cache = tempfile::tempdir().unwrap();

        // create, review, approve.
        let wt = create(project.path(), &cache.path().join("s/r")).unwrap();
        std::fs::write(wt.cwd.join("b.txt"), "b\n").unwrap();
        let r = review(&wt).unwrap();
        assert!(matches!(approve(&wt, &r.patch_sha256, "rom").unwrap(), Merge::Merged(_)));
        // reject.
        let wt = create(project.path(), &cache.path().join("s/x")).unwrap();
        std::fs::write(wt.cwd.join("x.txt"), "x\n").unwrap();
        let r = review(&wt).unwrap();
        reject(&wt, &r.patch_sha256, "rom").unwrap();
        // release of a clean one, and of a dirty one.
        let wt = create(project.path(), &cache.path().join("s/y")).unwrap();
        assert!(release(wt).is_none());
        let wt = create(project.path(), &cache.path().join("s/z")).unwrap();
        std::fs::write(wt.cwd.join("z.txt"), "z\n").unwrap();
        assert!(release(wt).is_some());
        // The abort after a conflict.
        let wt = create(project.path(), &cache.path().join("s/c")).unwrap();
        std::fs::write(wt.cwd.join("a.txt"), "agent\n").unwrap();
        let r = review(&wt).unwrap();
        std::fs::write(project.path().join("a.txt"), "other\n").unwrap();
        git(project.path(), &["-c", "user.name=t", "-c", "user.email=t@e", "commit", "-qam", "x"]).unwrap();
        assert!(matches!(approve(&wt, &r.patch_sha256, "rom").unwrap(), Merge::Conflict(_)));
        let ran: Vec<_> = std::fs::read_dir(marks.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(ran.is_empty(), "hooks ran: {ran:?}");
    }
}
