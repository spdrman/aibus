//! XDG locations (07 section 1). aibus uses the XDG layout under `$HOME` on every platform,
//! macOS included, so the files are where a person looks for them.

use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    /// `~/.config/aibus`: `config.toml`, user agents and workflows.
    pub config: PathBuf,
    /// `~/.local/share/aibus`: memory and the index.
    pub data: PathBuf,
    /// `~/.local/state/aibus`: sessions and logs.
    pub state: PathBuf,
    /// `~/.cache/aibus`: model tables and worktrees.
    pub cache: PathBuf,
    /// `AIBUS_CONFIG` (or `--config`): replaces `<config>/config.toml`.
    pub config_override: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("cannot find a home directory: HOME is not set")]
pub struct NoHome;

impl Paths {
    /// `env` looks a variable up. An `XDG_*` value that is not an absolute path is ignored, as
    /// the XDG spec says.
    pub fn resolve(env: impl Fn(&str) -> Option<String>) -> Result<Self, NoHome> {
        let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
        let dir = |var: &str, fallback: &str| -> Result<PathBuf, NoHome> {
            let base = match env(var).map(PathBuf::from).filter(|p| p.is_absolute()) {
                Some(p) => p,
                None => home.as_ref().ok_or(NoHome)?.join(fallback),
            };
            Ok(base.join("aibus"))
        };
        Ok(Self {
            config: dir("XDG_CONFIG_HOME", ".config")?,
            data: dir("XDG_DATA_HOME", ".local/share")?,
            state: dir("XDG_STATE_HOME", ".local/state")?,
            cache: dir("XDG_CACHE_HOME", ".cache")?,
            config_override: env("AIBUS_CONFIG")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        })
    }

    /// The process environment.
    pub fn from_env() -> Result<Self, NoHome> {
        Self::resolve(|k| std::env::var(k).ok())
    }

    /// The user config file.
    pub fn config_file(&self) -> PathBuf {
        self.config_override
            .clone()
            .unwrap_or_else(|| self.config.join("config.toml"))
    }
}

/// `<project>/.aibus`.
pub fn project_dir(cwd: &Path) -> PathBuf {
    cwd.join(".aibus")
}

/// `<project>/thoughts`, or whatever `continuity.dir` names; an absolute value stays as it is.
pub fn continuity_dir(cwd: &Path, configured: &str) -> PathBuf {
    cwd.join(configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn defaults_are_xdg_under_home_on_every_platform() {
        let p = Paths::resolve(env(&[("HOME", "/Users/me")])).unwrap();
        assert_eq!(p.config, Path::new("/Users/me/.config/aibus"));
        assert_eq!(p.data, Path::new("/Users/me/.local/share/aibus"));
        assert_eq!(p.state, Path::new("/Users/me/.local/state/aibus"));
        assert_eq!(p.cache, Path::new("/Users/me/.cache/aibus"));
        assert_eq!(
            p.config_file(),
            Path::new("/Users/me/.config/aibus/config.toml")
        );
    }

    #[test]
    fn every_xdg_variable_and_aibus_config_override() {
        let p = Paths::resolve(env(&[
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_DATA_HOME", "/d"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/k"),
            ("AIBUS_CONFIG", "/etc/aibus.toml"),
        ]))
        .unwrap();
        assert_eq!(p.config, Path::new("/c/aibus"));
        assert_eq!(p.data, Path::new("/d/aibus"));
        assert_eq!(p.state, Path::new("/s/aibus"));
        assert_eq!(p.cache, Path::new("/k/aibus"));
        assert_eq!(p.config_file(), Path::new("/etc/aibus.toml"));
    }

    #[test]
    fn a_relative_xdg_value_is_ignored() {
        let p = Paths::resolve(env(&[("HOME", "/h"), ("XDG_DATA_HOME", "rel/path")])).unwrap();
        assert_eq!(p.data, Path::new("/h/.local/share/aibus"));
    }

    #[test]
    fn no_home_is_an_error_unless_every_xdg_variable_is_set() {
        assert_eq!(Paths::resolve(env(&[])), Err(NoHome));
        let all = [
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_DATA_HOME", "/d"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/k"),
        ];
        assert!(Paths::resolve(env(&all)).is_ok());
    }

    #[test]
    fn project_and_continuity_dirs() {
        let cwd = Path::new("/work/app");
        assert_eq!(project_dir(cwd), Path::new("/work/app/.aibus"));
        assert_eq!(
            continuity_dir(cwd, "thoughts"),
            Path::new("/work/app/thoughts")
        );
        assert_eq!(continuity_dir(cwd, "/abs/notes"), Path::new("/abs/notes"));
    }
}
