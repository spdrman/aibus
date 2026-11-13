//! A server over a throwaway home, called directly (no transport).

#![allow(dead_code)]

use aibus_core::Redactor;
use aibus_mcp::{Caps, Server};
use aibus_testkit::{xdg_temp, Xdg};

pub fn caps() -> Caps {
    Caps::default()
}

/// The server, the home it lives in, and the project directory.
pub fn server(caps: Caps, redactor: Redactor) -> (Server, Xdg) {
    let home = xdg_temp();
    let project = home.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let server = Server::new(
        caps,
        project,
        &home.paths,
        Some(home.path().join("home")),
        redactor,
    )
    .expect("a server");
    (server, home)
}

/// The lines of the one audit file the server wrote.
pub fn audit_lines(home: &Xdg) -> Vec<serde_json::Value> {
    let dir = home.paths.state.join("logs/mcp");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "one file per process in {}", dir.display());
    std::fs::read_to_string(files[0].path())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A stand-in for `aibus run --workflow <name> --json -- <input>` that plays one scripted
/// chain, chosen by the input (the last argument): `ok`, `slow`, `ask`, `costly`, `step`, `echo:<text>` (puts its input in the output) or `loud` (a long log).
/// It writes its arguments (the input of an `echo:` step left out, since a test plants a key there) and its environment to `<dir>/argv.txt` and `<dir>/env.txt`, and an
/// empty `commits.jsonl.zst` to `<dir>/session/`, so a test can tell whether (and how) it was
/// started. Returns the path of the script.
#[cfg(unix)]
pub fn fake_aibus(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
for last; do :; done
# The input is the caller's text and a test may plant a key in it on purpose (`echo:<key>`), so the
# record keeps the shape of the line but not that text, or the secret scan on drop would find the
# fake's own copy of it.
case "$last" in
  echo:*) { for a; do [ "$a" = "$last" ] && printf 'echo:<input kept out of this file>\n' || printf '%s\n' "$a"; done; } > "DIR/argv.txt" ;;
  *) printf '%s\n' "$@" > "DIR/argv.txt" ;;
esac
env > "DIR/env.txt"
mkdir -p "DIR/session"
: > "DIR/session/commits.jsonl.zst"
ev() { printf '%s\n' "$1"; }
fin() { ev '{"type":"RunFinished","run":"'"$1"'","status":"'"$2"'","turns":1,"usage":{"input_tokens":'"$3"',"output_tokens":'"$4"',"cost_usd":'"$5"'},"summary":"'"$6"'"}'; }
case "$last" in
  ok)
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    sleep 0.4
    ev '{"type":"RunStarted","run":"C1","agent":"plan"}'
    fin C1 completed 1 1 0.01 planned
    ev '{"type":"RunStarted","run":"C2","agent":"general"}'
    fin C2 completed 1 1 0.01 built
    fin L1 completed 10 5 0.05 "all done"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":0}'
    exit 0 ;;
  step)
    sleep 0.5
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    sleep 30 ;;
  slow)
    trap 'fin L1 cancelled 1 1 0.0 stopped; ev "{\"type\":\"SessionEnd\",\"session\":\"SESSION1\",\"exit_code\":7}"; exit 7' INT
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    while :; do sleep 0.1; done ;;
  ask)
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    ev '{"type":"PermissionNeeded","id":"P1","run":"L1","tool":"bash"}'
    ev '{"type":"PermissionDecided","id":"P1","decision":"denied","reason":"dont_ask"}'
    fin L1 partial 3 3 0.02 "bash was denied"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":10}'
    exit 10 ;;
  echo:*)
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    printf '{"type":"Stream","text":"the step saw %s and kept going\\n"}\n' "$last"
    fin L1 completed 1 1 0.01 "echoed $last, then the last line of the report"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":0}'
    exit 0 ;;
  loud)
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    i=0
    while [ $i -lt 3000 ]; do
      ev '{"type":"Stream","text":"filler line of the log number xxxxxxxxxxxxxxxxxxxxxxxx\n"}'
      i=$((i+1))
    done
    fin L1 completed 1 1 0.01 "the last line of the report"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":0}'
    exit 0 ;;
  costly)
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    ev '{"type":"RunFailed","run":"L1","kind":"budget","message":"budget exceeded"}'
    fin L1 partial 100 100 3.0 "stopped at the budget"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":8}'
    exit 8 ;;
esac
"#
    .replace("DIR", &dir.display().to_string());
    let path = dir.join("fake-aibus.sh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}
