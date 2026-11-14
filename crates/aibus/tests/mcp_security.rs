//! `aibus mcp` and secrets (SECURITY.md): whatever a client sends, nothing key-shaped comes back
//! over stdio, reaches the audit log or the trace on stderr.

mod common;

use aibus_testkit::xdg_temp;
use common::mcp_client::{lines_that_are_not_json_rpc, mcp_command, McpClient, Protocol};
use serde_json::json;

/// Key-shaped for the redactor's `anthropic-key` pattern, and unique to this test.
const KEY: &str = "sk-ant-api03-Zq8wLm2Xv5Rt7Yb1Nc4Hd6Gf9Jk3P";

#[test]
fn no_secret_leaves_over_stdio() {
    let mut home = xdg_temp();
    // The temp tree is searched for it when the test ends, the audit log included. Every kind in
    // `CREDENTIAL_KINDS` that a pattern knows is forbidden by `xdg_temp` already; they ride along
    // as arguments below, next to KEY.
    home.forbid(KEY);
    let by_shape: Vec<&str> = aibus_testkit::CREDENTIAL_KINDS
        .iter()
        .filter(|k| k.2.is_some())
        .map(|k| k.1)
        .collect();
    for p in Protocol::BOTH {
        let mut cmd = mcp_command(home.path(), &[]);
        cmd.env("AIBUS_LOG", "trace");
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        // The key as an argument, as a tool name and as a made-up cursor.
        let refused = c
            .call("workflow_list", json!({ "api_key": KEY }))
            .unwrap_err();
        // Every kind with a shape the redactor knows, as an argument and as a cursor.
        let many = c
            .call("workflow_list", json!({ "notes": by_shape }))
            .unwrap_err();
        let paged_many = c.list_tools_page(Some(&by_shape.join(" "))).unwrap_err();
        for e in [&many, &paged_many] {
            for v in &by_shape {
                assert!(!e.message.contains(v), "{p:?}: {v} came back in an error");
            }
        }
        let unknown = c.call(&format!("tool-{KEY}"), json!({})).unwrap_err();
        let paged = c.list_tools_page(Some(KEY)).unwrap_err();
        for e in [&refused, &unknown, &paged] {
            assert!(!e.message.contains(KEY), "{p:?}: {e:?}");
        }
        c.call("workflow_list", json!({}))
            .expect("a normal call still works");
        let (done, lines) = c.finish();
        assert_eq!(
            lines_that_are_not_json_rpc(&lines),
            Vec::<String>::new(),
            "{p:?}"
        );
        assert!(
            !lines.join("\n").contains(KEY),
            "{p:?}: the key came back over stdout"
        );
        for v in &by_shape {
            assert!(
                !lines.join("\n").contains(v) && !done.stderr.contains(v),
                "{p:?}: {v} reached stdio"
            );
        }
        assert!(!done.stderr.contains(KEY), "{p:?}: the key reached stderr");
    }
    // One audit file per process, each with the key redacted by pattern.
    let dir = home.path().join(".local/state/aibus/logs/mcp");
    let mut seen = 0;
    for f in std::fs::read_dir(&dir)
        .expect("the audit directory")
        .flatten()
    {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains(KEY), "{}", f.path().display());
        for v in &by_shape {
            assert!(
                !text.contains(v),
                "{}: {v} is in the audit file",
                f.path().display()
            );
        }
        seen += text.matches("[redacted:pattern:anthropic-key]").count();
    }
    assert!(
        seen >= 2,
        "the audit log did not record the redacted arguments ({seen})"
    );
}

const FLOW: &str = "[[step]]\nagent = \"explore\"\nprompt = \"Look: {{input}}\"\n";

/// Every path below `dir`.
fn files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        out.push(e.path());
        out.extend(files(&e.path()));
    }
    out.sort();
    out
}

#[cfg(unix)]
#[test]
fn a_symlinked_workflows_dir_cannot_be_used_to_write_outside_it() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
        std::os::unix::fs::symlink(&outside, home.path().join("proj/.aibus/workflows")).unwrap();
        let mut c = McpClient::start(home.path(), p, &["--allow", "write"]);
        let r = c
            .call(
                "workflow_create",
                json!({ "name": "ship", "scope": "project", "toml": FLOW }),
            )
            .expect("a tool error");
        assert_eq!(r["isError"], true, "{p:?}: {r}");
        assert_eq!(files(&outside), Vec::<std::path::PathBuf>::new(), "{p:?}");
    }
}

#[test]
fn no_write_tool_can_reach_settings_or_the_user_config() {
    let home = xdg_temp();
    std::fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
    std::fs::create_dir_all(home.path().join(".config/aibus")).unwrap();
    let settings = home.path().join("proj/.aibus/settings.toml");
    let config = home.path().join(".config/aibus/config.toml");
    std::fs::write(&settings, "# mine\n").unwrap();
    std::fs::write(&config, "# mine\n").unwrap();
    let mut c = McpClient::start(
        home.path(),
        Protocol::V2026_07_28,
        &["--allow", "write", "--allow", "gates"],
    );
    for (scope, name) in [
        ("project", "../settings"),
        ("project", "../settings.toml"),
        ("user", "../config"),
        ("user", "../config.toml"),
        ("user", "../../../../.config/aibus/config"),
        ("project", "/etc/passwd"),
        ("project", "~/.config/aibus/config"),
    ] {
        let r = c
            .call(
                "workflow_create",
                json!({ "name": name, "scope": scope, "toml": FLOW }),
            )
            .expect("a tool error");
        assert_eq!(r["isError"], true, "{scope} {name}: {r}");
        let u = c
            .call(
                "workflow_update",
                json!({ "name": name, "scope": scope, "toml": FLOW, "expected_revision": "0".repeat(64) }),
            )
            .expect("a tool error");
        assert_eq!(u["isError"], true, "{scope} {name}: {u}");
    }
    // The names that are valid land in the workflows directory and nowhere else.
    for (scope, name) in [("project", "settings"), ("user", "config")] {
        let r = c
            .call(
                "workflow_create",
                json!({ "name": name, "scope": scope, "toml": FLOW }),
            )
            .expect("create");
        assert_ne!(r["isError"], true, "{r}");
    }
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "# mine\n");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "# mine\n");
    assert!(home
        .path()
        .join("proj/.aibus/workflows/settings.toml")
        .is_file());
    assert!(home
        .path()
        .join(".config/aibus/workflows/config.toml")
        .is_file());
}

#[test]
fn no_credential_reaches_stdout_or_the_audit_log_during_a_run() {
    let mut home = xdg_temp();
    home.forbid(KEY);
    let dir = home.path().join("proj/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ship.toml"), FLOW).unwrap();
    for p in Protocol::BOTH {
        let mut cmd = mcp_command(home.path(), &["--allow", "run"]);
        cmd.env("AIBUS_LOG", "trace");
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        // The key as the run's input and as a made-up handle.
        let started = c
            .call(
                "workflow_run",
                json!({ "name": "ship", "scope": "project", "input": format!("use {KEY}") }),
            )
            .expect("workflow_run");
        let run = started["structuredContent"]["run"]
            .as_str()
            .expect("a handle")
            .to_owned();
        for _ in 0..120 {
            let r = c
                .call("run_status", json!({ "run": run, "wait_ms": 5000 }))
                .expect("run_status");
            if !r["structuredContent"]["exit_code"].is_null() {
                break;
            }
        }
        // The run resources carry the same promise: the key as the input must not come back.
        let log = c
            .read_resource(&format!("aibus://runs/{run}/log"))
            .expect("the log resource");
        let status = c
            .read_resource(&format!("aibus://runs/{run}"))
            .expect("the status resource");
        assert!(
            !format!("{log}{status}").contains(KEY),
            "{p:?}: the key came back in a run resource"
        );
        assert!(
            c.read_resource(&format!("aibus://runs/run-{KEY}")).is_err(),
            "{p:?}"
        );
        let unknown = c
            .call("run_status", json!({ "run": format!("run-{KEY}") }))
            .expect("a tool error");
        assert_eq!(unknown["isError"], true);
        let (done, lines) = c.finish();
        assert_eq!(
            lines_that_are_not_json_rpc(&lines),
            Vec::<String>::new(),
            "{p:?}"
        );
        assert!(
            !lines.join("\n").contains(KEY),
            "{p:?}: the key came back over stdout"
        );
        assert!(!done.stderr.contains(KEY), "{p:?}: the key reached stderr");
    }
    // The audit files and the sessions of the runs are searched again when `home` is dropped.
    let dir = home.path().join(".local/state/aibus/logs/mcp");
    for f in std::fs::read_dir(&dir)
        .expect("the audit directory")
        .flatten()
    {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains(KEY), "{}", f.path().display());
    }
}

/// G9: every tool `aibus mcp --allow write` adds to the read-only set is one the author's session
/// asks about every time and never offers "always" for. The list comes from the server itself, so
/// a write tool added later fails here until `mcp::asks_every_time` knows it.
#[test]
fn every_mcp_write_tool_asks_and_hides_always() {
    let home = xdg_temp();
    let read: Vec<String> = McpClient::start(home.path(), Protocol::V2026_07_28, &[])
        .list_tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let mut c = McpClient::start(home.path(), Protocol::V2026_07_28, &["--allow", "write"]);
    let writes: Vec<String> = c
        .list_tools()
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] != true)
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        writes.iter().any(|n| n == "workflow_create")
            && writes.iter().any(|n| n == "workflow_update"),
        "{writes:?}"
    );
    for name in &writes {
        assert!(
            !read.contains(name),
            "{name} is a write tool and was listed without the flag"
        );
        let wire = format!("mcp__{}__{}", aibus_core::mcp::WRITE_SERVER, name);
        assert!(
            aibus_core::mcp::asks_every_time(&wire),
            "{wire} does not ask every time"
        );
    }
    // And the read tools are not in the set, so a read-only session never asks about them.
    for name in &read {
        let wire = format!("mcp__{}__{}", aibus_core::mcp::WRITE_SERVER, name);
        assert!(
            !aibus_core::mcp::asks_every_time(&wire),
            "{wire} would ask for a read"
        );
    }
}

/// #183: a fake key planted where a real one could sit (a workflow file, the arguments of every
/// tool, a run's input, a made-up tool name, handle and uri), then every tool, resource and
/// prompt driven with `--allow write` and `--allow run`. The key is in no reply, no stdout line,
/// no stderr at trace level, no audit line and, when `home` is dropped, in no file of the tree
/// (sessions, commit logs and audit files included). The one file that holds it on purpose is
/// removed first.
#[test]
fn a_planted_key_appears_in_no_byte_the_server_writes_or_stores() {
    const PLANTED: &str = "sk-ant-api03-Hv7Qe2Ls9Tn4Wx6Zc1Bd8Fg3Jm5R";
    let mut home = xdg_temp();
    home.forbid(PLANTED);
    let dir = home.path().join("proj/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    let leaky = dir.join("leaky.toml");
    std::fs::write(
        &leaky,
        format!("[[step]]\nagent = \"explore\"\nprompt = \"key {PLANTED}: {{{{input}}}}\"\n"),
    )
    .unwrap();
    std::fs::write(dir.join("ship.toml"), FLOW).unwrap();
    let keyed = format!("[[step]]\nagent = \"explore\"\nprompt = \"{PLANTED}\"\n");
    let unknown_tool = format!("tool-{PLANTED}");
    for p in Protocol::BOTH {
        let mut cmd = mcp_command(home.path(), &["--allow", "write", "--allow", "run"]);
        cmd.env("AIBUS_LOG", "trace");
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        let mut replies: Vec<String> = vec![json!(c.list_tools()).to_string()];
        let mut note = |r: common::mcp_client::Reply| {
            replies.push(match r {
                Ok(v) => v.to_string(),
                Err(e) => format!("{e:?}"),
            });
        };
        for (tool, args) in [
            ("workflow_list", json!({})),
            ("workflow_get", json!({ "name": "leaky" })),
            ("workflow_explain", json!({ "name": "leaky" })),
            ("workflow_validate", json!({ "name": "leaky" })),
            ("workflow_validate", json!({ "toml": keyed })),
            (
                "workflow_dry_run",
                json!({ "name": "leaky", "input": format!("use {PLANTED}") }),
            ),
            ("agent_list", json!({})),
            // A write that carries the key is refused and never echoed.
            (
                "workflow_create",
                json!({ "name": "bad", "scope": "project", "toml": keyed }),
            ),
            (
                "workflow_create",
                json!({ "name": format!("n-{PLANTED}"), "scope": "project", "toml": FLOW }),
            ),
            (
                "workflow_update",
                json!({ "name": "ship", "scope": "project", "toml": FLOW, "expected_revision": PLANTED }),
            ),
            ("run_status", json!({ "run": format!("run-{PLANTED}") })),
            ("run_cancel", json!({ "run": format!("run-{PLANTED}") })),
            (unknown_tool.as_str(), json!({})),
        ] {
            note(c.call(tool, args));
        }
        let started = c
            .call(
                "workflow_run",
                json!({ "name": "leaky", "scope": "project", "input": format!("use {PLANTED}") }),
            )
            .expect("workflow_run");
        let run = started["structuredContent"]["run"]
            .as_str()
            .expect("a handle")
            .to_owned();
        note(Ok(started));
        for _ in 0..120 {
            let r = c
                .call("run_status", json!({ "run": run, "wait_ms": 5000 }))
                .expect("run_status");
            let finished = !r["structuredContent"]["exit_code"].is_null();
            note(Ok(r));
            if finished {
                break;
            }
        }
        for uri in [
            "aibus://workflows".to_owned(),
            "aibus://workflows/project/leaky".to_owned(),
            "aibus://schema/workflow".to_owned(),
            "aibus://agents".to_owned(),
            format!("aibus://runs/{run}"),
            format!("aibus://runs/{run}/log"),
            format!("aibus://runs/run-{PLANTED}"),
            format!("aibus://workflows/project/{PLANTED}"),
        ] {
            note(c.read_resource(&uri));
        }
        note(c.list_prompts());
        note(c.get_prompt(
            "author_workflow",
            json!({ "goal": format!("use {PLANTED}") }),
        ));
        note(c.get_prompt("review_workflow", json!({ "name": "leaky" })));
        note(c.get_prompt("explain_run", json!({ "run": run })));

        let (done, lines) = c.finish();
        assert_eq!(
            lines_that_are_not_json_rpc(&lines),
            Vec::<String>::new(),
            "{p:?}"
        );
        assert!(
            !lines.join("\n").contains(PLANTED),
            "{p:?}: the key came back over stdout"
        );
        assert!(
            !done.stderr.contains(PLANTED),
            "{p:?}: the key reached stderr"
        );
        for r in &replies {
            assert!(!r.contains(PLANTED), "{p:?}: the key is in a reply: {r}");
        }
        assert!(
            replies.len() > 25,
            "{p:?}: only {} replies were checked",
            replies.len()
        );
    }
    let audit = home.path().join(".local/state/aibus/logs/mcp");
    let mut lines = 0;
    for f in std::fs::read_dir(&audit)
        .expect("the audit directory")
        .flatten()
    {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains(PLANTED), "{}", f.path().display());
        lines += text.lines().count();
    }
    assert!(lines > 50, "the audit files hold only {lines} lines");
    std::fs::remove_file(&leaky).unwrap();
}

/// Every path below `dir` with its bytes (`None` for a directory), so a before and an after show a
/// change to a file as well as a new one.
fn snapshot(
    dir: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    let mut out = std::collections::BTreeMap::new();
    for p in files(dir) {
        let bytes = if p.is_dir() {
            None
        } else {
            std::fs::read(&p).ok()
        };
        out.insert(p, bytes);
    }
    out
}

/// A workflow whose gate would leave `gate-ran` behind if anything ran it.
fn trap(home: &std::path::Path) -> std::path::PathBuf {
    let dir = home.join("proj/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        "description = \"trap\"\n\n[[step]]\nagent = \"explore\"\nprompt = \"Look: {{{{input}}}}\"\ngate = \"touch {}\"\n",
        home.join("gate-ran").display()
    );
    std::fs::write(dir.join("trap.toml"), toml).unwrap();
    home.join("gate-ran")
}

/// #183: with every flag off, nothing a client can ask for writes, runs, connects or spawns.
/// The tripwires are a gate that would touch a sentinel, a loopback listener that every proxy and
/// base-url variable points at, a snapshot of the home directory and the server's own children and
/// sockets. The same tripwires are tripped by `--allow run`, which is what makes them worth
/// reading.
#[test]
fn with_no_flag_nothing_writes_runs_connects_or_spawns() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let sentinel = trap(home.path());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let before = snapshot(home.path());

        let mut cmd = mcp_command(home.path(), &[]);
        for v in [
            "http_proxy",
            "https_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "ANTHROPIC_BASE_URL",
            "OPENAI_BASE_URL",
        ] {
            cmd.env(v, &url);
        }
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        let listed: Vec<String> = c
            .list_tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        // Every tool the server lists, once, with arguments that would do the most if they could.
        let args = |name: &str| match name {
            "workflow_list" | "agent_list" => json!({}),
            "workflow_validate" => json!({ "name": "trap" }),
            "workflow_dry_run" => json!({ "name": "trap", "input": "x" }),
            _ => json!({ "name": "trap", "scope": "project" }),
        };
        assert_eq!(listed.len(), 6, "{p:?}: {listed:?}");
        for name in &listed {
            let r = c
                .call(name, args(name))
                .unwrap_or_else(|e| panic!("{p:?}: {name}: {e:?}"));
            assert_ne!(r["isError"], true, "{p:?}: {name}: {r}");
        }
        // The tools that are off, by name, as a client that read the docs would try them.
        let toml =
            std::fs::read_to_string(home.path().join("proj/.aibus/workflows/trap.toml")).unwrap();
        for (name, a) in [
            (
                "workflow_create",
                json!({ "name": "evil", "scope": "project", "toml": toml }),
            ),
            (
                "workflow_update",
                json!({ "name": "trap", "scope": "project", "toml": toml, "expected_revision": "0".repeat(64) }),
            ),
            (
                "workflow_run",
                json!({ "name": "trap", "scope": "project", "input": "x" }),
            ),
            (
                "run_status",
                json!({ "run": "run-00000000000000000000000000" }),
            ),
            (
                "run_cancel",
                json!({ "run": "run-00000000000000000000000000" }),
            ),
        ] {
            assert_eq!(
                c.call(name, a).expect_err(name).code,
                -32602,
                "{p:?}: {name}"
            );
        }
        // Every resource and prompt.
        for uri in [
            "aibus://workflows",
            "aibus://workflows/project/trap",
            "aibus://schema/workflow",
            "aibus://agents",
        ] {
            c.read_resource(uri)
                .unwrap_or_else(|e| panic!("{p:?}: {uri}: {e:?}"));
        }
        assert_eq!(
            c.read_resource("aibus://runs/run-00000000000000000000000000")
                .unwrap_err()
                .code,
            -32602
        );
        c.request("resources/list", json!({}))
            .expect("resources/list");
        c.request("resources/templates/list", json!({}))
            .expect("resources/templates/list");
        c.list_prompts().expect("prompts/list");
        c.get_prompt("author_workflow", json!({ "goal": "run the trap" }))
            .expect("author_workflow");
        c.get_prompt("review_workflow", json!({ "name": "trap" }))
            .expect("review_workflow");
        c.get_prompt(
            "explain_run",
            json!({ "run": "run-00000000000000000000000000" }),
        )
        .expect("explain_run");

        // Spawn: no child now, and the gate never ran.
        if let Ok(ps) = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,ppid="])
            .output()
        {
            let table = String::from_utf8_lossy(&ps.stdout).into_owned();
            let me = c.pid().to_string();
            let children: Vec<&str> = table
                .lines()
                .filter(|l| l.split_whitespace().nth(1) == Some(me.as_str()))
                .collect();
            assert!(
                children.is_empty(),
                "{p:?}: the server has children: {children:?}"
            );
        }
        assert!(!sentinel.exists(), "{p:?}: the gate ran");
        // Network: no connection reached the listener, and no socket is open in the server.
        assert!(
            matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "{p:?}: a connection"
        );
        #[cfg(target_os = "linux")]
        for fd in std::fs::read_dir(format!("/proc/{}/fd", c.pid()))
            .unwrap()
            .flatten()
        {
            let target = std::fs::read_link(fd.path())
                .map(|t| t.display().to_string())
                .unwrap_or_default();
            assert!(
                !target.starts_with("socket:"),
                "{p:?}: an open socket {target}"
            );
        }
        let (done, lines) = c.finish();
        assert!(done.status.success(), "{p:?}: {}", done.stderr);
        assert_eq!(
            lines_that_are_not_json_rpc(&lines),
            Vec::<String>::new(),
            "{p:?}"
        );

        // Write: everything that was there is there, byte for byte, and the only new files are
        // the audit file's.
        let after = snapshot(home.path());
        for (path, bytes) in &before {
            assert_eq!(
                after.get(path),
                Some(bytes),
                "{p:?}: {} changed",
                path.display()
            );
        }
        let audit = home.path().join(".local/state/aibus/logs");
        let new: Vec<_> = after
            .keys()
            .filter(|k| !before.contains_key(*k) && !k.starts_with(&audit) && !audit.starts_with(k))
            .collect();
        assert!(
            new.is_empty(),
            "{p:?}: new paths outside the audit log: {new:?}"
        );
        assert!(
            !home.path().join(".local/state/aibus/sessions").exists(),
            "{p:?}: a session was made"
        );
    }
}

/// The control for the test above: the same tripwires, with every flag on, go off. A tripwire that
/// nothing can trip would pass the test above for any server.
#[test]
fn the_tripwires_trip_when_the_flags_are_on() {
    let home = xdg_temp();
    let sentinel = trap(home.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(listener.accept().is_ok(), "the listener sees a connection");

    let mut c = McpClient::start(
        home.path(),
        Protocol::V2026_07_28,
        &[
            "--allow",
            "write",
            "--allow",
            "gates",
            "--allow",
            "run",
            "--confirm",
            "never",
        ],
    );
    c.list_tools();
    let started = c
        .call(
            "workflow_run",
            json!({ "name": "trap", "scope": "project", "input": "x" }),
        )
        .expect("workflow_run");
    let run = started["structuredContent"]["run"]
        .as_str()
        .expect("a handle")
        .to_owned();
    for _ in 0..120 {
        let r = c
            .call("run_status", json!({ "run": run, "wait_ms": 5000 }))
            .expect("run_status");
        if !r["structuredContent"]["exit_code"].is_null() {
            break;
        }
    }
    assert!(sentinel.exists(), "the gate did not run under --allow run");
    assert!(
        home.path().join(".local/state/aibus/sessions").is_dir(),
        "no session was made under --allow run"
    );
}
