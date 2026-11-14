//! The lead's continuity tools (01 section 9): `ledger_update`, `create_handoff` and `recall`.
//! Workers are not given them, so they never see the ledger (Q24).

use std::io;
use std::time::SystemTime;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::readonly::input;
use crate::agent::PermissionMode;
use crate::continuity::handoff::{self, Front, Handoff};
use crate::continuity::ledger;
use crate::continuity::recall::MemoryIndex;
use crate::continuity::Layout;
use crate::msg::{Command, Msg};
use crate::persist::index::Index;
use crate::persist::time::date;
use crate::redact::Redactor;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

fn json_out(v: Value) -> ToolOutput {
    ToolOutput {
        content: v.to_string(),
        ..Default::default()
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T, ToolError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) if e.kind() == io::ErrorKind::InvalidInput => Err(ToolError::Invalid(e.to_string())),
        Ok(Err(e)) => Err(ToolError::Io(e)),
        Err(e) => Err(ToolError::Io(io::Error::other(e))),
    }
}

/// Changes one section of the topic's ledger, and tells the graph what the status bar shows.
pub struct LedgerUpdate {
    layout: Layout,
    redactor: Redactor,
}

impl LedgerUpdate {
    pub fn new(layout: Layout, redactor: Redactor) -> Self {
        Self { layout, redactor }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerInput {
    section: String,
    text: String,
    #[serde(default)]
    append: bool,
}

#[async_trait::async_trait]
impl Tool for LedgerUpdate {
    fn name(&self) -> &'static str {
        "ledger_update"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ledger_update".into(),
            description: "Keep the continuity ledger of this project up to date. `section` is Goal, Completed, \
                In Progress, Blockers or Decisions. `text` replaces the section, or is added to it with \
                `append`; every line of a list section becomes an item. The first In Progress item and the \
                Goal are what the status bar shows."
                .into(),
            schema: json!({"type":"object","required":["section","text"],"additionalProperties":false,"properties":{
                "section":{"type":"string","enum":ledger::SECTIONS},
                "text":{"type":"string"},
                "append":{"type":"boolean"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: LedgerInput = input(v)?;
        let text = self.redactor.redact(&i.text).into_owned();
        let (layout, today) = (self.layout.clone(), date(SystemTime::now()));
        let (section, append) = (i.section, i.append);
        let l = blocking(move || ledger::update(&layout, &section, &text, append, &today)).await?;
        for (to, text) in [("goal", l.goal()), ("in_progress", l.in_progress().unwrap_or(""))] {
            let _ = cx
                .tx
                .send(Msg::Command(Command::LedgerUpdate {
                    section: to.into(),
                    text: text.into(),
                }))
                .await;
        }
        Ok(json_out(json!({ "ok": true, "path": self.layout.ledger(), "updated": l.updated })))
    }
}

/// Writes a handoff (01 section 8.2) for the topic.
pub struct CreateHandoff {
    layout: Layout,
    redactor: Redactor,
    /// `--outcome` or `AIBUS_OUTCOME`, canonical: goes into the front matter.
    outcome: Option<String>,
}

impl CreateHandoff {
    pub fn new(layout: Layout, redactor: Redactor, outcome: Option<String>) -> Self {
        Self {
            layout,
            redactor,
            outcome,
        }
    }
}

#[async_trait::async_trait]
impl Tool for CreateHandoff {
    fn name(&self) -> &'static str {
        "create_handoff"
    }

    fn spec(&self) -> ToolSpec {
        let list = json!({"type":"array"});
        ToolSpec {
            name: "create_handoff".into(),
            description: "Write a handoff for the next session: where the work stands and what to do first. \
                `goal` and `now` are required; the rest follows the Continuous-Claude handoff schema."
                .into(),
            schema: json!({"type":"object","required":["goal","now"],"additionalProperties":false,"properties":{
                "goal":{"type":"string","minLength":1},
                "now":{"type":"string","minLength":1},
                "status":{"type":"string","enum":handoff::STATUSES},
                "outcome":{"type":"string","enum":handoff::OUTCOMES},
                "test":{"type":"string"},
                "done_this_session":list, "blockers":list, "questions":list, "decisions":list,
                "findings":list, "worked":list, "failed":list, "next":list,
                "files":{"type":"object"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let mut body: Map<String, Value> = input(v)?;
        let mut front_text = |k: &str| match body.remove(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(ToolError::Invalid(format!("`{k}` must be a string"))),
        };
        let status = front_text("status")?.unwrap_or_else(|| "partial".into());
        let outcome = front_text("outcome")?.or_else(|| self.outcome.clone());
        let mut body = Value::Object(body);
        self.redactor.redact_json(&mut body);
        let now = SystemTime::now();
        let today = date(now);
        let front = Front {
            session: &self.layout.topic,
            date: &today,
            status: &status,
            outcome: outcome.as_deref(),
        };
        let h = Handoff::build(&front, body.as_object().unwrap_or(&Map::new())).map_err(ToolError::Invalid)?;
        let layout = self.layout.clone();
        let path = blocking(move || handoff::write(&layout, &h, false, now)).await?;
        let _ = cx.tx.send(Msg::HandoffWritten { path: path.clone() }).await;
        Ok(json_out(json!({ "ok": true, "path": path })))
    }
}

/// `recall(query, k)`: the best matches among the memory files, learnings, ledgers and
/// handoffs in scope.
pub struct Recall {
    layout: Layout,
    /// `memory.recall_k`, for a call that gives no `k`.
    k: usize,
}

impl Recall {
    pub fn new(layout: Layout, k: usize) -> Self {
        Self { layout, k }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallInput {
    query: String,
    k: Option<usize>,
}

#[async_trait::async_trait]
impl Tool for Recall {
    fn name(&self) -> &'static str {
        "recall"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "recall".into(),
            description: "Search the memory files, learnings, ledgers and handoffs for a query. Returns \
                `[{path, score, snippet}]`, best first; read a file with `read` or `memory_read`."
                .into(),
            schema: json!({"type":"object","required":["query"],"additionalProperties":false,"properties":{
                "query":{"type":"string","minLength":1},
                "k":{"type":"integer","minimum":1,"maximum":50}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: RecallInput = input(v)?;
        let k = i.k.unwrap_or(self.k).clamp(1, 50);
        let layout = self.layout.clone();
        let hits = blocking(move || {
            // Files changed since the last sync (a `memory_write` a moment ago) are found too.
            let mut index = Index::open(&layout.index())?;
            index.sync(&layout)?;
            index.recall(&layout, &i.query, k)
        })
        .await?;
        Ok(json_out(json!(hits)))
    }
}
