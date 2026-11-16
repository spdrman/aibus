//! The hook entries in the user's settings.
//!
//! One `http` hook per event, posting to a loopback endpoint with a per-install token in an
//! `Authorization` header. Claude Code treats a connection failure or a non-2xx answer as a
//! non-blocking error unless `onFailure` says `block`, which these entries never set, so a
//! session whose aibus is not listening is not slowed or stopped: the hook fails open.
//! Nothing is written without `confirm` having seen the exact diff.

use std::path::Path;

use serde_json::{json, Value};

/// The loopback endpoint and the per-install token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// The events the normaliser maps. No `PermissionRequest` (aibus never decides a prompt) and no
/// `Stop` (a Stop hook can hold a turn open, and a tap has no reason to be near that).
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
];

/// The path every entry posts to, and how aibus recognises its own entries again.
pub const HOOK_PATH: &str = "/aibus-tap/hook";

/// What a change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    Declined,
    Unchanged,
}

impl Endpoint {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{HOOK_PATH}", self.port)
    }

    /// The endpoint saved by an earlier install under `<state>/tap/endpoint.json`, or a new one
    /// (a free loopback port and a random token), saved with mode 0600.
    pub fn load_or_create(state: &Path) -> Result<Self, String> {
        let dir = state.join("tap");
        let file = dir.join("endpoint.json");
        if let Some(v) = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            if let (Some(port), Some(token)) = (
                v["port"].as_u64().and_then(|p| u16::try_from(p).ok()),
                v["token"].as_str().filter(|t| t.len() >= 32),
            ) {
                return Ok(Self {
                    port,
                    token: token.to_owned(),
                });
            }
        }
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|l| l.local_addr())
            .map_err(|e| format!("no free loopback port: {e}"))?
            .port();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let ep = Self { port, token };
        write_private(
            &dir,
            &file,
            json!({"port": ep.port, "token": ep.token})
                .to_string()
                .as_bytes(),
        )?;
        Ok(ep)
    }
}

/// Creates `dir` (0700) and writes `file` (0600) with `bytes`.
pub(crate) fn write_private(dir: &Path, file: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())
}

fn is_ours(hook: &Value) -> bool {
    hook["type"] == "http"
        && hook["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("http://127.0.0.1:") && u.ends_with(HOOK_PATH))
}

pub fn without_hooks(settings: &Value) -> Value {
    let mut out = settings.clone();
    let Some(hooks) = out.get_mut("hooks").and_then(Value::as_object_mut) else {
        return out;
    };
    for groups in hooks.values_mut() {
        let Some(list) = groups.as_array_mut() else {
            continue;
        };
        for g in list.iter_mut() {
            if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                hs.retain(|h| !is_ours(h));
            }
        }
        list.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        out.as_object_mut().map(|o| o.remove("hooks"));
    }
    out
}

pub fn with_hooks(settings: &Value, ep: &Endpoint) -> Value {
    let mut out = without_hooks(settings);
    if !out.is_object() {
        return out;
    }
    let hooks = out
        .as_object_mut()
        .map(|o| o.entry("hooks").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut);
    let Some(hooks) = hooks else {
        return out;
    };
    for e in EVENTS {
        let entry = json!({"hooks": [{
            "headers": {"Authorization": format!("Bearer {}", ep.token)},
            "timeout": 5,
            "type": "http",
            "url": ep.url(),
        }]});
        match hooks.entry(*e).or_insert_with(|| json!([])).as_array_mut() {
            Some(list) => list.push(entry),
            None => return settings.clone(),
        }
    }
    out
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s
}

/// A line diff of two texts: changed lines with two lines of context, `+`, `-` or two spaces in
/// front, and `...` where lines are left out. Empty when the texts are the same.
fn text_diff(before: &str, after: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut rows: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            rows.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            rows.push(('+', b[j]));
            j += 1;
        } else {
            rows.push(('-', a[i]));
            i += 1;
        }
    }
    if rows.iter().all(|r| r.0 == ' ') {
        return String::new();
    }
    let keep: Vec<bool> = (0..rows.len())
        .map(|n| (n.saturating_sub(2)..=(n + 2).min(rows.len() - 1)).any(|m| rows[m].0 != ' '))
        .collect();
    let mut out = String::new();
    let mut skipped = false;
    for (n, (c, l)) in rows.iter().enumerate() {
        if keep[n] {
            out.push_str(&format!("{c} {l}\n"));
            skipped = false;
        } else if !skipped {
            out.push_str("...\n");
            skipped = true;
        }
    }
    out
}

/// A line diff of the two documents, pretty printed.
pub fn diff(before: &Value, after: &Value) -> String {
    text_diff(&pretty(before), &pretty(after))
}

/// Installs (`Some`) or removes (`None`) the entries in `settings_path`, after `confirm` has seen
/// the exact diff against the file as it is now. Declined or unchanged means nothing was written.
/// The file is rewritten as sorted, two-space JSON (that is part of the diff); the first time,
/// the old file is kept next to it as `<name>.aibus-before`.
pub fn apply(
    settings_path: &Path,
    ep: Option<&Endpoint>,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<Outcome, String> {
    // A link to a dotfiles repository is written through, not replaced.
    let target = std::fs::canonicalize(settings_path).unwrap_or_else(|_| settings_path.to_owned());
    let (old_text, before) = match std::fs::read_to_string(&target) {
        Ok(t) => {
            let v: Value = serde_json::from_str(&t).map_err(|e| {
                format!(
                    "{} is not JSON ({e}); aibus will not replace it",
                    target.display()
                )
            })?;
            if !v.is_object() {
                return Err(format!("{} is not a JSON object", target.display()));
            }
            (t, v)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), json!({})),
        Err(e) => return Err(format!("{}: {e}", target.display())),
    };
    let after = match ep {
        Some(ep) => with_hooks(&before, ep),
        None => without_hooks(&before),
    };
    if after == before {
        return Ok(Outcome::Unchanged);
    }
    let new_text = pretty(&after);
    if !confirm(&text_diff(&old_text, &new_text)) {
        return Ok(Outcome::Declined);
    }
    if !old_text.is_empty() {
        let mut backup = target.clone().into_os_string();
        backup.push(".aibus-before");
        let backup = std::path::PathBuf::from(backup);
        if !backup.exists() {
            std::fs::write(&backup, &old_text).map_err(|e| format!("{}: {e}", backup.display()))?;
        }
    }
    let mut tmp = target.clone().into_os_string();
    tmp.push(".aibus-tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, &new_text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Ok(m) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(Outcome::Changed)
}

/// Whether `header` (the `Authorization` value) carries the token. Compared in constant time, as
/// digests, so the length does not leak either.
pub fn accepts(ep: &Endpoint, header: Option<&str>) -> bool {
    use subtle::ConstantTimeEq;
    let want = aibus_trace::sha256_hex(format!("Bearer {}", ep.token).as_bytes());
    let got = aibus_trace::sha256_hex(header.unwrap_or("\0").as_bytes());
    want.as_bytes().ct_eq(got.as_bytes()).into()
}
