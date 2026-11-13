//! The log file, `--trace`, the environment variables of 06 section 7, `config edit`, `doctor`
//! options and the completion scripts.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{aibus, command, ndjson, stderr};
use serde_json::Value;
use aibus_testkit::xdg_temp;

const FAKE_KEY: &str = "fake-key-DO-NOT-LOG-7f3a9c";

fn logs(home: &Path) -> PathBuf {
    home.join(".local/state/aibus/logs")
}

fn user_config(home: &Path, text: &str) {
    let dir = home.join(".config/aibus");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), text).unwrap();
}

fn log_text(home: &Path) -> String {
    fs::read_to_string(logs(home).join("aibus.log")).unwrap_or_default()
}

// ---- log file ---------------------------------------------------------------------------

#[test]
fn the_log_level_follows_the_flags_and_aibus_log_adds_directives() {
    let quiet = xdg_temp();
    let out = aibus(quiet.path(), &["-q", "run", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(log_text(quiet.path()).is_empty(), "-q logs only errors");

    let loud = xdg_temp();
    let out = aibus(loud.path(), &["-vv", "run", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = log_text(loud.path());
    assert!(text.contains("DEBUG") || text.contains("INFO"), "{text}");
    assert!(!text.contains(FAKE_KEY));
    assert!(!stderr(&out).contains("DEBUG"), "never to the terminal");

    // AIBUS_LOG alone, without -v: what the directive asks for reaches the file.
    let filtered = xdg_temp();
    let out = command(filtered.path())
        .env("AIBUS_LOG", "trace")
        .args(["run", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!log_text(filtered.path()).is_empty(), "AIBUS_LOG=trace logs something");

    // A directive that does not parse is ignored and noted in the file.
    let bad = xdg_temp();
    let out = command(bad.path())
        .env("AIBUS_LOG", "loud=sometimes")
        .args(["run", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(log_text(bad.path()).contains("ignoring directive"));
}

#[test]
fn old_log_files_rotate_and_only_log_files_kept_remain() {
    let home = xdg_temp();
    user_config(home.path(), "[persist]\nlog_files_kept = 3\n");
    let dir = logs(home.path());
    fs::create_dir_all(&dir).unwrap();
    for day in ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-04"] {
        fs::write(dir.join(format!("aibus.log.{day}")), day).unwrap();
    }
    // Yesterday's file is still called aibus.log.
    let active = dir.join("aibus.log");
    fs::write(&active, "yesterday\n").unwrap();
    let file = fs::File::options().write(true).open(&active).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(2 * 86_400)).unwrap();
    drop(file);

    let out = aibus(home.path(), &["-vv", "run", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    // The active file plus the two newest rotated ones.
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"aibus.log".to_owned()));
    assert!(!names.iter().any(|n| n.ends_with("2026-01-01") || n.ends_with("2026-01-02")), "{names:?}");
    assert!(!log_text(home.path()).contains("yesterday"));
}

// ---- --trace ----------------------------------------------------------------------------

#[test]
fn trace_writes_chrome_trace_events_for_every_kind_of_span() {
    let home = xdg_temp();
    user_config(
        home.path(),
        "[hooks]\nsession_start = [{ command = \"true\" }]\n",
    );
    let trace = home.path().join("run.trace.json");
    let out = aibus(
        home.path(),
        &["--trace", trace.to_str().unwrap(), "run", "--agent", "explore", "map this repo"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_str(&fs::read_to_string(&trace).unwrap()).unwrap();
    let events = doc["traceEvents"].as_array().unwrap();
    let spans: Vec<&Value> = events.iter().filter(|e| e["ph"] == "X").collect();
    let named = |n: &str| spans.iter().filter(|e| e["name"] == n).collect::<Vec<_>>();
    for e in &spans {
        for k in ["ts", "dur", "pid", "tid", "cat"] {
            assert!(e.get(k).is_some(), "{e}");
        }
    }

    let commits = named("commit");
    assert!(commits.len() > 5, "{} commits", commits.len());
    assert!(commits.iter().all(|e| e["args"]["intent"].is_string()
        && e["args"]["changed"].is_u64()
        && e["args"]["recompute_us"].is_u64()));
    assert!(commits.iter().any(|e| e["args"]["intent"] == "RunStarted"));

    let provider = named("provider_call");
    assert_eq!(provider.len(), 2, "two turns, two requests");
    assert!(provider.iter().all(|e| e["args"]["ttfb_ms"].is_u64() && e["args"]["tokens"].is_u64()));
    assert_eq!(named("tool_call")[0]["args"]["tool"], "ls");
    assert_eq!(named("hook")[0]["args"]["event"], "session_start");
    assert!(!named("effect").is_empty(), "no effect spans");
    // Names of the tracks, for the viewer.
    assert!(events.iter().any(|e| e["ph"] == "M" && e["args"]["name"] == "provider_call"));

    // The spans are not log lines.
    assert!(!log_text(home.path()).contains("recompute_us"));
}

#[test]
fn without_trace_nothing_is_written_and_no_span_reaches_the_log() {
    let home = xdg_temp();
    let out = aibus(home.path(), &["-vvv", "run", "--agent", "explore", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!log_text(home.path()).contains("recompute_us"));
    assert!(!log_text(home.path()).contains("provider_call"));
}

// ---- environment variables --------------------------------------------------------------

#[test]
fn limits_and_the_outcome_come_from_the_environment() {
    let home = xdg_temp();
    let show = command(home.path())
        .env("AIBUS_MAX_SPAWN_DEPTH", "1")
        .env("AIBUS_MAX_CONCURRENT_RUNS", "2")
        .args(["config", "show", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!((v["limits"]["max_spawn_depth"].as_u64(), v["limits"]["max_concurrent_runs"].as_u64()), (Some(1), Some(2)));

    let bad = command(home.path()).env("AIBUS_MAX_SPAWN_DEPTH", "deep").args(["config", "show"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));

    let out = command(home.path())
        .env("AIBUS_OUTCOME", "partial_plus")
        .args(["run", "--json", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let id = ndjson(&out).last().unwrap()["session"].as_str().unwrap().to_owned();
    let session: Value = serde_json::from_str(
        &fs::read_to_string(home.path().join(".local/state/aibus/sessions").join(id).join("session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(session["handoff_outcome"], "PARTIAL_PLUS");

    // The flag wins over the variable.
    let out = command(home.path())
        .env("AIBUS_OUTCOME", "failed")
        .args(["run", "--json", "--agent", "explore", "--outcome", "SUCCEEDED", "hi"])
        .output()
        .unwrap();
    let id = ndjson(&out).last().unwrap()["session"].as_str().unwrap().to_owned();
    let text = fs::read_to_string(home.path().join(".local/state/aibus/sessions").join(id).join("session.json")).unwrap();
    assert!(text.contains("\"handoff_outcome\": \"SUCCEEDED\""), "{text}");
}

#[test]
fn the_environment_is_never_listed_in_the_log() {
    let home = xdg_temp();
    let out = command(home.path())
        .env("AIBUS_ACCOUNT_SECRETLY_KEY", "sk-from-the-environment-123456")
        .env("SOME_OTHER_VARIABLE", "marker-value-should-not-appear")
        .args(["-vvv", "run", "--agent", "explore", "hi"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = log_text(home.path());
    assert!(!text.contains("sk-from-the-environment"), "{text}");
    assert!(!text.contains("marker-value-should-not-appear"), "{text}");
}

#[test]
fn config_edit_opens_the_editor_on_the_user_config_and_checks_the_result() {
    let home = xdg_temp();
    let edit = |editor: &str| {
        command(home.path()).env("EDITOR", editor).args(["config", "edit"]).output().unwrap()
    };
    // $EDITOR can carry arguments, and the file is handed over as the last one.
    let ok = edit("printf '[limits]\\nmax_turns = 7\\n' >>");
    assert!(ok.status.success(), "{}", stderr(&ok));
    let show = aibus(home.path(), &["config", "show", "--json"]);
    let v: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(v["limits"]["max_turns"], 7);

    // An edit that breaks the file is exit 3 straight away.
    let bad = edit("printf '[limits]\\nmax_turns = \"many\"\\n' >>");
    assert_eq!(bad.status.code(), Some(3), "{}", stderr(&bad));
    // And an editor that fails is an error, not a silent success.
    assert_eq!(edit("false").status.code(), Some(1));
}

// ---- doctor -----------------------------------------------------------------------------

#[test]
fn doctor_lists_kept_worktrees_and_reads_the_session_numbers() {
    let home = xdg_temp();
    let kept = home.path().join(".cache/aibus/worktrees/S1/R1");
    fs::create_dir_all(&kept).unwrap();
    let out = aibus(home.path(), &["doctor"]);
    assert_eq!(out.status.code(), Some(11));
    assert!(String::from_utf8_lossy(&out.stdout).contains("R1"));
    fs::remove_dir_all(home.path().join(".cache")).unwrap();

    let run = aibus(home.path(), &["run", "--json", "--agent", "explore", "hi"]);
    let id = ndjson(&run).last().unwrap()["session"].as_str().unwrap().to_owned();
    let dir = home.path().join(".local/state/aibus/sessions").join(&id);
    // The run wrote its own numbers on the way out (08 section 5).
    let live = aibus(home.path(), &["doctor", "--session", &id]);
    let live = String::from_utf8_lossy(&live.stdout);
    assert!(live.contains(r#""log_cap":2000"#) && live.contains(r#""commit_cap":4096"#), "{live}");
    fs::write(dir.join("memory.json"), r#"{"rss_mb":42}"#).unwrap();
    let out = aibus(home.path(), &["doctor", "--session", &id]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#"{"rss_mb":42}"#));
    let gone = aibus(home.path(), &["doctor", "--session", "01JC0000000000000000000000"]);
    assert_eq!(gone.status.code(), Some(11));
}

#[test]
fn doctor_shows_the_region_and_project_of_the_cloud_providers() {
    let home = xdg_temp();
    user_config(
        home.path(),
        "[[account]]\nname = \"bedrock\"\nprovider = \"bedrock\"\nauth = { sigv4 = true }\nregion = \"us-east-1\"\n\n\
         [[account]]\nname = \"vertex\"\nprovider = \"vertex\"\nauth = { gcloud = true }\nproject = \"my-project\"\nregion = \"us-east5\"\n",
    );
    let out = aibus(home.path(), &["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let row = |id: &str| text.lines().find(|l| l.contains(&format!("provider: {id}: "))).unwrap_or_default();
    let (bedrock, vertex) = (row("bedrock"), row("vertex"));
    assert!(bedrock.contains("anthropic-bedrock") && bedrock.contains("auth sigv4") && bedrock.contains("region us-east-1"), "{text}");
    assert!(vertex.contains("anthropic-vertex") && vertex.contains("auth gcloud") && vertex.contains("project my-project") && vertex.contains("region us-east5"), "{text}");
}

#[test]
fn doctor_has_a_row_per_provider_and_warns_about_one_no_account_uses() {
    let home = xdg_temp();
    user_config(
        home.path(),
        "[[provider]]\nid = \"lab\"\napi = \"openai-chat\"\nbase_url = \"http://10.1.1.1:8000/v1\"\n\n\
         [[account]]\nname = \"g\"\nprovider = \"groq\"\nauth = { env = \"GROQ_API_KEY\" }\n",
    );
    let out = aibus(home.path(), &["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let groq = text.lines().find(|l| l.contains("provider: groq: ")).unwrap_or_default();
    assert!(groq.contains("openai-chat") && groq.contains("preset groq") && groq.contains("auth env") && groq.contains("no cache"), "{text}");
    assert!(text.lines().any(|l| l.contains("provider: lab: ") && l.contains("custom [[provider]]")), "{text}");
    assert!(text.lines().any(|l| l.contains("warning: [[provider]] lab is used by no enabled account")), "{text}");
    assert_eq!(out.status.code(), Some(0), "a warning is not a failure: {text}");
}

// ---- completions ------------------------------------------------------------------------

#[test]
fn every_shell_gets_a_script_and_the_help_prints_the_key_map() {
    let home = xdg_temp();
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let out = aibus(home.path(), &["completions", shell]);
        assert!(out.status.success(), "{shell}: {}", stderr(&out));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("agent list --names") || text.contains("list --names"), "{shell}");
        assert!(text.len() > 2000, "{shell}");
    }
    assert_eq!(aibus(home.path(), &["completions", "tcsh"]).status.code(), Some(2));
    let help = aibus(home.path(), &["--help"]);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("Keys in the dashboard") && text.contains("Ctrl+c"), "{text}");
}

#[test]
fn the_names_helpers_list_one_name_per_line() {
    let home = xdg_temp();
    user_config(
        home.path(),
        "[[account]]\nname = \"work\"\nprovider = \"fake\"\nauth = { env = \"WORK_KEY\" }\n",
    );
    let wf = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&wf).unwrap();
    fs::write(wf.join("fix.toml"), "").unwrap();
    fs::write(wf.join("review.toml"), "").unwrap();
    let names = |args: &[&str]| -> Vec<String> {
        let out = aibus(home.path(), args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    };
    assert_eq!(names(&["account", "list", "--names"]), ["work"]);
    assert_eq!(names(&["workflow", "list", "--names"]), ["fix", "review"]);
    assert!(names(&["agent", "list", "--names"]).contains(&"explore".to_owned()));
    assert!(names(&["session", "list", "--names"]).is_empty());
}

/// The generated bash script, loaded into a real bash, completes names from the helpers.
#[test]
fn bash_completes_account_names_dynamically() {
    if std::process::Command::new("bash").arg("--version").output().is_err() {
        eprintln!("skipped: no bash");
        return;
    }
    let home = xdg_temp();
    user_config(
        home.path(),
        "[[account]]\nname = \"work\"\nprovider = \"fake\"\nauth = { env = \"WORK_KEY\" }\n\n[[account]]\nname = \"wasp\"\nprovider = \"fake\"\nauth = { env = \"WASP_KEY\" }\n\n[[account]]\nname = \"home\"\nprovider = \"fake\"\nauth = { env = \"HOME_KEY\" }\n",
    );
    let script = aibus(home.path(), &["completions", "bash"]);
    let path = home.path().join("aibus.bash");
    fs::write(&path, &script.stdout).unwrap();
    let syntax = std::process::Command::new("bash").arg("-n").arg(&path).output().unwrap();
    assert!(syntax.status.success(), "{}", String::from_utf8_lossy(&syntax.stderr));

    let bin_dir = Path::new(env!("CARGO_BIN_EXE_aibus")).parent().unwrap().to_owned();
    let complete = |words: &str, cword: usize| -> Vec<String> {
        let body = format!(
            "source {}; COMP_WORDS=({words}); COMP_CWORD={cword}; _aibus_dynamic; printf '%s\\n' \"${{COMPREPLY[@]}}\"",
            path.display()
        );
        let mut cmd = std::process::Command::new("bash");
        cmd.arg("-c")
            .arg(body)
            .env("HOME", home.path())
            .env("PATH", format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap()))
            .current_dir(home.path().join("proj"));
        // The `aibus` the script calls must read this test's home, not an XDG directory the
        // host happens to export (a hosted runner does).
        for v in common::XDG {
            cmd.env_remove(v);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "bash failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    };
    // `aibus account show w<TAB>` offers the two accounts that start with w.
    assert_eq!(complete("aibus account show w", 3), ["work", "wasp"]);
    assert_eq!(complete("aibus --account ''", 2).len(), 3);
    assert_eq!(complete("aibus --account h", 2), ["home"]);
}
