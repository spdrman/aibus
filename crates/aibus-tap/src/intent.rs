//! Plan and task files, as intent: what the session wrote down, not proof of what it did.
//!
//! Tasks are `<config>/tasks/<session id>/*.json`; the plan is the newest `<config>/plans/*.md`
//! written after the session started (the CLI does not tie a plan file to a session, so that
//! match is `inferred` and says so). A plan the CLI never wrote down is not invented.

use std::path::Path;

use aibus_core::Redactor;
use aibus_trace::view::clean;
use serde_json::Value;

const MAX_FILE: u64 = 64 << 10;
const MAX_TASKS: usize = 50;
const MAX_PLAN_LINES: usize = 20;

/// Lines for the intent section, redacted and cleaned.
pub fn read(
    config: &Path,
    session_id: &str,
    started_ms: Option<i64>,
    redactor: &Redactor,
) -> Vec<String> {
    let shown = |s: &str, n: usize| clean(&redactor.redact(s), n);
    let mut out = Vec::new();
    if crate::tap::valid_id(session_id) {
        let dir = config.join("tasks").join(session_id);
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        for p in files.into_iter().take(MAX_TASKS) {
            if !crate::own_regular_file(&p, MAX_FILE) {
                continue;
            }
            let Some(v) = std::fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            else {
                continue;
            };
            let field = |k: &str| match &v[k] {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            // Only these three fields: whatever else a task file holds stays there.
            out.push(format!(
                "task {} [{}] {}",
                shown(&field("id").unwrap_or_else(|| "?".into()), 20),
                shown(&field("status").unwrap_or_else(|| "unknown".into()), 20),
                shown(&field("subject").unwrap_or_default(), 160),
            ));
        }
    }
    if let Some(start) = started_ms {
        let newest = std::fs::read_dir(config.join("plans"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "md") && crate::own_regular_file(p, MAX_FILE)
            })
            .filter_map(|p| {
                let ms = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as i64;
                (ms >= start).then_some((ms, p))
            })
            .max();
        if let Some((_, p)) = newest {
            if let Ok(text) = std::fs::read_to_string(&p) {
                out.push(format!(
                    "plan {} (inferred: the newest plan file written after the session started; the CLI does not tie a plan to a session)",
                    shown(&p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()), 80),
                ));
                for l in text.lines().take(MAX_PLAN_LINES) {
                    out.push(format!("  {}", shown(l, 160)));
                }
            }
        }
    }
    out
}
