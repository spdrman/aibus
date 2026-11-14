//! Trigger hints (01 section 4.1, D-07): `.aibus/triggers.toml` maps keywords and regexes to agent
//! names, and a prompt that matches gets `[hint] matching agents: a, b` added to the lead's turn.
//! Nothing here starts a run; the model reads the hint and decides.
//!
//! ```toml
//! [[trigger]]
//! agents = ["sleuth", "scout"]
//! keywords = ["crash", "stack trace"]    # case-insensitive, anywhere in the prompt
//! patterns = ["fail(s|ed)? silently"]    # regexes, case-sensitive unless they say (?i)
//! ```

use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::Deserialize;

/// A pattern that compiles to more than this is refused.
const REGEX_BYTES: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, rename = "trigger")]
    triggers: Vec<Raw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    agents: Vec<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    patterns: Vec<String>,
}

#[derive(Debug)]
struct Trigger {
    agents: Vec<String>,
    keywords: Vec<String>,
    patterns: Vec<Regex>,
}

#[derive(Debug, Default)]
pub struct Triggers(Vec<Trigger>);

impl Triggers {
    /// Parses a `triggers.toml`. `Err` names the entry that is wrong.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for (i, t) in file.triggers.into_iter().enumerate() {
            let n = i + 1;
            if t.agents.is_empty() || t.agents.iter().any(|a| crate::ids::AgentName::new(a).is_err()) {
                return Err(format!("trigger {n}: agents wants a list of agent names"));
            }
            if t.keywords.is_empty() && t.patterns.is_empty() {
                return Err(format!("trigger {n}: it needs keywords or patterns"));
            }
            let patterns = t
                .patterns
                .iter()
                .map(|p| {
                    RegexBuilder::new(p)
                        .size_limit(REGEX_BYTES)
                        .build()
                        .map_err(|e| format!("trigger {n}: pattern {p:?}: {e}"))
                })
                .collect::<Result<_, _>>()?;
            out.push(Trigger {
                agents: t.agents,
                keywords: t.keywords.iter().map(|k| k.to_lowercase()).collect(),
                patterns,
            });
        }
        Ok(Self(out))
    }

    /// The project's file and then the user's, each when it exists. A file that does not parse is
    /// reported in the warnings and left out.
    pub fn load(files: &[PathBuf]) -> (Self, Vec<String>) {
        let mut all = Vec::new();
        let mut warnings = Vec::new();
        for path in files {
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            match Self::parse(&text) {
                Ok(t) => all.extend(t.0),
                Err(e) => warnings.push(format!("{}: {e}", path.display())),
            }
        }
        (Self(all), warnings)
    }

    /// `<project>/.aibus/triggers.toml`, then `<config>/triggers.toml`.
    pub fn files(project: &Path, config: Option<&Path>) -> Vec<PathBuf> {
        std::iter::once(crate::persist::paths::project_dir(project).join("triggers.toml"))
            .chain(config.map(|c| c.join("triggers.toml")))
            .collect()
    }

    /// The agents whose triggers match `prompt`, each once, in the order they matched.
    pub fn matching(&self, prompt: &str) -> Vec<&str> {
        let lower = prompt.to_lowercase();
        let mut out: Vec<&str> = Vec::new();
        for t in &self.0 {
            let hit = t.keywords.iter().any(|k| lower.contains(k.as_str()))
                || t.patterns.iter().any(|p| p.is_match(prompt));
            if hit {
                for a in &t.agents {
                    if !out.contains(&a.as_str()) {
                        out.push(a);
                    }
                }
            }
        }
        out
    }

    /// The reminder for `prompt`, if anything matched.
    pub fn hint(&self, prompt: &str) -> Option<String> {
        let agents = self.matching(prompt);
        (!agents.is_empty()).then(|| format!("[hint] matching agents: {}", agents.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
[[trigger]]
agents = ["sleuth", "scout"]
keywords = ["Crash", "stack trace"]
patterns = ["fail(s|ed)? silently"]

[[trigger]]
agents = ["scout", "arbiter"]
keywords = ["flaky"]
"#;

    #[test]
    fn keywords_ignore_case_and_patterns_are_regexes() {
        let t = Triggers::parse(FILE).unwrap();
        assert_eq!(t.hint("the app CRASHES on start").as_deref(), Some("[hint] matching agents: sleuth, scout"));
        assert_eq!(t.hint("login fails silently").as_deref(), Some("[hint] matching agents: sleuth, scout"));
        assert_eq!(t.hint("a flaky crash").as_deref(), Some("[hint] matching agents: sleuth, scout, arbiter"));
        assert_eq!(t.hint("rename a variable"), None);
    }

    #[test]
    fn a_bad_entry_is_named() {
        assert!(Triggers::parse("[[trigger]]\nagents = []\nkeywords = [\"a\"]").unwrap_err().contains("trigger 1"));
        assert!(Triggers::parse("[[trigger]]\nagents = [\"a\"]").unwrap_err().contains("keywords or patterns"));
        assert!(Triggers::parse("[[trigger]]\nagents = [\"a\"]\npatterns = [\"(\"]").unwrap_err().contains("pattern"));
        assert!(Triggers::parse("[[trigger]]\nagents = [\"a\"]\nkeywords = [\"k\"]\nwhen = 1").is_err());
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.toml");
        let bad = dir.path().join("bad.toml");
        std::fs::write(&good, FILE).unwrap();
        std::fs::write(&bad, "[[trigger]]").unwrap();
        let (t, warnings) = Triggers::load(&[bad, good, dir.path().join("missing.toml")]);
        assert_eq!(warnings.len(), 1);
        assert!(t.hint("a crash").is_some());
    }
}
