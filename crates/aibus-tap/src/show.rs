//! The two texts: the list and one tapped session. The CLI and the TUI both print these.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use aibus_core::Redactor;
use aibus_trace::view::{self, clean, Filter, Recorded};

use crate::agents::{self, Session};
use crate::{intent, tap, transcript, Env};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn live(env: &Env, lister: &dyn Fn() -> Result<Vec<u8>, String>) -> Result<Vec<Session>, String> {
    let mut list = agents::parse_agents(&lister()?)?;
    agents::merge_local(&mut list, &env.config.join("sessions"));
    Ok(list)
}

fn has_tap(env: &Env, id: &str) -> bool {
    tap::valid_id(id) && env.taps().join(id).join("events.jsonl").is_file()
}

pub fn list(
    env: &Env,
    lister: &dyn Fn() -> Result<Vec<u8>, String>,
    _redactor: &Redactor,
) -> Result<String, String> {
    let sessions = live(env, lister)?;
    let tapped: Vec<String> = sessions
        .iter()
        .filter(|s| has_tap(env, &s.session_id))
        .map(|s| s.session_id.clone())
        .collect();
    Ok(agents::render_list(&sessions, &tapped, now_ms()))
}

/// One session: header, what each source gives, the graph and timeline `aibus runs inspect`
/// prints, intent, transcript tail, and what control exists (none in phase 1).
pub fn tap(
    env: &Env,
    lister: &dyn Fn() -> Result<Vec<u8>, String>,
    _redactor: &Redactor,
    arg: &str,
) -> Result<String, String> {
    let sessions = live(env, lister)?;
    let gone;
    let s = match agents::resolve(&sessions, arg) {
        Ok(s) => s,
        // No longer listed, but a tap run of that exact id is on disk: show it as gone.
        Err(_) if has_tap(env, arg) => {
            gone = Session {
                session_id: arg.to_owned(),
                job_id: None,
                name: String::new(),
                cwd: String::new(),
                kind: "unknown".into(),
                started_ms: None,
                state: "gone".into(),
                pid: None,
                version: None,
            };
            &gone
        }
        Err(e) => return Err(e),
    };
    let redactor = Redactor::new();
    let dir = env.taps().join(&s.session_id);
    let rec = has_tap(env, &s.session_id)
        .then(|| Recorded::load(&dir).ok())
        .flatten();
    let short: String = s.session_id.chars().take(8).collect();

    let mut out = format!(
        "tap claude {} ({})\nclient: claude   version: {}   state: {}   kind: {}\ncwd: {}\n",
        clean(&s.name, 80),
        clean(&short, 8),
        s.version.as_deref().map_or_else(
            || "unavailable (the list does not carry it)".to_owned(),
            |v| clean(v, 40)
        ),
        clean(&s.state, 20),
        clean(&s.kind, 20),
        clean(&s.cwd, 160),
    );
    out.push_str("sources:\n");
    match &rec {
        Some(r) => out.push_str(&format!(
            "  hooks: {} events, observed (the session posted them; only what happened since the hooks were installed)\n",
            r.events.len()
        )),
        None => out.push_str(
            "  hooks: unavailable (no hook event received for this session; `aibus sessions hooks install --client claude`, then keep `aibus sessions tap <id> --follow` running)\n",
        ),
    }
    let tail = path_of(&dir)
        .map(|p| transcript::tail(&env.state, &env.config, &p, s.version.as_deref(), &redactor));
    let on = transcript::allowed(&env.state);
    match (&tail, on) {
        (_, false) => out.push_str(
            "  transcripts: off (opt in with `aibus sessions allow-transcripts claude`; the format is internal to Claude Code)\n",
        ),
        (None, true) => out.push_str(
            "  transcripts: on, no transcript path known yet (the first hook event brings it)\n",
        ),
        (Some(t), true) => match &t.status {
            transcript::Status::Tested => out.push_str("  transcripts: on, unstable-format, version tested\n"),
            transcript::Status::UntestedVersion => out.push_str(
                "  transcripts: on, unstable-format, version not tested: shown best effort\n",
            ),
            transcript::Status::Unreadable(why) => {
                out.push_str(&format!("  transcripts: on, refused: {}\n", clean(why, 120)));
            }
            transcript::Status::Off => out.push_str("  transcripts: off\n"),
        },
    }
    out.push_str("  plan and task files: intent, not proof of execution\n");
    out.push_str(
        "control: watch only (phase 1 has no control path: aibus sends nothing to the session, types nothing and answers no permission prompt)\n\n",
    );

    match &rec {
        Some(r) => {
            let name = dir
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            out.push_str(&view::graph(r, &name));
            out.push_str(&view::timeline(r, &Filter::default()));
        }
        None => out.push_str("graph and timeline: unavailable (no events yet)\n"),
    }

    let lines = intent::read(&env.config, &s.session_id, s.started_ms, &redactor);
    out.push_str("\nintent (what the session wrote down; not proof of execution):\n");
    if lines.is_empty() {
        out.push_str("  unavailable (no plan or task file found for this session)\n");
    }
    for l in &lines {
        out.push_str(&format!("  {l}\n"));
    }

    if let Some(t) = tail.filter(|_| on) {
        if !t.messages.is_empty() {
            out.push_str(
                "\nrecent messages (transcript, read-only, redacted; format is unstable):\n",
            );
            for m in &t.messages {
                out.push_str(&format!("  {m}\n"));
            }
        }
        if t.unknown_variants > 0 {
            out.push_str(&format!(
                "  trace_warning unknown_variant: {} transcript entries or blocks not understood\n",
                t.unknown_variants
            ));
        }
    }
    Ok(out)
}

fn path_of(dir: &Path) -> Option<std::path::PathBuf> {
    tap::transcript_path(dir)
}
