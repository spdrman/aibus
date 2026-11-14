//! The Continuous-Claude loop, native (01 section 8, D-11): the ledger, handoffs, per-agent
//! memory, recall over all of them, and the learnings extracted when a session ends.
//!
//! Everything here is plain files under the project and the data dir; `index.sqlite` is only a
//! cache over them.

pub mod handoff;
pub mod learn;
pub mod ledger;
pub mod memory;
pub mod recall;

use std::fs;
use std::path::{Path, PathBuf};

use crate::agent::discover::project_root;
use crate::agent::MemoryScope;
use crate::config::Config;

/// The topic when there is no ledger yet and nothing asked for one.
pub const DEFAULT_TOPIC: &str = "general";

/// Where a session's continuity files are, and which ones are in scope. Built once at startup
/// and shared by the tools, the compactor and the commands, which is why these fields travel
/// together.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Layout {
    /// The project root.
    pub project: PathBuf,
    /// `thoughts/`, or whatever `continuity.dir` names.
    pub dir: PathBuf,
    /// The aibus data dir: user-scope memory and `index.sqlite`.
    pub data: PathBuf,
    pub topic: String,
    /// `memory.scopes`, what recall and the index cover.
    pub scopes: Vec<MemoryScope>,
}

/// A topic is a path segment, so it is limited to what is safe in one.
pub fn clean_topic(raw: &str) -> Result<String, String> {
    let t = raw.trim().replace('/', "-");
    let ok = !t.is_empty()
        && !t.starts_with('.')
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(t)
    } else {
        Err(format!(
            "topic {raw:?}: use letters, digits, '-', '_' and '.' (not at the start)"
        ))
    }
}

impl Layout {
    /// The topic is `flag` (`--topic`); else the git branch when `continuity.topic = "branch"`;
    /// else the topic of the most recently modified ledger; else `general` (Q10).
    pub fn new(cwd: &Path, cfg: &Config, data: &Path, flag: Option<&str>) -> Result<Self, String> {
        let project = project_root(cwd).unwrap_or(cwd).to_path_buf();
        let dir = crate::persist::paths::continuity_dir(&project, &cfg.continuity.dir);
        let topic = match flag {
            Some(t) => clean_topic(t)?,
            None => {
                let branch = (cfg.continuity.topic == "branch")
                    .then(|| crate::context::gather_git(&project))
                    .flatten()
                    .map(|g| g.branch)
                    .filter(|b| b != "HEAD")
                    .and_then(|b| clean_topic(&b).ok());
                branch
                    .or_else(|| ledger::newest_topic(&dir))
                    .unwrap_or_else(|| DEFAULT_TOPIC.to_owned())
            }
        };
        let scopes = cfg
            .memory
            .scopes
            .iter()
            .filter_map(|s| MemoryScope::parse(s))
            .collect();
        Ok(Self {
            project,
            dir,
            data: data.to_path_buf(),
            topic,
            scopes,
        })
    }

    pub fn ledgers(&self) -> PathBuf {
        self.dir.join("ledgers")
    }

    pub fn ledger(&self) -> PathBuf {
        self.ledgers().join(format!("CONTINUITY_{}.md", self.topic))
    }

    /// `thoughts/shared/handoffs`, with one directory per topic in it.
    pub fn handoffs(&self) -> PathBuf {
        self.dir.join("shared/handoffs")
    }

    pub fn handoff_dir(&self) -> PathBuf {
        self.handoffs().join(&self.topic)
    }

    pub fn index(&self) -> PathBuf {
        self.data.join("index.sqlite")
    }

    pub fn has(&self, scope: MemoryScope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// `*.<ext>` files directly in `dir`.
fn files_with(dir: &Path, ext: &str) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == ext))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_topic_is_one_safe_path_segment() {
        assert_eq!(clean_topic(" auth-refactor ").unwrap(), "auth-refactor");
        assert_eq!(clean_topic("feature/login").unwrap(), "feature-login");
        for bad in ["", "..", ".hidden", "a b", "a\\b", "über"] {
            assert!(clean_topic(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_topic_comes_from_the_flag_then_the_newest_ledger_then_general() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let make = |flag| Layout::new(dir.path(), &cfg, dir.path(), flag).unwrap();
        assert_eq!(make(None).topic, "general");
        let ledgers = dir.path().join("thoughts/ledgers");
        fs::create_dir_all(&ledgers).unwrap();
        fs::write(ledgers.join("CONTINUITY_old.md"), "x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(ledgers.join("CONTINUITY_new.md"), "x").unwrap();
        assert_eq!(make(None).topic, "new");
        assert_eq!(make(Some("pinned")).topic, "pinned");
        assert!(Layout::new(dir.path(), &cfg, dir.path(), Some("a b")).is_err());
    }

    #[test]
    fn the_branch_option_names_the_topic_after_the_branch() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "feature/login"]);
        git(&["commit", "-q", "--allow-empty", "-m", "first"]);
        let mut cfg = Config::default();
        cfg.continuity.topic = "branch".into();
        let l = Layout::new(dir.path(), &cfg, dir.path(), None).unwrap();
        assert_eq!(l.topic, "feature-login");
        assert_eq!(
            l.ledger(),
            dir.path()
                .join("thoughts/ledgers/CONTINUITY_feature-login.md")
        );
        assert_eq!(
            l.handoff_dir(),
            dir.path().join("thoughts/shared/handoffs/feature-login")
        );
    }
}
