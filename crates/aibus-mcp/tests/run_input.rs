//! How a run gets its input (#246): on the child's stdin, never on its command line, where any
//! local user could read it with `ps`. Against the stand-in for `aibus run`
//! (`common::fake_aibus`), which records how the input reached it.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use aibus_core::Redactor;
use aibus_mcp::{Caps, RunConfig, Server};
use aibus_testkit::Xdg;
use serde_json::{json, Value};

const FLOW: &str = "[[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n";

struct Rig {
    server: Server,
    // Held so the home (and the scan for a leaked key on drop) outlives the run.
    _home: Xdg,
    fake_dir: PathBuf,
    rt: tokio::runtime::Runtime,
}

fn rig() -> Rig {
    let (server, home) = common::server(
        Caps::new(false, false, true, vec![]).unwrap(),
        Redactor::new(),
    );
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ship.toml"), FLOW).unwrap();
    let fake_dir = home.path().join("fake");
    fs::create_dir_all(&fake_dir).unwrap();
    let fake = common::fake_aibus(&fake_dir);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    Rig {
        server: server.with_runs(RunConfig::new(fake, 20.0)),
        _home: home,
        fake_dir,
        rt,
    }
}

impl Rig {
    fn call(&self, tool: &str, args: Value) -> Value {
        let r = self.rt.block_on(self.server.call_async(
            None,
            None,
            tool,
            args.as_object().cloned(),
            Instant::now(),
        ));
        serde_json::to_value(r.unwrap()).unwrap()
    }

    /// Starts `ship` with `input` and polls until the child has exited; the last status body.
    fn finished(&self, input: &str) -> Value {
        let started = self.call(
            "workflow_run",
            json!({ "name": "ship", "scope": "project", "input": input }),
        );
        assert_ne!(started["isError"], true, "{started}");
        let run = started["structuredContent"]["run"]
            .as_str()
            .unwrap()
            .to_owned();
        for _ in 0..100 {
            let r = self.call("run_status", json!({ "run": run, "wait_ms": 2000 }));
            if !r["structuredContent"]["exit_code"].is_null() {
                return r["structuredContent"].clone();
            }
        }
        panic!("the run never ended");
    }

    fn file(&self, name: &str) -> String {
        fs::read_to_string(self.fake_dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }
}

const KEY: &str = "sk-ant-api03-Zq8wLm2Xv5Rt7Yb1Nc4Hd6Gf9Jk3P";

#[test]
fn the_input_reaches_the_child_on_stdin_and_never_on_its_command_line() {
    let rig = rig();
    let input = format!("echo:{KEY}");
    let done = rig.finished(&input);
    // The chain ran from what the child read on stdin, so the input did arrive.
    assert_eq!(done["status"], "completed", "{done}");
    assert!(
        done["tail"].as_str().unwrap().contains("the step saw"),
        "{done}"
    );
    assert_eq!(rig.file("input-via.txt").trim(), "stdin");
    assert_eq!(rig.file("input-len.txt").trim(), input.len().to_string());
    // Its command line ends with the placeholder `-`, and neither it nor the environment holds
    // any of the key (the scan on drop looks at the files too).
    let argv = rig.file("argv.txt");
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(&argv[argv.len() - 2..], ["--", "-"], "{argv:?}");
    assert!(
        !argv
            .iter()
            .any(|a| a.contains("Zq8wLm2X") || a.contains("echo:")),
        "{argv:?}"
    );
    assert!(!rig.file("env.txt").contains("Zq8wLm2X"));
}

#[test]
fn an_input_past_the_single_argument_limit_goes_through_intact() {
    // 200 KB: over the 128 KiB one argument may be on Linux, under the 256 KiB the server takes.
    let rig = rig();
    let body: String = (0..200_000)
        .map(|i| char::from(b'0' + (i % 10) as u8))
        .collect();
    let input = format!("big:{body}");
    let done = rig.finished(&input);
    assert_eq!(done["status"], "completed", "{done}");
    assert!(
        done["tail"]
            .as_str()
            .unwrap()
            .contains(&format!("read {} bytes", input.len())),
        "{done}"
    );
    assert_eq!(rig.file("input-via.txt").trim(), "stdin");
    assert_eq!(rig.file("input-len.txt").trim(), input.len().to_string());
    let ends = rig.file("input-ends.txt");
    let (head, tail) = ends.trim_end().split_once('\n').unwrap();
    assert_eq!((head, tail), (&input[..40], &input[input.len() - 40..]));
}

#[test]
fn a_child_that_exits_without_reading_its_input_is_an_ordinary_failed_run() {
    let rig = rig();
    fs::write(rig.fake_dir.join("deaf"), "").unwrap();
    let input = format!("big:{}", "a".repeat(200_000));
    let done = rig.finished(&input);
    assert_eq!(
        (done["status"].as_str(), done["exit_code"].as_i64()),
        (Some("failed"), Some(3)),
        "{done}"
    );
    assert_eq!(rig.file("input-via.txt").trim(), "stdin");
    // The server is still answering.
    assert_ne!(rig.call("workflow_list", json!({}))["isError"], true);
}
