//! The NDJSON event for a `Msg` (06 section 2): `type` is the `Msg` name, the other field names
//! are the spec's. Every string in an event passes through the redactor before it is printed.

use std::time::{SystemTime, UNIX_EPOCH};

use aibus_core::state::value::value_to_json;
use aibus_core::{Decision, Msg, Redactor};
use aibus_provider::StreamChunk;
use serde_json::{json, Value};

/// `YYYY-MM-DD`, UTC (days since 1970-01-01 on the proleptic Gregorian calendar).
pub fn date(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// RFC 3339 UTC with milliseconds, as 06 section 4 wants timestamps.
pub fn rfc3339(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let rem = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}.{:03}Z",
        date(at),
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60,
        since.subsec_millis()
    )
}

/// One event, redacted. Messages the spec's schema does not list carry their redacted summary
/// (ids, counts, lengths) under the same `type` and `t`.
pub fn event(msg: &Msg, at: SystemTime, redactor: &Redactor) -> Value {
    let mut fields = match msg {
        Msg::RunStarted {
            run,
            agent,
            model,
            account,
            ..
        } => json!({
            "run": run.to_string(),
            "agent": agent.as_str(),
            "model": model.0.as_str(),
            "account": account.0.as_str(),
        }),
        Msg::Stream { run, chunks } => {
            let text: String = chunks
                .iter()
                .filter_map(|c| match c {
                    StreamChunk::TextDelta(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            json!({ "run": run.to_string(), "text": text })
        }
        Msg::ToolCallRequested { run, call } => json!({
            "run": run.to_string(),
            "call": { "id": call.id.0.to_string(), "name": call.name.as_str(), "input": call.input },
        }),
        Msg::ToolCallFinished {
            run,
            call_id,
            result,
            duration,
        } => json!({
            "run": run.to_string(),
            "call_id": call_id.0.to_string(),
            "ok": !result.is_error,
            "bytes": result.content.len(),
            "ms": duration.as_millis() as u64,
        }),
        Msg::PermissionNeeded { request } => json!({
            "id": request.id.to_string(),
            "run": request.run.to_string(),
            "tool": request.tool.as_str(),
            "summary": request.summary,
        }),
        Msg::PermissionDecided { id, decision } => json!({
            "id": id.to_string(),
            "decision": match decision {
                Decision::Allow => "allowed",
                Decision::AllowSession => "allowed_session",
                Decision::Deny => "denied",
            },
        }),
        Msg::RunFinished { run, result } => json!({
            "run": run.to_string(),
            "status": result.status.as_value(),
            "turns": result.turns,
            "usage": {
                "input_tokens": result.usage.input_tokens,
                "output_tokens": result.usage.output_tokens,
                "cost_usd": result.usage.cost_usd,
            },
            "summary": result.summary,
            "summary_file": result.summary_file,
        }),
        Msg::RunFailed { run, error } => json!({
            "run": run.to_string(),
            "kind": error.kind.as_str(),
            "message": error.message,
        }),
        other => {
            let mut v = value_to_json(&other.summary());
            if let Value::Object(o) = &mut v {
                o.remove("msg");
            }
            v
        }
    };
    if let Value::Object(o) = &mut fields {
        o.insert("type".into(), msg.name().into());
        o.insert("t".into(), rfc3339(at).into());
    }
    redactor.redact_json(&mut fields);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_timestamps() {
        let t = UNIX_EPOCH + std::time::Duration::from_millis(1_794_181_685_012);
        assert_eq!(date(t), "2026-11-08");
        assert_eq!(rfc3339(t), "2026-11-08T23:48:05.012Z");
        assert_eq!(date(UNIX_EPOCH), "1970-01-01");
        // A leap day.
        assert_eq!(date(UNIX_EPOCH + std::time::Duration::from_secs(1_709_164_800)), "2024-02-29");
    }
}
