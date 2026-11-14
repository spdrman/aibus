//! Learning extraction (01 section 8.3, Q11): at the end of a session the lead's model is asked
//! once, in a fresh context, for up to ten things worth remembering. They are appended to
//! `agent-memory/lead/LEARNINGS.md` in the user's data dir and indexed with the rest.

use std::fs;
use std::io;
use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::persist::fs::update_locked;
use crate::redact::Redactor;
use crate::run::{Conversation, ModelCompactor};

pub const KINDS: [&str; 4] = ["working_solution", "failed_approach", "decision", "fact"];
pub const CONFIDENCES: [&str; 3] = ["low", "medium", "high"];
/// The most one session adds.
pub const MAX: usize = 10;

const INSTRUCTION: &str = "Read the conversation below and extract up to ten learnings that would help in a later \
session on the same project: solutions that worked, approaches that failed, decisions with their reasons, and facts about \
the code or the people. Skip anything that is only true of this one session. Answer with a JSON array and nothing else; \
each element is {\"type\": \"working_solution\" | \"failed_approach\" | \"decision\" | \"fact\", \"confidence\": \
\"low\" | \"medium\" | \"high\", \"content\": \"one or two sentences\"}. Answer [] when nothing is worth keeping.";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Learning {
    pub kind: String,
    pub confidence: String,
    pub content: String,
}

/// The entries in a model's reply: the first JSON array in it, at most `MAX` valid elements.
/// A reply that is not that gives none, and an element with an unknown type, an unknown
/// confidence or no content is dropped.
pub fn parse(reply: &str) -> Vec<Learning> {
    let (Some(start), Some(end)) = (reply.find('['), reply.rfind(']')) else {
        return Vec::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&reply[start..=end.max(start)]) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| {
            let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim);
            let (kind, confidence, content) = (field("type")?, field("confidence")?, field("content")?);
            (KINDS.contains(&kind) && CONFIDENCES.contains(&confidence) && !content.is_empty()).then(|| Learning {
                kind: kind.to_owned(),
                confidence: confidence.to_owned(),
                content: content.split_whitespace().collect::<Vec<_>>().join(" "),
            })
        })
        .take(MAX)
        .collect()
}

/// A heading and one list line per entry.
pub fn render(entries: &[Learning], heading: &str) -> String {
    let mut out = format!("## {heading}\n\n");
    for e in entries {
        out.push_str(&format!("- [{}, {}] {}\n", e.kind, e.confidence, e.content));
    }
    out
}

/// Appends the entries to `file` under `heading`, redacted, holding the file lock. A file two
/// sessions end into at once keeps both.
pub fn append(file: &Path, entries: &[Learning], heading: &str, redactor: &Redactor) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let block = redactor.redact(&render(entries, heading)).into_owned();
    update_locked(file, |old| {
        let mut text = old.to_owned();
        if !text.is_empty() {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
        }
        text.push_str(&block);
        Ok(text)
    })
}

impl ModelCompactor {
    /// Asks the model once for the learnings in `conv` (the lead's compacted conversation).
    pub async fn extract(
        &self,
        conv: &Conversation,
        cancel: &CancellationToken,
    ) -> Result<Vec<Learning>, String> {
        let system = "You extract lasting lessons from engineering sessions.";
        let (reply, _) = self.ask(system, INSTRUCTION, conv, cancel).await?;
        Ok(parse(&reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_with_chatter_around_the_array_still_parses() {
        let reply = "Here you go:\n```json\n[{\"type\":\"fact\",\"confidence\":\"high\",\"content\":\"the cache key is the file hash\"},\
            {\"type\":\"decision\",\"confidence\":\"low\",\"content\":\"use\\nmiddleware\"}]\n```\nDone.";
        let got = parse(reply);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Learning { kind: "fact".into(), confidence: "high".into(), content: "the cache key is the file hash".into() });
        assert_eq!(got[1].content, "use middleware");
    }

    #[test]
    fn bad_elements_and_bad_replies_give_nothing() {
        assert!(parse("").is_empty() && parse("[]").is_empty() && parse("no array").is_empty());
        assert!(parse("] [").is_empty() && parse("[not json]").is_empty());
        let mixed = r#"[{"type":"gossip","confidence":"high","content":"x"},
            {"type":"fact","confidence":"sure","content":"x"},
            {"type":"fact","confidence":"high","content":"  "},
            {"type":"fact","confidence":"high"},
            "text",
            {"type":"failed_approach","confidence":"medium","content":"grep for the error"}]"#;
        let got = parse(mixed);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, "failed_approach");
    }

    #[test]
    fn at_most_ten_are_kept() {
        let many: Vec<_> = (0..14)
            .map(|i| serde_json::json!({"type": "fact", "confidence": "low", "content": format!("fact {i}")}))
            .collect();
        let got = parse(&serde_json::Value::Array(many).to_string());
        assert_eq!(got.len(), 10);
        assert_eq!(got[9].content, "fact 9");
    }

    #[test]
    fn entries_are_appended_under_a_heading_and_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent-memory/lead/LEARNINGS.md");
        let redactor = Redactor::new();
        redactor.register(&crate::ids::AccountName("a".into()), "sk-secret-value");
        let one = [Learning { kind: "fact".into(), confidence: "high".into(), content: "uses sk-secret-value".into() }];
        append(&file, &one, "2026-11-08 session A", &redactor).unwrap();
        append(&file, &one, "2026-11-09 session B", &redactor).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("## 2026-11-08 session A\n\n- [fact, high] uses "), "{text}");
        assert!(text.contains("\n\n## 2026-11-09 session B\n"));
        assert!(!text.contains("sk-secret-value"), "{text}");
        assert_eq!(text.matches("- [fact, high]").count(), 2);
    }
}
