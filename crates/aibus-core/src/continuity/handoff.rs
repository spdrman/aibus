//! Handoffs (01 section 8.2): `thoughts/shared/handoffs/<topic>/<YYYY-MM-DD_HH-MM>_<slug>.yaml`
//! in the Continuous-Claude schema, kept as it is so the files go both ways. A small YAML front
//! matter (`session`, `date`, `status`, `outcome`) and then the body, which must have `goal` and
//! `now`.
//!
//! A handoff is held as the ordered YAML mapping it was read as, so a key this version does not
//! know about survives a read and a write.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value};
use yaml_rust2::yaml::Hash;
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

use super::ledger::Ledger;
use super::{files_with, Layout};
use crate::persist::fs::write_locked;
use crate::persist::time::rfc3339;
use crate::run::{ContentBlock, Conversation, MessageRole, RunStatus};

pub const STATUSES: [&str; 3] = ["complete", "partial", "blocked"];
pub const OUTCOMES: [&str; 4] = ["SUCCEEDED", "PARTIAL_PLUS", "PARTIAL_MINUS", "FAILED"];

/// The keys of the front matter, in order.
const FRONT: [&str; 4] = ["session", "date", "status", "outcome"];
/// The keys of the body the schema names, in order; any others follow.
const BODY: [&str; 12] = [
    "goal",
    "now",
    "test",
    "done_this_session",
    "blockers",
    "questions",
    "decisions",
    "findings",
    "worked",
    "failed",
    "next",
    "files",
];

/// The front matter of a handoff that is being written.
pub struct Front<'a> {
    pub session: &'a str,
    pub date: &'a str,
    pub status: &'a str,
    pub outcome: Option<&'a str>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Handoff(Hash);

fn key(k: &str) -> Yaml {
    Yaml::String(k.to_owned())
}

/// A scalar as text; a number or a boolean is what a person might write for `session`.
fn scalar(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Real(r) => Some(r.clone()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn to_yaml(v: &Value) -> Yaml {
    match v {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Boolean(*b),
        Value::Number(n) => n
            .as_i64()
            .map_or_else(|| Yaml::Real(n.to_string()), Yaml::Integer),
        Value::String(s) => Yaml::String(s.clone()),
        Value::Array(a) => Yaml::Array(a.iter().map(to_yaml).collect()),
        Value::Object(o) => Yaml::Hash(o.iter().map(|(k, v)| (key(k), to_yaml(v))).collect()),
    }
}

fn to_json(y: &Yaml) -> Value {
    match y {
        Yaml::Null | Yaml::BadValue | Yaml::Alias(_) => Value::Null,
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(i) => Value::from(*i),
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Real(r) => r
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(r.clone()), Value::Number),
        Yaml::Array(a) => Value::Array(a.iter().map(to_json).collect()),
        Yaml::Hash(h) => Value::Object(
            h.iter()
                .filter_map(|(k, v)| Some((scalar(k)?, to_json(v))))
                .collect(),
        ),
    }
}

impl Handoff {
    /// Reads a handoff: a front matter document and a body document, or one document that has
    /// both. `goal` and `now` have to be there.
    pub fn parse(text: &str) -> Result<Self, String> {
        // The schema's own example has `test: cargo test -p server auth::`, which a strict YAML
        // reader refuses (the trailing colon starts a mapping). The files this reads are written
        // by people and by models, so a text that fails is tried again with such values quoted.
        let docs = YamlLoader::load_from_str(text)
            .or_else(|first| YamlLoader::load_from_str(&quote_colons(text)).map_err(|_| first))
            .map_err(|e| e.to_string())?;
        let mut all = Hash::new();
        if docs.len() > 2 {
            return Err(format!(
                "{} YAML documents; want a front matter and a body",
                docs.len()
            ));
        }
        for doc in docs {
            let Yaml::Hash(h) = doc else {
                return Err("a handoff is a YAML mapping".into());
            };
            for (k, v) in h {
                all.insert(k, v);
            }
        }
        let h = Self(all);
        for required in ["goal", "now"] {
            if h.text(required).is_none_or(|t| t.trim().is_empty()) {
                return Err(format!("`{required}` is required"));
            }
        }
        Ok(h)
    }

    /// Builds one from the front matter and a JSON body (what the tool and the commands have).
    /// `goal` and `now` have to be non-empty strings; the status and the outcome have to be
    /// ones the schema names.
    pub fn build(front: &Front<'_>, body: &Map<String, Value>) -> Result<Self, String> {
        if !STATUSES.contains(&front.status) {
            return Err(format!(
                "status {}: want {}",
                front.status,
                STATUSES.join(", ")
            ));
        }
        if let Some(o) = front.outcome.filter(|o| !OUTCOMES.contains(o)) {
            return Err(format!("outcome {o}: want {}", OUTCOMES.join(", ")));
        }
        let mut h = Hash::new();
        let mut put = |k: &str, v: Yaml| {
            h.insert(key(k), v);
        };
        put("session", Yaml::String(front.session.to_owned()));
        put("date", Yaml::String(front.date.to_owned()));
        put("status", Yaml::String(front.status.to_owned()));
        if let Some(o) = front.outcome {
            put("outcome", Yaml::String(o.to_owned()));
        }
        for required in ["goal", "now"] {
            match body.get(required).and_then(Value::as_str) {
                Some(t) if !t.trim().is_empty() => {}
                _ => return Err(format!("`{required}` is required")),
            }
        }
        for k in BODY {
            if let Some(v) = body.get(k) {
                put(k, to_yaml(v));
            }
        }
        for (k, v) in body {
            if FRONT.contains(&k.as_str()) {
                return Err(format!("`{k}` belongs to the front matter"));
            }
            if !BODY.contains(&k.as_str()) {
                put(k, to_yaml(v));
            }
        }
        Ok(Self(h))
    }

    pub fn text(&self, k: &str) -> Option<String> {
        self.0.get(&key(k)).and_then(scalar)
    }

    pub fn goal(&self) -> String {
        self.text("goal").unwrap_or_default()
    }

    pub fn now(&self) -> String {
        self.text("now").unwrap_or_default()
    }

    /// Records the session's outcome in the front matter, after `status`.
    pub fn set_outcome(&mut self, outcome: &str) {
        let k = key("outcome");
        if self.0.contains_key(&k) {
            self.0.insert(k, Yaml::String(outcome.to_owned()));
            return;
        }
        // Rebuilt so that `outcome` lands in the front matter and not at the end.
        let mut out = Hash::new();
        let mut placed = false;
        for (name, value) in std::mem::take(&mut self.0) {
            let in_front = scalar(&name).is_some_and(|n| FRONT.contains(&n.as_str()));
            if !placed && !in_front {
                out.insert(k.clone(), Yaml::String(outcome.to_owned()));
                placed = true;
            }
            out.insert(name, value);
        }
        if !placed {
            out.insert(k, Yaml::String(outcome.to_owned()));
        }
        self.0 = out;
    }

    /// The front matter, then the body, as two YAML documents (`---` opens each).
    pub fn to_yaml(&self) -> String {
        let (mut front, mut body) = (Hash::new(), Hash::new());
        for (k, v) in &self.0 {
            let in_front = scalar(k).is_some_and(|n| FRONT.contains(&n.as_str()));
            let into = if in_front { &mut front } else { &mut body };
            into.insert(k.clone(), v.clone());
        }
        let docs: Vec<Yaml> = [front, body]
            .into_iter()
            .filter(|d| !d.is_empty())
            .map(Yaml::Hash)
            .collect();
        let mut out = String::new();
        for doc in &docs {
            if !out.is_empty() {
                out.push('\n');
            }
            // Writing to a String cannot fail.
            let _ = YamlEmitter::new(&mut out).dump(doc);
        }
        // The emitter writes some scalars (a control character inside a key, say) in a form its
        // own reader refuses, and a handoff that was read has to be writable again. JSON is YAML
        // too, so when the text does not read back as what was written, write that instead
        // (found by the handoff fuzz target, fixtures/fuzz/handoff).
        if !YamlLoader::load_from_str(&out).is_ok_and(|back| back == docs) {
            out = docs
                .iter()
                .map(|d| format!("---\n{}\n", to_json(d)))
                .collect::<Vec<_>>()
                .join("\n");
        }
        out.push('\n');
        out
    }

    /// The YAML as one JSON object (06 section 4).
    pub fn to_json(&self) -> Value {
        to_json(&Yaml::Hash(self.0.clone()))
    }

    /// The line that opens what the lead is shown at `session_start`.
    pub fn line(&self) -> String {
        format!(
            "[handoff] goal: {} now: {}",
            one_line(&self.goal()),
            one_line(&self.now())
        )
    }
}

/// `text` with every plain `key: value` whose value has a `: ` in it, or ends in `:`, written as
/// a single-quoted value. Lines of a block scalar are not told apart, which is why this only
/// runs on text that did not parse.
fn quote_colons(text: &str) -> String {
    let fixed = |line: &str| -> Option<String> {
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        let (dash, rest) = body.strip_prefix("- ").map_or(("", body), |r| ("- ", r));
        let (k, v) = rest.split_once(": ")?;
        let plain_key = !k.is_empty()
            && k.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'));
        let v = v.trim();
        let plain_value =
            !v.is_empty() && !v.starts_with(['"', '\'', '[', '{', '|', '>', '&', '*', '!', '#']);
        (plain_key && plain_value && (v.contains(": ") || v.ends_with(':')))
            .then(|| format!("{indent}{dash}{k}: '{}'", v.replace('\'', "''")))
    };
    let mut out: Vec<String> = text
        .lines()
        .map(|l| fixed(l).unwrap_or_else(|| l.to_owned()))
        .collect();
    out.push(String::new());
    out.join("\n")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text injected at `session_start`: the `[handoff]` line and the whole file.
pub fn context(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let h = Handoff::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{}\n\n{}", h.line(), text.trim_end()))
}

/// The first line of `text`, cut to `max` characters.
fn head(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    line.chars().take(max).collect()
}

fn text_of(c: &Conversation, role: MessageRole, first: bool) -> Option<String> {
    let mut texts = c
        .messages
        .iter()
        .filter(|m| m.role == role)
        .filter_map(|m| {
            m.blocks.iter().find_map(|b| match b {
                ContentBlock::Text(t) if !t.trim().is_empty() => Some(t.clone()),
                _ => None,
            })
        });
    if first {
        texts.next()
    } else {
        texts.next_back()
    }
}

/// What the ledger and the conversation say about where the work stands, as a handoff body:
/// the ledger's goal and first open item, falling back to the first user message and the last
/// assistant one. It never fails, since a handoff written at a compaction must not stop one.
pub fn summarise(ledger: Option<&Ledger>, conv: Option<&Conversation>) -> Map<String, Value> {
    let from_conv = |role, first, max| {
        conv.and_then(|c| text_of(c, role, first))
            .map(|t| head(&t, max))
            .filter(|t| !t.is_empty())
    };
    let goal = ledger
        .map(|l| head(l.goal(), 300))
        .filter(|g| !g.is_empty())
        .or_else(|| from_conv(MessageRole::User, true, 300))
        .unwrap_or_else(|| "no goal was recorded".into());
    let now = ledger
        .and_then(Ledger::in_progress)
        .map(str::to_owned)
        .or_else(|| from_conv(MessageRole::Assistant, false, 300))
        .unwrap_or_else(|| "pick up from the ledger".into());
    let mut body = Map::new();
    body.insert("goal".into(), goal.into());
    body.insert("now".into(), now.into());
    if let Some(l) = ledger {
        let list = |name| {
            Value::from(
                l.items(name)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            )
        };
        let done: Vec<Value> = l
            .items("Completed")
            .into_iter()
            .map(|t| Value::Object(Map::from_iter([("task".to_owned(), t.into())])))
            .collect();
        body.insert("done_this_session".into(), done.into());
        body.insert("blockers".into(), list("Blockers"));
        body.insert("decisions".into(), list("Decisions"));
        let next: Vec<_> = l
            .items("In Progress")
            .into_iter()
            .skip(1)
            .map(str::to_owned)
            .collect();
        body.insert("next".into(), next.into());
    }
    body
}

/// The handoff `status` for how a run ended.
pub fn status_of(run: RunStatus) -> &'static str {
    match run {
        RunStatus::Completed => "complete",
        RunStatus::Failed | RunStatus::Denied => "blocked",
        _ => "partial",
    }
}

/// `YYYY-MM-DD_HH-MM`, or with `-SS` when `secs` is set.
pub fn stamp(at: SystemTime, secs: bool) -> String {
    let t = rfc3339(at);
    let mut out = format!("{}_{}-{}", &t[..10], &t[11..13], &t[14..16]);
    if secs {
        out.push_str(&format!("-{}", &t[17..19]));
    }
    out
}

fn slug(goal: &str) -> String {
    let words: Vec<String> = goal
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .map(str::to_lowercase)
        .collect();
    let s: String = words
        .join("-")
        .chars()
        .filter(char::is_ascii)
        .take(40)
        .collect();
    if s.is_empty() {
        "handoff".into()
    } else {
        s
    }
}

/// Writes the handoff under the topic's directory, named `<stamp>_<slug>.yaml`, or
/// `auto-handoff-<stamp>.yaml` for one written without being asked. A name already taken gets
/// `-2`, `-3`, and so on, and the write holds the file lock.
pub fn write(layout: &Layout, h: &Handoff, auto: bool, at: SystemTime) -> io::Result<PathBuf> {
    let dir = layout.handoff_dir();
    fs::create_dir_all(&dir)?;
    let base = if auto {
        format!("auto-handoff-{}", stamp(at, true))
    } else {
        format!("{}_{}", stamp(at, false), slug(&h.goal()))
    };
    for n in 1.. {
        let name = if n == 1 {
            format!("{base}.yaml")
        } else {
            format!("{base}-{n}.yaml")
        };
        let path = dir.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {
                write_locked(&path, h.to_yaml())?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop only ends by returning")
}

/// What a handoff's name sorts by: its timestamp, wherever `auto-handoff-` puts it.
fn order(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        name.strip_prefix("auto-handoff-")
            .unwrap_or(&name)
            .to_owned(),
        name,
    )
}

/// Every handoff of `topic`, or of all topics, newest first (the newest name wins, 07
/// section 5).
pub fn list(root: &Path, topic: Option<&str>) -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = match topic {
        Some(t) => vec![root.join(t)],
        None => fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
    };
    let mut all: Vec<PathBuf> = dirs.iter().flat_map(|d| files_with(d, "yaml")).collect();
    all.sort_by_key(|p| std::cmp::Reverse(order(p)));
    all
}

/// The newest handoff of `topic`, or of any topic.
pub fn latest(root: &Path, topic: Option<&str>) -> Option<PathBuf> {
    list(root, topic).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::time::Duration;

    const SPEC: &str = "---
session: auth-refactor
date: 2026-11-07
status: partial            # complete | partial | blocked
outcome: PARTIAL_PLUS      # SUCCEEDED | PARTIAL_PLUS | PARTIAL_MINUS | FAILED
---
goal: Move session auth to middleware
now: Run the migration and add one test for the expired-token path
test: cargo test -p server auth::
done_this_session:
  - task: Extracted AuthLayer
    files: [src/auth/layer.rs, src/main.rs]
blockers: []
questions: [Should refresh tokens rotate on every use?]
decisions:
  - middleware_over_extractor: keeps handlers free of auth plumbing
findings:
  - fixture_bug: test failed because the fixture, not the code, was stale
worked: [running the failing test alone first]
failed: [grepping for the error text, it is templated]
next:
  - Run migration 0042
  - Add expired-token test
files:
  created: [src/auth/layer.rs]
  modified: [src/main.rs, Cargo.toml]
";

    fn layout(dir: &Path) -> Layout {
        Layout::new(dir, &Config::default(), dir, Some("auth")).unwrap()
    }

    fn at(secs: u64) -> SystemTime {
        // 2026-11-08 09:15:30 UTC
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_129_330 + secs)
    }

    #[test]
    fn the_schema_example_round_trips_without_losing_anything() {
        let h = Handoff::parse(SPEC).unwrap();
        assert_eq!(h.goal(), "Move session auth to middleware");
        assert_eq!(h.text("outcome").as_deref(), Some("PARTIAL_PLUS"));
        let again = Handoff::parse(&h.to_yaml()).unwrap();
        assert_eq!(again, h);
        assert_eq!(again.to_json(), h.to_json());
        let j = h.to_json();
        assert_eq!(j["files"]["modified"][1], "Cargo.toml");
        assert_eq!(
            j["decisions"][0]["middleware_over_extractor"],
            "keeps handlers free of auth plumbing"
        );
        assert_eq!(j["blockers"], serde_json::json!([]));
    }

    #[test]
    fn the_output_has_the_front_matter_first_then_the_body() {
        let h = Handoff::parse(SPEC).unwrap();
        let text = h.to_yaml();
        assert!(text.starts_with("---\nsession: auth-refactor\ndate: 2026-11-07\nstatus: partial\noutcome: PARTIAL_PLUS\n---\ngoal: "), "{text}");
    }

    #[test]
    fn goal_and_now_are_required() {
        assert!(Handoff::parse("goal: a\n").unwrap_err().contains("now"));
        assert!(Handoff::parse("now: a\n").unwrap_err().contains("goal"));
        assert!(Handoff::parse("goal: ''\nnow: b\n")
            .unwrap_err()
            .contains("goal"));
        assert!(Handoff::parse("- a\n- b\n").is_err());
        assert!(Handoff::parse("a: [\n").is_err());
        // One document with everything in it is fine too.
        assert!(Handoff::parse("session: s\ngoal: a\nnow: b\n").is_ok());
    }

    #[test]
    fn keys_this_version_does_not_know_survive() {
        let h = Handoff::parse(
            "---\nsession: s\nmood: tired\n---\ngoal: a\nnow: b\nmy_notes: [1, two]\n",
        )
        .unwrap();
        let again = Handoff::parse(&h.to_yaml()).unwrap();
        assert_eq!(again.to_json()["my_notes"], serde_json::json!([1, "two"]));
        assert_eq!(again.to_json()["mood"], "tired");
    }

    #[test]
    fn build_checks_the_front_and_the_two_required_fields() {
        let body = |v: Value| v.as_object().unwrap().clone();
        let front = |status, outcome| Front {
            session: "s",
            date: "2026-11-08",
            status,
            outcome,
        };
        let ok = Handoff::build(
            &front("partial", Some("FAILED")),
            &body(serde_json::json!({"now": "n", "goal": "g", "next": ["a", "b: c"], "extra": 1})),
        )
        .unwrap();
        let text = ok.to_yaml();
        assert!(text.find("goal:").unwrap() < text.find("now:").unwrap());
        assert!(text.find("next:").unwrap() < text.find("extra:").unwrap());
        assert_eq!(Handoff::parse(&text).unwrap(), ok);
        let b = body(serde_json::json!({"goal": "g", "now": "n"}));
        assert!(Handoff::build(&front("done", None), &b)
            .unwrap_err()
            .contains("status"));
        assert!(Handoff::build(&front("partial", Some("GREAT")), &b)
            .unwrap_err()
            .contains("outcome"));
        assert!(Handoff::build(
            &front("partial", None),
            &body(serde_json::json!({"goal": "g"}))
        )
        .is_err());
        assert!(Handoff::build(
            &front("partial", None),
            &body(serde_json::json!({"goal": "g", "now": 4}))
        )
        .is_err());
        assert!(Handoff::build(
            &front("partial", None),
            &body(serde_json::json!({"goal": "g", "now": "n", "status": "x"}))
        )
        .is_err());
    }

    #[test]
    fn awkward_text_survives_the_emitter() {
        let b = serde_json::json!({"goal": "fix: the \"quoted\" # thing", "now": "line one\nline two", "questions": ["true", "123", "- dash", "null"]});
        let front = Front {
            session: "s",
            date: "2026-11-08",
            status: "partial",
            outcome: None,
        };
        let h = Handoff::build(&front, b.as_object().unwrap()).unwrap();
        let again = Handoff::parse(&h.to_yaml()).unwrap();
        assert_eq!(again.to_json(), h.to_json());
        assert_eq!(again.to_json()["questions"], b["questions"]);
    }

    #[test]
    fn set_outcome_goes_into_the_front_matter() {
        let mut h =
            Handoff::parse("---\nsession: s\nstatus: partial\n---\ngoal: a\nnow: b\n").unwrap();
        h.set_outcome("SUCCEEDED");
        assert!(h
            .to_yaml()
            .starts_with("---\nsession: s\nstatus: partial\noutcome: SUCCEEDED\n---\ngoal"));
        h.set_outcome("FAILED");
        assert_eq!(h.text("outcome").as_deref(), Some("FAILED"));
        let mut bare = Handoff::parse("goal: a\nnow: b\n").unwrap();
        bare.set_outcome("FAILED");
        assert!(bare
            .to_yaml()
            .starts_with("---\noutcome: FAILED\n---\ngoal"));
    }

    #[test]
    fn the_context_line_is_goal_and_now_on_one_line() {
        let h = Handoff::parse("goal: |\n  a\n  b\nnow: c\n").unwrap();
        assert_eq!(h.line(), "[handoff] goal: a b now: c");
    }

    #[test]
    fn names_follow_the_spec_and_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let l = layout(dir.path());
        let h = Handoff::parse(SPEC).unwrap();
        let a = write(&l, &h, false, at(0)).unwrap();
        assert_eq!(
            a,
            dir.path().join("thoughts/shared/handoffs/auth/2026-11-08_09-15_move-session-auth-to-middleware.yaml")
        );
        let b = write(&l, &h, false, at(1)).unwrap();
        assert_eq!(
            b.file_name().unwrap(),
            "2026-11-08_09-15_move-session-auth-to-middleware-2.yaml"
        );
        let auto = write(&l, &h, true, at(0)).unwrap();
        assert_eq!(
            auto.file_name().unwrap(),
            "auto-handoff-2026-11-08_09-15-30.yaml"
        );
        assert_eq!(Handoff::parse(&fs::read_to_string(a).unwrap()).unwrap(), h);
    }

    #[test]
    fn the_newest_timestamp_wins_even_for_an_auto_handoff() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "general/2026-11-07_23-52_review.yaml",
            "auth/2026-11-08_09-00_login.yaml",
            "auth/auto-handoff-2026-11-08_09-10-05.yaml",
            "auth/auto-handoff-2026-11-06_09-10-05.yaml",
            "auth/notes.txt",
        ] {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        assert_eq!(
            latest(dir.path(), None).unwrap(),
            dir.path()
                .join("auth/auto-handoff-2026-11-08_09-10-05.yaml")
        );
        assert_eq!(
            latest(dir.path(), Some("general")).unwrap(),
            dir.path().join("general/2026-11-07_23-52_review.yaml")
        );
        assert_eq!(list(dir.path(), Some("auth")).len(), 3);
        assert!(latest(dir.path(), Some("none")).is_none());
        assert!(latest(&dir.path().join("missing"), None).is_none());
    }

    #[test]
    fn a_summary_prefers_the_ledger_and_falls_back_to_the_conversation() {
        let mut ledger = Ledger::new("auth", "2026-11-08");
        ledger.update("goal", "Ship auth", false, "d").unwrap();
        ledger
            .update("in progress", "write tests\nrun migration", false, "d")
            .unwrap();
        ledger.update("completed", "layer", false, "d").unwrap();
        let s = summarise(Some(&ledger), None);
        assert_eq!(s["goal"], "Ship auth");
        assert_eq!(s["now"], "write tests");
        assert_eq!(s["next"], serde_json::json!(["run migration"]));
        assert_eq!(s["done_this_session"][0]["task"], "layer");

        let conv = Conversation {
            messages: vec![
                aibus_provider::Message::text(MessageRole::User, "fix the login bug\nmore"),
                aibus_provider::Message::text(MessageRole::Assistant, "looking"),
                aibus_provider::Message::text(MessageRole::Assistant, "found it in auth.rs"),
            ],
            ..Default::default()
        };
        let s = summarise(None, Some(&conv));
        assert_eq!(s["goal"], "fix the login bug");
        assert_eq!(s["now"], "found it in auth.rs");
        let s = summarise(None, None);
        assert!(s["goal"].as_str().unwrap().contains("no goal"));
        let front = Front {
            session: "s",
            date: "d",
            status: "partial",
            outcome: None,
        };
        Handoff::build(&front, &s).unwrap();
    }

    #[test]
    fn statuses_and_stamps() {
        assert_eq!(status_of(RunStatus::Completed), "complete");
        assert_eq!(status_of(RunStatus::Failed), "blocked");
        assert_eq!(status_of(RunStatus::Partial), "partial");
        assert_eq!(stamp(at(0), false), "2026-11-08_09-15");
        assert_eq!(stamp(at(0), true), "2026-11-08_09-15-30");
        assert_eq!(
            slug("Fix: the login!! bug in auth now please"),
            "fix-the-login-bug-in"
        );
        assert_eq!(slug("???"), "handoff");
    }
}
