//! The session list.

use std::path::Path;

/// One live session as `claude agents --json` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    /// The background job id the CLI gives a background session.
    pub job_id: Option<String>,
    pub name: String,
    pub cwd: String,
    /// `interactive` or `background`, as the CLI says.
    pub kind: String,
    pub started_ms: Option<i64>,
    /// `state` (background) or `status` (interactive), as the CLI says, or `unknown`.
    pub state: String,
    pub pid: Option<u32>,
}

/// Parses the output of `claude agents --json`.
pub fn parse_agents(_json: &[u8]) -> Result<Vec<Session>, String> {
    Err("not implemented".into())
}

/// Fills fields the command lacked from `<config>/sessions/*.json`; never adds a session.
pub fn merge_local(_list: &mut [Session], _sessions_dir: &Path) {}

/// The list as text: what each session is and what aibus can do with it.
pub fn render_list(_list: &[Session], _tapped: &[String], _now_ms: i64) -> String {
    String::new()
}

/// Runs `claude agents --json` (read-only) and returns its stdout.
pub fn run_agents_json() -> Result<Vec<u8>, String> {
    Err("not implemented".into())
}

/// The one session `arg` names: a session id, a unique prefix of one, a job id or a unique name.
pub fn resolve<'a>(_list: &'a [Session], _arg: &str) -> Result<&'a Session, String> {
    Err("not implemented".into())
}
