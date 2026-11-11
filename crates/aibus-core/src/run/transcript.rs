//! A run's conversation on disk (01 section 5.4): `<session>/runs/<run>/conversation.jsonl`, one
//! JSON object per line. It sits beside the display transcript (`transcript.jsonl`, written by
//! `persist::transcript` from the `Msg`s, with long results cut), because a resume needs the
//! messages exactly as the provider saw them. It keeps everything: compaction only cuts the live
//! context and adds a `compact_boundary` line.
//!
//! Lines: `start` (who the run is, once per start or resume), `system` (the system prompt, once),
//! `message` (one conversation message) and `compact_boundary` (`pre_tokens` and the messages
//! that make up the context from then on).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{ContentBlock, Conversation, Message};
use crate::ids::{AccountName, AgentName, ModelId, RunId};
use crate::persist::fs::create_private_dir;

/// How a start of the run came about.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Run,
    /// A run that inherited its parent's conversation (01 section 5.2).
    Fork,
    Resume,
}

/// What a `start` line records, and so what a resume reads back.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RunMeta {
    pub kind: Kind,
    pub agent: AgentName,
    pub model: ModelId,
    pub account: AccountName,
    pub parent: Option<RunId>,
    pub depth: u8,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Line {
    Start(RunMeta),
    System { text: String },
    Message { message: Message },
    /// Blocks added to the message before it, which a resume does when the file ends in a user
    /// message (the new text joins it rather than following it).
    Extend { blocks: Vec<ContentBlock> },
    CompactBoundary { pre_tokens: u64, messages: Vec<Message> },
    /// A fallback moved the run to another account before the first token of a turn (F9). The
    /// `start` line names the account the run began on; a reader of the file sees both.
    Switch { account: AccountName, model: ModelId },
}

/// What a transcript holds: the conversation as the run would send it now, and the latest
/// `start` line (for a fork that is the fork's, for a resumed run the resume's).
#[derive(Debug)]
pub struct Loaded {
    pub conv: Conversation,
    pub meta: RunMeta,
}

/// Reads `path`. A last line that does not parse (the process died while writing it) is
/// ignored; any other bad line is an error with its number.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut conv = Conversation::default();
    let mut meta = None;
    for (i, raw) in lines.iter().enumerate() {
        let line: Line = match serde_json::from_str(raw) {
            Ok(l) => l,
            Err(_) if i + 1 == lines.len() => break,
            Err(e) => return Err(format!("{}:{}: {e}", path.display(), i + 1)),
        };
        match line {
            Line::Start(m) => meta = Some(m),
            Line::System { text } => conv.system = text,
            Line::Message { message } => conv.messages.push(message),
            Line::Extend { blocks } => {
                if let Some(last) = conv.messages.last_mut() {
                    last.blocks.extend(blocks);
                }
            }
            // A record of where the run went; a resume starts again from its own `start` line.
            Line::Switch { .. } => {}
            Line::CompactBoundary { messages, .. } => {
                conv.messages = messages;
                conv.compact_boundary = Some(0);
            }
        }
    }
    let meta = meta.ok_or_else(|| format!("{}: no start line", path.display()))?;
    Ok(Loaded { conv, meta })
}

/// The open file and how many of the conversation's messages it already holds.
pub struct Transcript {
    file: File,
    written: usize,
}

impl Transcript {
    /// Opens `path` for appending (mode `0600`), creating its directory.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        Ok(Self {
            file: opts.open(path)?,
            written: 0,
        })
    }

    /// Appends `line`; `redact` scrubs the serialised text first, since a transcript must not
    /// hold a credential any more than the graph may.
    pub fn write(&mut self, line: &Line, redact: impl Fn(&str) -> String) -> io::Result<()> {
        let mut text = redact(&serde_json::to_string(line).map_err(io::Error::other)?);
        text.push('\n');
        self.file.write_all(text.as_bytes())
    }

    /// The conversation's first `n` messages are in the file already.
    pub fn mark(&mut self, n: usize) {
        self.written = n;
    }

    /// Appends the messages `conv` gained since the last call.
    pub fn sync(&mut self, conv: &Conversation, redact: impl Fn(&str) -> String) -> io::Result<()> {
        for message in conv.messages.iter().skip(self.written) {
            self.write(&Line::Message { message: message.clone() }, &redact)?;
        }
        self.written = conv.messages.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::MessageRole;

    fn meta(kind: Kind) -> RunMeta {
        RunMeta {
            kind,
            agent: "lead".parse().unwrap(),
            model: ModelId("m".into()),
            account: AccountName("a".into()),
            parent: None,
            depth: 0,
        }
    }

    fn id(s: &str) -> impl Fn(&str) -> String + '_ {
        move |t| t.replace(s, "[redacted]")
    }

    #[test]
    fn a_boundary_replaces_the_live_messages_when_loading_and_the_file_keeps_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r/transcript.jsonl");
        let mut t = Transcript::open(&path).unwrap();
        t.write(&Line::Start(meta(Kind::Run)), |s| s.to_owned()).unwrap();
        t.write(&Line::System { text: "sys".into() }, |s| s.to_owned()).unwrap();
        let mut conv = Conversation {
            system: "sys".into(),
            ..Default::default()
        };
        conv.messages.push(Message::text(MessageRole::User, "one"));
        conv.messages.push(Message::text(MessageRole::Assistant, "two"));
        t.sync(&conv, |s| s.to_owned()).unwrap();
        t.write(
            &Line::CompactBoundary {
                pre_tokens: 99,
                messages: vec![Message::text(MessageRole::User, "summary")],
            },
            |s| s.to_owned(),
        )
        .unwrap();
        t.mark(1);
        conv.messages = vec![Message::text(MessageRole::User, "summary")];
        conv.messages.push(Message::text(MessageRole::Assistant, "three"));
        t.sync(&conv, |s| s.to_owned()).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.conv.system, "sys");
        let texts: Vec<_> = loaded.conv.messages.iter().map(Message::plain_text).collect();
        assert_eq!(texts, ["summary", "three"]);
        assert_eq!(loaded.conv.compact_boundary, Some(0));
        // The file itself still has the first two messages.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"one\"") && raw.contains("\"two\""));
        assert_eq!(raw.matches("compact_boundary").count(), 1);
    }

    #[test]
    fn a_torn_last_line_is_ignored_and_a_torn_middle_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut t = Transcript::open(&path).unwrap();
        t.write(&Line::Start(meta(Kind::Fork)), |s| s.to_owned()).unwrap();
        drop(t);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"mess");
        std::fs::write(&path, &text).unwrap();
        assert_eq!(load(&path).unwrap().meta.kind, Kind::Fork);
        text.push_str("\n{\"type\":\"system\",\"text\":\"x\"}\n");
        std::fs::write(&path, text).unwrap();
        assert!(load(&path).unwrap_err().contains(":2:"));
    }

    #[test]
    fn the_redactor_runs_over_every_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut t = Transcript::open(&path).unwrap();
        t.write(&Line::Start(meta(Kind::Run)), |s| s.to_owned()).unwrap();
        let mut conv = Conversation::default();
        conv.messages.push(Message::text(MessageRole::User, "key is sk-secret-1"));
        t.sync(&conv, id("sk-secret-1")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-secret-1") && raw.contains("[redacted]"));
    }
}
