//! The session's two ends of the continuity loop (01 section 8): at `session_start` the lead is
//! shown the newest handoff of the topic and the ledger, the status bar is told what the ledger
//! says and the index is brought up to date; at `session_end` a handoff is written when the
//! ledger changed, learnings are extracted and the index is brought up to date again.

use std::fmt::Display;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aibus_core::config::Config;
use aibus_core::continuity::handoff::{self, Front, Handoff};
use aibus_core::continuity::recall::MemoryIndex;
use aibus_core::continuity::{learn, ledger, memory, Layout};
use aibus_core::persist::fs::content_hash;
use aibus_core::persist::index::Index;
use aibus_core::persist::session::read_runs;
use aibus_core::persist::time::date;
use aibus_core::{Command, MemoryScope, ModelCompactor, Msg, RunStatus};

use crate::startup::Ctx;

/// How long the extraction call may take before the session ends without it.
const EXTRACT_WITHIN: Duration = Duration::from_secs(60);

/// What the session keeps of the continuity loop between its two ends.
pub struct Continuity {
    pub layout: Layout,
    /// The ledger's hash at `session_start`, to tell at the end whether it changed.
    ledger_at_start: Option<String>,
    /// Asks the lead's model, for the learnings.
    extractor: Arc<ModelCompactor>,
    /// The handoff written at `session_end`, for the `SessionEnd` event.
    pub ended: Option<PathBuf>,
}

fn warn(what: &str, why: impl Display) {
    eprintln!("aibus: warning: {what}: {why}");
}

fn ledger_hash(layout: &Layout) -> Option<String> {
    fs::read(layout.ledger()).ok().map(|b| content_hash(&b))
}

/// The index is a cache, so a failure here is a warning.
pub fn reindex(layout: &Layout) {
    let synced = Index::open(&layout.index()).and_then(|mut i| i.sync(layout));
    if let Err(e) = synced {
        warn("indexing memory", e);
    }
}

impl Continuity {
    pub fn new(layout: Layout, extractor: Arc<ModelCompactor>) -> Self {
        let ledger_at_start = ledger_hash(&layout);
        Self {
            layout,
            ledger_at_start,
            extractor,
            ended: None,
        }
    }

    /// `session_start`: tells the graph about the ledger and the newest handoff, re-indexes, and
    /// returns what goes in front of the lead's task: the handoff of the topic (the `[handoff]`
    /// line and the whole file) and the ledger. Workers are not shown either (Q24).
    pub fn start(&self, app: &mut aibus_core::App<aibus_graph::mini::MiniGraph>) -> Vec<String> {
        let l = &self.layout;
        let mut shown = Vec::new();
        if let Some(path) = handoff::latest(&l.handoffs(), None) {
            let _ = app.step(Msg::HandoffWritten { path });
        }
        if let Some(path) = handoff::latest(&l.handoffs(), Some(&l.topic)) {
            match handoff::context(&path) {
                Ok(text) => shown.push(text),
                Err(e) => warn("reading the newest handoff", e),
            }
        }
        match ledger::read(l) {
            Ok(Some(ledger)) => {
                tell_status_bar(app, &ledger);
                shown.push(format!("[ledger] {}\n\n{}", l.topic, ledger.render().trim_end()));
            }
            Ok(None) => {}
            Err(e) => warn("reading the ledger", e),
        }
        reindex(l);
        shown
    }
}

/// `model:ledger`, which the status bar shows: the goal and the first thing in progress.
pub fn tell_status_bar(app: &mut aibus_core::App<aibus_graph::mini::MiniGraph>, l: &ledger::Ledger) {
    for (section, text) in [("goal", l.goal()), ("in_progress", l.in_progress().unwrap_or(""))] {
        let _ = app.step(Msg::Command(Command::LedgerUpdate {
            section: section.into(),
            text: text.into(),
        }));
    }
}

/// `session_end`. Everything here is best effort: a failure is a warning and the session still
/// ends. The handoff's path is kept in `ctx.continuity` for the `SessionEnd` event.
pub fn end(ctx: &mut Ctx) {
    let Some(mut c) = ctx.continuity.take() else {
        return;
    };
    let cfg: Config = match ctx.loaded.as_ref() {
        Some(l) => l.config.clone(),
        None => return,
    };
    let Some(session) = ctx.session.as_ref() else {
        return;
    };
    let lead = ctx.finished.as_ref().map(|r| r.run_id);
    let conv = lead.and_then(|run| {
        let file = session.dir.join("runs").join(run.to_string()).join("conversation.jsonl");
        aibus_core::run::transcript::load(&file).ok().map(|l| l.conv)
    });

    if cfg.continuity.handoff_on_end {
        let now_hash = ledger_hash(&c.layout);
        if now_hash.is_some() && now_hash != c.ledger_at_start {
            match write_handoff(ctx, &c.layout, conv.as_ref()) {
                Ok(path) => {
                    if let Some(app) = ctx.app.as_mut() {
                        let _ = app.step(Msg::HandoffWritten { path: path.clone() });
                    }
                    c.ended = Some(path);
                }
                Err(e) => warn("writing the handoff", e),
            }
        }
    }

    let completed = read_runs(&session.dir).iter().any(|r| r.status == RunStatus::Completed.as_value());
    let wanted = cfg.memory.extract_on_end && cfg.continuity.extract_learnings;
    if let (true, true, false, Some(conv), Some(rt)) = (wanted, completed, ctx.interrupted, &conv, ctx.rt.as_ref()) {
        let cancel = tokio_util::sync::CancellationToken::new();
        let asked = rt.block_on(async {
            tokio::time::timeout(EXTRACT_WITHIN, c.extractor.extract(conv, &cancel)).await
        });
        match asked {
            Ok(Ok(found)) if !found.is_empty() => {
                let now = SystemTime::now();
                let file = memory::scope_dir(MemoryScope::User, "lead", &c.layout.project, &c.layout.data)
                    .join("LEARNINGS.md");
                let heading = format!("{} session {}", date(now), session.id);
                if let Err(e) = learn::append(&file, &found, &heading, &ctx.redactor) {
                    warn("writing LEARNINGS.md", e);
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn("extracting learnings", e),
            Err(_) => warn("extracting learnings", "timed out"),
        }
    }
    reindex(&c.layout);
    ctx.continuity = Some(c);
}

/// The handoff for a session whose ledger changed: the ledger's goal and first open item, the
/// run's status and the session's outcome (`--outcome`, `AIBUS_OUTCOME`).
fn write_handoff(
    ctx: &Ctx,
    layout: &Layout,
    conv: Option<&aibus_core::Conversation>,
) -> Result<PathBuf, String> {
    let ledger = ledger::read(layout).map_err(|e| e.to_string())?;
    let mut body = serde_json::Value::Object(handoff::summarise(ledger.as_ref(), conv));
    ctx.redactor.redact_json(&mut body);
    let now = SystemTime::now();
    let status = ctx.finished.as_ref().map_or("partial", |r| handoff::status_of(r.status));
    let front = Front {
        session: &layout.topic,
        date: &date(now),
        status,
        outcome: ctx.outcome.as_deref(),
    };
    let h = Handoff::build(&front, body.as_object().ok_or("no handoff body")?)?;
    handoff::write(layout, &h, false, now).map_err(|e| e.to_string())
}
