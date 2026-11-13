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
[ "$1" = --aibus-fake-probe ] && exit 0
for last; do :; done
input=$last
[ "$last" = "-" ] && input=$(cat)
# The input is the caller's text and a test may plant a key in it on purpose (`echo:<key>`), so the
# record keeps the shape of the line but not that text, or the secret scan on drop would find the
# fake's own copy of it.
case "$last" in
  echo:*|leak*:*|leak:*|split*:*) { for a; do [ "$a" = "$last" ] && printf '%s:<input kept out of this file>\n' "${last%%:*}" || printf '%s\n' "$a"; done; } > "DIR/argv.txt" ;;
  *) printf '%s\n' "$@" > "DIR/argv.txt" ;;
esac
env > "DIR/env.txt"
mkdir -p "DIR/session"
: > "DIR/session/commits.jsonl.zst"
ev() { printf '%s\n' "$1"; }
fin() { ev '{"type":"RunFinished","run":"'"$1"'","status":"'"$2"'","turns":1,"usage":{"input_tokens":'"$3"',"output_tokens":'"$4"',"cost_usd":'"$5"'},"summary":"'"$6"'"}'; }
case "$input" in
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
    printf '{"type":"Stream","text":"the step saw %s and kept going\\n"}\n' "$input"
    fin L1 completed 1 1 0.01 "echoed $input, then the last line of the report"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":0}'
    exit 0 ;;
  leak:*)
    # The input after `leak:` is a key. It comes back in the strings of an attempt (its name,
    # status and failure) and in the session; the failure is padded so the 300 byte cut lands
    # inside the key.
    key="${input#leak:}"
    pad=$(printf 'x%.0s' $(seq 1 280))
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    ev '{"type":"RunStarted","run":"C1","agent":"'"$key"'"}'
    ev '{"type":"RunFailed","run":"C1","kind":"provider","message":"'"$pad$key"' and more"}'
    ev '{"type":"RunFinished","run":"C1","status":"'"$key"'","turns":1,"usage":{"input_tokens":1,"output_tokens":1,"cost_usd":0.01},"summary":"s"}'
    fin L1 failed 1 1 0.01 s
    ev '{"type":"SessionEnd","session":"'"$key"'","exit_code":1}'
    exit 1 ;;
  leakstream:*)
    # The key in one stream event, placed so the 4096 byte window of the tail starts 20 bytes
    # into it: 100 before, 4070 after, a newline, and the 3 bytes the summary `s` adds.
    key="${input#leakstream:}"
    before=$(printf 'x%.0s' $(seq 1 100))
    after=$(printf '.%.0s' $(seq 1 4070))
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    ev '{"type":"Stream","text":"'"$before$key$after"'\n"}'
    fin L1 failed 1 1 0.01 s
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":1}'
    exit 1 ;;
  leaksummary:*)
    # The key at the start of the lead's summary, which is 4115 bytes, so the window starts 20
    # bytes into the key (the summary is pushed as a newline, the text, a newline).
    key="${input#leaksummary:}"
    after=$(printf '.%.0s' $(seq 1 4073))
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    fin L1 failed 1 1 0.01 "$key$after"
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":1}'
    exit 1 ;;
  leakstray:*)
    # The same for a failure of a run that is not an attempt, which goes to the report (4113 bytes
    # with the two newlines the empty summary adds).
    key="${input#leakstray:}"
    after=$(printf '.%.0s' $(seq 1 4071))
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    ev '{"type":"RunFailed","run":"NOSUCH","kind":"provider","message":"'"$key$after"'"}'
    fin L1 failed 1 1 0.01 ""
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":1}'
    exit 1 ;;
  splitlive:*)
    # The key in two stream events and then the run stays alive, so a test reads in the middle.
    key="${input#splitlive:}"
    c() { printf '%s' "$key" | cut -c"$1"; }
    trap 'fin L1 cancelled 1 1 0.0 stopped; ev "{\"type\":\"SessionEnd\",\"session\":\"SESSION1\",\"exit_code\":7}"; exit 7' INT
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    ev '{"type":"Stream","text":"'"$(c 1-25)"'"}'
    ev '{"type":"Stream","text":"'"$(c 26-)"'"}'
    while :; do sleep 0.1; done ;;
  split2:*|split3:*|splitnear:*)
    # The key in pieces over two or three stream events, as a streamed key arrives, with the
    # window of the tail starting 20 bytes into it (`splitnear` has nothing after it, so no cut).
    key="${input#*:}"
    after=$(printf '.%.0s' $(seq 1 4070))
    [ "${input%%:*}" = splitnear ] && after=z
    c() { printf '%s' "$key" | cut -c"$1"; }
    ev '{"type":"RunStarted","run":"L1","agent":"lead"}'
    case "${input%%:*}" in
      split3)
        ev '{"type":"Stream","text":"'"$(c 1-14)"'"}'
        ev '{"type":"Stream","text":"'"$(c 15-30)"'"}'
        ev '{"type":"Stream","text":"'"$(c 31-)$after"'\n"}' ;;
      *)
        ev '{"type":"Stream","text":"'"$(c 1-25)"'"}'
        ev '{"type":"Stream","text":"'"$(c 26-)$after"'\n"}' ;;
    esac
    fin L1 failed 1 1 0.01 s
    ev '{"type":"SessionEnd","session":"SESSION1","exit_code":1}'
    exit 1 ;;
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
    // Another thread forking right now can give a child a copy of the write handle until it execs,
    // and starting the script then fails with ETXTBSY (#232). A handle that is already closed
    // cannot be copied, so once one start succeeds none can fail that way. That start is a probe:
    // the probe is started with `--aibus-fake-probe` as its only argument, which a real run never
    // has (it starts with `--cwd`), and the first line of the script exits 0 when it sees it, so the body never runs, no file is written and nothing outlives the wait below.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut probe = loop {
        let started = std::process::Command::new(&path)
            .arg("--aibus-fake-probe")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match started {
            Ok(child) => break child,
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => panic!("the fake {} would not start: {e}", path.display()),
        }
    };
    while probe.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = probe.kill();
            let _ = probe.wait();
            panic!("the probe of {} did not exit", path.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    path
}
