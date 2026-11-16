//! The hook entries in the user's settings.
//!
//! Six `command` hooks, one per event, each running a small script that aibus keeps in its own
//! state directory (0700). The script reads the receiver's url and the token from 0600 files next
//! to it, posts the event to the loopback receiver with a one second limit, prints nothing and
//! always exits 0: whatever the receiver answers (or whoever holds the port) is thrown away, so
//! no reply can decide a permission prompt or add text to the session, and a receiver that is not
//! there, or never answers, costs the session one second at most. The token is never in settings.
//! Nothing is written to the settings without `confirm` having seen the exact diff.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::value::RawValue;
use serde_json::{json, Value};

type Raw = Box<RawValue>;

/// The loopback endpoint and the per-install token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
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

/// The path the receiver serves, and how an entry of the first version of this feature (an `http`
/// hook) is still recognised so an uninstall takes it out.
pub const HOOK_PATH: &str = "/aibus-tap/hook";

/// The script a command hook runs.
pub const SCRIPT_NAME: &str = "aibus-tap-hook.sh";

const SCRIPT: &str = r#"#!/bin/sh
# aibus tap hook (#355). Posts the hook event on stdin to the loopback receiver of
# `aibus sessions tap --follow`. It never prints, never reads the reply and always exits 0, so it
# cannot allow, deny or add anything to the session, and it gives up after one second.
d=$(dirname "$0")
[ -r "$d/hook.url" ] && [ -r "$d/hook.header" ] || exit 0
command -v curl >/dev/null 2>&1 || exit 0
curl -s -m 1 --connect-timeout 1 -o /dev/null -H @"$d/hook.header" \
  -H 'Content-Type: application/json' --data-binary @- "$(cat "$d/hook.url")" >/dev/null 2>&1
exit 0
"#;

/// What a change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    Declined,
    Unchanged,
}

impl Endpoint {
    pub fn url(&self) -> String {
        format!("http://localhost{HOOK_PATH}")
    }

    fn save(&self, state: &Path) -> Result<(), String> {
        let dir = state.join("tap");
        write_private(
            &dir,
            &dir.join("endpoint.json"),
            json!({"token": self.token}).to_string().as_bytes(),
            0o600,
        )?;
        // The script's header file follows the token once the script has been installed.
        if dir.join("hook.header").exists() {
            write_private(
                &dir,
                &dir.join("hook.header"),
                format!("Authorization: Bearer {}\n", self.token).as_bytes(),
                0o600,
            )?;
        }
        Ok(())
    }

    /// The endpoint saved by an earlier install under `<state>/tap/endpoint.json`, or a new one
    /// (a random token), saved with mode 0600.
    pub fn load_or_create(state: &Path) -> Result<Self, String> {
        let file = state.join("tap").join("endpoint.json");
        if let Some(v) = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            if let Some(token) = v["token"].as_str().filter(|t| t.len() >= 32) {
                return Ok(Self {
                    token: token.to_owned(),
                });
            }
        }
        let ep = Self { token: new_token() };
        ep.save(state)?;
        Ok(ep)
    }

    /// A new token, saved; the old one stops working.
    pub fn rotate(&self, state: &Path) -> Result<Self, String> {
        let next = Self { token: new_token() };
        next.save(state)?;
        Ok(next)
    }
}

fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Creates `dir` (0700) and writes `file` with `bytes` and `mode`, whatever the umask was.
pub(crate) fn write_private(
    dir: &Path,
    file: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|e| e.to_string())
}

/// Writes the hook script (0700) and the files it reads (0600) under `<state>/tap/`, and returns
/// the script's path.
pub fn install_files(state: &Path, ep: &Endpoint) -> Result<PathBuf, String> {
    let dir = state.join("tap");
    write_private(
        &dir,
        &dir.join("hook.url"),
        format!("{}\n", ep.url()).as_bytes(),
        0o600,
    )?;
    write_private(
        &dir,
        &dir.join("hook.header"),
        format!("Authorization: Bearer {}\n", ep.token).as_bytes(),
        0o600,
    )?;
    let sock = socket_path(state, Path::new("/tmp"))?;
    write_private(
        &dir,
        &dir.join("hook.sock"),
        format!("{}\n", sock.display()).as_bytes(),
        0o600,
    )?;
    let script = dir.join(SCRIPT_NAME);
    write_private(&dir, &script, SCRIPT.as_bytes(), 0o700)?;
    Ok(std::fs::canonicalize(&script).unwrap_or(script))
}

/// Where the receiver's socket is: `<state>/tap/hook.sock`, or, when that path is too long for
/// `sun_path` (104 bytes on macOS), `<short>/aibus-<uid>/tap-<hash>.sock`. Never TCP.
pub fn socket_path(state: &Path, _short: &Path) -> Result<PathBuf, String> {
    Ok(state.join("tap").join("hook.sock"))
}

/// Binds the receiver's socket: the directory must be this user's and closed to others, a stale
/// socket of ours is replaced, anything else in the way is refused, and the socket is 0600.
pub fn bind_socket(_path: &Path) -> Result<std::os::unix::net::UnixListener, String> {
    Err("not implemented".into())
}

/// The settings `command` string for a script path, quoted for a shell when it needs it.
pub fn command_for(script: &Path) -> String {
    let p = script.display().to_string();
    if p.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    {
        p
    } else {
        format!("'{}'", p.replace('\'', "'\\''"))
    }
}

fn is_ours(hook: &Value) -> bool {
    let command = hook["type"] == "command"
        && hook["command"]
            .as_str()
            .is_some_and(|c| c.contains(SCRIPT_NAME));
    let http = hook["type"] == "http"
        && hook["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("http://127.0.0.1:") && u.ends_with(HOOK_PATH));
    command || http
}

enum Group {
    NoneOurs,
    AllOurs,
    Mixed(Value),
}

fn classify(g: &Value) -> Group {
    let Some(hs) = g["hooks"].as_array() else {
        return Group::NoneOurs;
    };
    let ours = hs.iter().filter(|h| is_ours(h)).count();
    if ours == 0 {
        Group::NoneOurs
    } else if ours == hs.len() {
        Group::AllOurs
    } else {
        let mut v = g.clone();
        if let Some(a) = v["hooks"].as_array_mut() {
            a.retain(|h| !is_ours(h));
        }
        Group::Mixed(v)
    }
}

/// `s` with every line after the first indented by `n` spaces.
fn pad(s: &str, n: usize) -> String {
    let p = " ".repeat(n);
    s.lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.to_owned()
            } else {
                format!("{p}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn raw(s: String) -> Result<Raw, String> {
    RawValue::from_string(s).map_err(|e| e.to_string())
}

fn object(items: &[(String, String)], depth: usize) -> String {
    if items.is_empty() {
        return "{}".into();
    }
    let ind = " ".repeat(2 * (depth + 1));
    let body: Vec<String> = items
        .iter()
        .map(|(k, v)| format!("{ind}{}: {v}", serde_json::to_string(k).unwrap_or_default()))
        .collect();
    format!("{{\n{}\n{}}}", body.join(",\n"), " ".repeat(2 * depth))
}

fn array(items: &[String], depth: usize) -> String {
    let ind = " ".repeat(2 * (depth + 1));
    let body: Vec<String> = items.iter().map(|v| format!("{ind}{v}")).collect();
    format!("[\n{}\n{}]", body.join(",\n"), " ".repeat(2 * depth))
}

/// The new text of a settings file with aibus's hooks put in (`Some(command)`) or taken out
/// (`None`), or `None` when nothing changes. Everything aibus does not touch keeps its own text
/// and its place: other keys, other events, a user's empty lists. Refuses what it cannot edit.
/// The text it builds is two-space JSON, so a file written another way shows as reformatted.
pub fn edit(text: &str, command: Option<&str>) -> Result<Option<String>, String> {
    let src = if text.trim().is_empty() { "{}" } else { text };
    let mut top: IndexMap<String, Raw> = serde_json::from_str(src)
        .map_err(|e| format!("not a JSON object ({e}); aibus will not replace it"))?;
    let mut events: IndexMap<String, Raw> = match top.get("hooks") {
        Some(r) => serde_json::from_str(r.get())
            .map_err(|_| "\"hooks\" is not an object; aibus will not edit it".to_owned())?,
        None => IndexMap::new(),
    };
    let had_events = !events.is_empty();
    for ev in EVENTS {
        let groups: Vec<Raw> = match events.get(*ev) {
            Some(r) => serde_json::from_str(r.get())
                .map_err(|_| format!("hooks.{ev} is not a list; aibus will not edit it"))?,
            None => Vec::new(),
        };
        let was_nonempty = !groups.is_empty();
        let mut items: Vec<String> = Vec::new();
        for g in groups {
            match classify(&serde_json::from_str(g.get()).unwrap_or(Value::Null)) {
                Group::NoneOurs => items.push(g.get().to_owned()),
                Group::AllOurs => {}
                Group::Mixed(v) => {
                    items.push(pad(
                        &serde_json::to_string_pretty(&v).unwrap_or_default(),
                        6,
                    ));
                }
            }
        }
        if let Some(c) = command {
            let ours = json!({"hooks": [{"command": c, "timeout": 5, "type": "command"}]});
            items.push(pad(
                &serde_json::to_string_pretty(&ours).unwrap_or_default(),
                6,
            ));
        }
        if items.is_empty() {
            // Only a list that held something and now holds nothing goes; a user's own empty
            // list stays.
            if was_nonempty {
                events.shift_remove(*ev);
            }
        } else {
            events.insert((*ev).to_owned(), raw(array(&items, 2))?);
        }
    }
    if events.is_empty() && had_events {
        top.shift_remove("hooks");
    } else if !events.is_empty() || top.contains_key("hooks") {
        let items: Vec<(String, String)> = events
            .iter()
            .map(|(k, v)| (k.clone(), v.get().to_owned()))
            .collect();
        top.insert("hooks".into(), raw(object(&items, 1))?);
    }
    let items: Vec<(String, String)> = top
        .iter()
        .map(|(k, v)| (k.clone(), v.get().to_owned()))
        .collect();
    let new = format!("{}\n", object(&items, 0));
    let same = serde_json::from_str::<Value>(src).ok() == serde_json::from_str::<Value>(&new).ok();
    Ok((!same).then_some(new))
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

/// A line diff of two texts.
pub fn diff(before: &str, after: &str) -> String {
    text_diff(before, after)
}

/// Installs (`Some(command)`) or removes (`None`) the entries in `settings_path`, after `confirm`
/// has seen the exact diff against the file as it is now. Declined or unchanged means nothing was
/// written. Before every write the old text is saved next to the file as `<name>.aibus-before`
/// with the same permissions as the settings file.
pub fn apply(
    settings_path: &Path,
    command: Option<&str>,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<Outcome, String> {
    use std::os::unix::fs::PermissionsExt;
    // A link to a dotfiles repository is written through, not replaced.
    let target = std::fs::canonicalize(settings_path).unwrap_or_else(|_| settings_path.to_owned());
    let (old_text, mode) = match std::fs::read_to_string(&target) {
        Ok(t) => {
            let mode = std::fs::metadata(&target).map_or(0o600, |m| m.permissions().mode() & 0o777);
            (t, mode)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), 0o600),
        Err(e) => return Err(format!("{}: {e}", target.display())),
    };
    let Some(new_text) =
        edit(&old_text, command).map_err(|e| format!("{}: {e}", target.display()))?
    else {
        return Ok(Outcome::Unchanged);
    };
    if !confirm(&text_diff(&old_text, &new_text)) {
        return Ok(Outcome::Declined);
    }
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if !old_text.is_empty() {
        let backup = dir.join(format!("{name}.aibus-before"));
        write_private(dir, &backup, old_text.as_bytes(), mode)?;
    }
    let tmp = dir.join(format!("{name}.aibus-tmp"));
    write_private(dir, &tmp, new_text.as_bytes(), mode)?;
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
