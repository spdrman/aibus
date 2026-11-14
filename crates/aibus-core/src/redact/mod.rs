//! The redactor (09 section 4): a function every writer calls before output leaves the process,
//! and a `tracing` layer that does it for log lines.
//!
//! Two halves. Registered secrets are replaced by exact value (and by their base64) with
//! `[redacted:<account>]`; that half is reliable. The patterns in `patterns.toml` catch the
//! shapes of keys nobody registered, as `[redacted:pattern:<name>]`; that half is best effort.

mod layer;

pub use layer::layer;

use std::borrow::Cow;
use std::sync::{Arc, RwLock};

use base64::Engine;
use regex::Regex;
use serde::Deserialize;

use crate::ids::AccountName;

struct Known {
    account: AccountName,
    exact: String,
    base64: String,
}

struct Pattern {
    name: String,
    re: Regex,
}

struct Inner {
    known: RwLock<Vec<Known>>,
    patterns: Vec<Pattern>,
}

/// Cheap to clone; every clone shares the same registered secrets.
#[derive(Clone)]
pub struct Redactor(Arc<Inner>);

#[derive(Deserialize)]
struct PatternFile {
    pattern: Vec<PatternDef>,
}

#[derive(Deserialize)]
struct PatternDef {
    name: String,
    regex: String,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    /// A redactor with the built-in patterns and no registered secrets.
    pub fn new() -> Self {
        // The file is compiled in and the `patterns_compile` test loads it, so this cannot fail
        // at run time.
        let file: PatternFile = toml::from_str(include_str!("patterns.toml"))
            .expect("redact/patterns.toml is valid TOML");
        let patterns = file
            .pattern
            .into_iter()
            .map(|p| Pattern {
                re: Regex::new(&p.regex).expect("redact/patterns.toml holds valid regexes"),
                name: p.name,
            })
            .collect();
        Self(Arc::new(Inner {
            known: RwLock::new(Vec::new()),
            patterns,
        }))
    }

    /// Registers a live credential for as long as it is resolved. A second call for the same
    /// account replaces the first (a rotated key).
    pub fn register(&self, account: &AccountName, secret: &str) {
        // A three-letter "secret" replaced everywhere would wreck the output and protect nothing.
        if secret.len() < 6 {
            return;
        }
        let entry = Known {
            account: account.clone(),
            exact: secret.to_owned(),
            base64: base64::engine::general_purpose::STANDARD.encode(secret),
        };
        let mut known = self.0.known.write().unwrap_or_else(|e| e.into_inner());
        known.retain(|k| k.account != *account);
        known.push(entry);
        // Longest first, so a secret that contains another is replaced whole.
        known.sort_by_key(|k| std::cmp::Reverse(k.exact.len()));
    }

    pub fn unregister(&self, account: &AccountName) {
        let mut known = self.0.known.write().unwrap_or_else(|e| e.into_inner());
        known.retain(|k| k.account != *account);
    }

    /// `text` with registered secrets (exact and base64) and pattern matches replaced.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        {
            let known = self.0.known.read().unwrap_or_else(|e| e.into_inner());
            for k in known.iter() {
                let tag = format!("[redacted:{}]", k.account.0);
                for needle in [&k.exact, &k.base64] {
                    if out.contains(needle.as_str()) {
                        out = Cow::Owned(out.replace(needle.as_str(), &tag));
                    }
                }
            }
        }
        for p in &self.0.patterns {
            if p.re.is_match(&out) {
                let tag = format!("[redacted:pattern:{}]", p.name);
                out = Cow::Owned(p.re.replace_all(&out, regex::NoExpand(&tag)).into_owned());
            }
        }
        out
    }

    /// Redacts every string in a JSON value in place (object keys are left alone).
    pub fn redact_json(&self, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                if let Cow::Owned(new) = self.redact(s) {
                    *s = new;
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| self.redact_json(x)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|x| self.redact_json(x)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(n: &str) -> AccountName {
        AccountName(n.into())
    }

    #[test]
    fn patterns_compile() {
        let r = Redactor::new();
        let in_file = include_str!("patterns.toml")
            .lines()
            .filter(|l| l.trim_start().starts_with("[[pattern]]"))
            .count();
        assert_eq!(
            r.0.patterns.len(),
            in_file,
            "every [[pattern]] in patterns.toml compiles"
        );
    }

    #[test]
    fn exact_and_base64_are_replaced_with_the_account_name() {
        let r = Redactor::new();
        r.register(&acct("work"), "hunter2-hunter2");
        let b64 = base64::engine::general_purpose::STANDARD.encode("hunter2-hunter2");
        let text = format!("key=hunter2-hunter2 and {b64} again");
        assert_eq!(
            r.redact(&text),
            "key=[redacted:work] and [redacted:work] again"
        );
    }

    #[test]
    fn unregister_stops_replacing() {
        let r = Redactor::new();
        r.register(&acct("a"), "secret-value");
        r.unregister(&acct("a"));
        assert_eq!(r.redact("secret-value"), "secret-value");
    }

    #[test]
    fn a_rotated_key_replaces_the_old_registration() {
        let r = Redactor::new();
        r.register(&acct("a"), "old-secret-1");
        r.register(&acct("a"), "new-secret-2");
        assert_eq!(
            r.redact("old-secret-1 new-secret-2"),
            "old-secret-1 [redacted:a]"
        );
    }

    #[test]
    fn a_tiny_secret_is_not_registered() {
        let r = Redactor::new();
        r.register(&acct("a"), "ab");
        assert_eq!(r.redact("a cab at the lab"), "a cab at the lab");
    }

    #[test]
    fn json_strings_are_redacted_in_place() {
        let r = Redactor::new();
        r.register(&acct("a"), "secret-value");
        let mut v = serde_json::json!({"x": ["has secret-value inside", 3], "secret-value": "k"});
        r.redact_json(&mut v);
        assert_eq!(
            v,
            serde_json::json!({"x": ["has [redacted:a] inside", 3], "secret-value": "k"})
        );
    }
}
