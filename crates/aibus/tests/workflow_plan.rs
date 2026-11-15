//! #283 from the command line: `workflow plan --dry-run`, `workflow diff`, `workflow manifest` and
//! `workflow export`, and the refusals that keep a plan-only or invalid workflow from starting.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use aibus_testkit::xdg_temp;
use common::{aibus, command, ndjson, stderr};
use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Copies a fixture into the project's workflows directory under `name`.
fn install(home: &Path, fixture: &str, name: &str) -> PathBuf {
    let dir = home.join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.toml"));
    fs::copy(root().join(fixture), &path).unwrap();
    path
}

fn json_of(out: &Output) -> Value {
    serde_json::from_str(&stdout(out))
        .unwrap_or_else(|e| panic!("{e}: {}\n{}", stdout(out), stderr(out)))
}

#[test]
fn plan_dry_run_prints_the_same_plan_every_time() {
    let home = xdg_temp();
    let h = home.path();
    install(
        h,
        "fixtures/workflows/graph/repo-review.toml",
        "repo-review",
    );
    let one = aibus(
        h,
        &["--json", "workflow", "plan", "repo-review", "--dry-run"],
    );
    assert_eq!(one.status.code(), Some(0), "{}", stderr(&one));
    let two = aibus(
        h,
        &["--json", "workflow", "plan", "repo-review", "--dry-run"],
    );
    assert_eq!(one.stdout, two.stdout);
    let plan = json_of(&one);
    assert_eq!(plan["kind"], "aibus.workflow.plan");
    assert!(plan["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    // A path works as well as a name, and gives the same plan.
    let by_path = aibus(
        h,
        &[
            "--json",
            "workflow",
            "plan",
            ".aibus/workflows/repo-review.toml",
            "--dry-run",
        ],
    );
    assert_eq!(json_of(&by_path)["plan_hash"], plan["plan_hash"]);
    // The text form says what a person needs: each step, what it runs on, and that it can't run yet.
    let text = aibus(h, &["workflow", "plan", "repo-review", "--dry-run"]);
    assert_eq!(text.status.code(), Some(0), "{}", stderr(&text));
    let shown = stdout(&text);
    for want in [
        "lead",
        "search",
        "audit",
        "cli.claude",
        "native",
        "plan_hash",
        "#288",
    ] {
        assert!(shown.contains(want), "{want} missing from:\n{shown}");
    }
}

#[test]
fn plan_without_dry_run_is_a_usage_error_and_starts_nothing() {
    let home = xdg_temp();
    let h = home.path();
    install(h, "examples/workflows/fix.toml", "fix");
    let out = aibus(h, &["workflow", "plan", "fix"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--dry-run"), "{}", stderr(&out));
    assert!(!h.join("proj/.aibus/sessions").exists());
}

#[test]
fn an_invalid_workflow_gets_no_plan_and_exits_12() {
    let home = xdg_temp();
    let h = home.path();
    install(h, "fixtures/workflows/graph-invalid/cycle.toml", "cycle");
    let out = aibus(h, &["--json", "workflow", "plan", "cycle", "--dry-run"]);
    assert_eq!(out.status.code(), Some(12), "{}", stderr(&out));
    let report = json_of(&out);
    assert_eq!(report["ok"], false);
    assert!(
        report["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("cycle"),
        "{report}"
    );
    assert!(report.get("plan_hash").is_none());
    // A missing capability is a static error too, and `validate` says the same.
    let dir = h.join("proj/.aibus/workflows");
    fs::write(
        dir.join("weak.toml"),
        "[[step]]\nagent = \"explore\"\nrequires = [\"shell\"]\nprompt = \"p\"\n",
    )
    .unwrap();
    for args in [
        &["--json", "workflow", "plan", "weak", "--dry-run"][..],
        &["--json", "workflow", "validate", "weak"],
    ] {
        let out = aibus(h, args);
        assert_eq!(out.status.code(), Some(12), "{args:?}: {}", stderr(&out));
        assert!(
            stdout(&out).contains("requires shell"),
            "{args:?}: {}",
            stdout(&out)
        );
    }
}

#[test]
fn a_locked_plan_catches_prompt_drift_with_exit_13() {
    let home = xdg_temp();
    let h = home.path();
    let path = install(h, "fixtures/workflows/graph/fan-out.toml", "fan-out");
    let plan = aibus(h, &["--json", "workflow", "plan", "fan-out", "--dry-run"]);
    let lock = h.join("fan-out.plan.json");
    fs::write(&lock, &plan.stdout).unwrap();
    let same = command(h)
        .args(["workflow", "plan", "fan-out", "--dry-run", "--locked"])
        .arg(&lock)
        .output()
        .unwrap();
    assert_eq!(same.status.code(), Some(0), "{}", stderr(&same));
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        text.replace("Find the tests this plan touches", "Find all tests"),
    )
    .unwrap();
    let drift = command(h)
        .args(["workflow", "plan", "fan-out", "--dry-run", "--locked"])
        .arg(&lock)
        .output()
        .unwrap();
    assert_eq!(drift.status.code(), Some(13), "{}", stderr(&drift));
    assert!(
        stdout(&drift).contains("steps.tests.prompt.sha256"),
        "{}",
        stdout(&drift)
    );
}

#[test]
fn workflow_diff_names_what_changed_between_two_revisions() {
    let home = xdg_temp();
    let h = home.path();
    let a = install(h, "fixtures/workflows/graph/fan-out.toml", "a");
    let b = h.join("proj/.aibus/workflows/b.toml");
    let text = fs::read_to_string(&a).unwrap();
    fs::write(
        &b,
        text.replace("max_parallel = 2", "max_parallel = 3")
            .replace("revision = \"2\"", "revision = \"3\""),
    )
    .unwrap();
    let out = aibus(h, &["--json", "workflow", "diff", "a", "b"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report = json_of(&out);
    assert_eq!(report["same"], false);
    let paths: Vec<&str> = report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"envelope.max_parallel"), "{paths:?}");
    assert!(paths.contains(&"workflow.revision"), "{paths:?}");
    assert!(
        !paths.iter().any(|p| p.contains(".prompt.")),
        "no prompt changed: {paths:?}"
    );
    // A saved plan is a side too.
    let saved = h.join("a.plan.json");
    fs::write(
        &saved,
        aibus(h, &["--json", "workflow", "plan", "a", "--dry-run"]).stdout,
    )
    .unwrap();
    let out = command(h)
        .args(["--json", "workflow", "diff"])
        .arg(&saved)
        .arg("a")
        .output()
        .unwrap();
    assert_eq!(json_of(&out)["same"], true, "{}", stdout(&out));
    let human = aibus(h, &["workflow", "diff", "a", "b"]);
    assert!(
        stdout(&human).contains("envelope.max_parallel: 2 -> 3"),
        "{}",
        stdout(&human)
    );
}

#[test]
fn workflow_manifest_pins_the_input_by_hash_and_marks_what_is_unknown() {
    let home = xdg_temp();
    let h = home.path();
    install(
        h,
        "fixtures/workflows/graph/repo-review.toml",
        "repo-review",
    );
    let mut child = command(h)
        .args(["workflow", "manifest", "repo-review", "--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"review PR 17 with a secret plan")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let m = json_of(&out);
    assert_eq!(m["kind"], "aibus.run-manifest");
    assert_eq!(m["task_input"]["bytes"]["value"], 31);
    assert!(!stdout(&out).contains("secret plan"));
    assert!(m["aibus"]["version"]["value"].as_str().is_some());
    assert!(m["execution"]["started_at"]["unknown"].as_str().is_some());
    // The project is not a git repository, and the manifest says so instead of guessing.
    assert!(
        m["repository"]["base_commit"]["unknown"].as_str().is_some(),
        "{}",
        m["repository"]
    );
}

#[test]
fn workflow_export_previews_and_writes_only_with_the_hash_it_showed() {
    let home = xdg_temp();
    let h = home.path();
    install(
        h,
        "fixtures/workflows/graph/repo-review.toml",
        "repo-review",
    );
    let preview = aibus(h, &["--json", "workflow", "export", "repo-review"]);
    assert_eq!(preview.status.code(), Some(0), "{}", stderr(&preview));
    let shown = json_of(&preview);
    assert!(
        shown["diff"].as_str().unwrap().contains("+++ search.md"),
        "{shown}"
    );
    let target = h.join("proj/.claude/agents/search.md");
    assert!(!target.exists(), "a preview writes nothing");
    let wrong = aibus(
        h,
        &["workflow", "export", "repo-review", "--apply", "sha256:abc"],
    );
    assert_eq!(wrong.status.code(), Some(2), "{}", stderr(&wrong));
    assert!(!target.exists());
    let hash = shown["hash"].as_str().unwrap();
    let apply = aibus(h, &["workflow", "export", "repo-review", "--apply", hash]);
    assert_eq!(apply.status.code(), Some(0), "{}", stderr(&apply));
    assert!(fs::read_to_string(&target)
        .unwrap()
        .starts_with("---\nname: search\n"));
}

#[test]
fn a_workflow_file_that_is_not_utf8_or_is_huge_is_refused_without_a_panic() {
    let home = xdg_temp();
    let h = home.path();
    let dir = h.join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("bin.toml"),
        b"[[step]]\nagent = \"plan\"\nprompt = \"\xff\xfe\"\n",
    )
    .unwrap();
    let mut big = String::new();
    while big.len() <= 1024 * 1024 {
        big.push_str(
            "[[step]]\nagent = \"plan\"\nprompt = \"a long enough prompt to fill a megabyte\"\n",
        );
    }
    fs::write(dir.join("big.toml"), big).unwrap();
    for name in ["bin", "big"] {
        for sub in [
            &["workflow", "plan", name, "--dry-run"][..],
            &["workflow", "manifest", name],
            &["workflow", "export", name],
        ] {
            let out = aibus(h, sub);
            assert_eq!(out.status.code(), Some(2), "{sub:?}: {}", stderr(&out));
            assert!(
                !stderr(&out).contains("panicked"),
                "{sub:?}: {}",
                stderr(&out)
            );
        }
    }
}

#[test]
fn running_a_plan_only_workflow_is_refused_before_anything_starts() {
    let home = xdg_temp();
    let h = home.path();
    install(h, "fixtures/workflows/graph/fan-out.toml", "fan-out");
    let script = h.join("s.ron");
    fs::write(&script, "[Text(\"should not be asked\"), Stop(EndTurn)]").unwrap();
    let out = command(h)
        .args([
            "--json",
            "workflow",
            "run",
            "fan-out",
            "add a flag",
            "--script",
        ])
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("the scheduler (#288)"),
        "{}",
        stderr(&out)
    );
    assert!(
        ndjson(&out).iter().all(|l| l["type"] != "RunStarted"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn validate_accepts_the_graph_fixtures_and_warns_about_the_experimental_runtime() {
    let home = xdg_temp();
    let h = home.path();
    install(
        h,
        "fixtures/workflows/graph/repo-review.toml",
        "repo-review",
    );
    install(h, "fixtures/workflows/graph/fan-out.toml", "fan-out");
    let out = aibus(h, &["--json", "workflow", "validate"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}\n{}",
        stdout(&out),
        stderr(&out)
    );
    let report = json_of(&out);
    assert_eq!(report["ok"], true);
    let warned: Vec<&str> = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["message"].as_str().unwrap())
        .collect();
    assert!(
        warned
            .iter()
            .any(|w| w.contains("cli.claude") && w.contains("experimental")),
        "{warned:?}"
    );
}

// Combined with #271: the human plan and diff go through the same cleaner as `workflow show`, and a
// name finds a workflow that only exists inside a `.aibus` bundle.
#[test]
fn plan_and_diff_print_no_escape_sequences_and_a_bundled_name_plans() {
    let home = xdg_temp();
    let h = home.path();
    let dir = h.join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    let esc = "name = \"esc\"\ndescription = \"d\"\n\n[[step]]\nagent = \"explore\"\nprompt = \"look \\u001b[2J{{input}}\"\n\n[[step]]\nagent = \"plan\"\nprompt = \"{{prev.summary}}\"\ngate = \"printf '\\u001b]0;owned\\u0007'; exit 0\"\n";
    fs::write(dir.join("esc.toml"), esc).unwrap();
    fs::copy(
        root().join("examples/workflows/fix.toml"),
        dir.join("fix.toml"),
    )
    .unwrap();
    for args in [
        &["workflow", "plan", "esc", "--dry-run"][..],
        &["workflow", "diff", "esc", "fix"][..],
    ] {
        let out = aibus(h, args);
        let all = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            !all.contains('\u{1b}') && !all.contains('\u{7}'),
            "{args:?}: {all:?}"
        );
    }
    // Pack `fix` into a bundle and remove the loose file: `plan fix` reads it from the bundle.
    let packed = aibus(
        h,
        &[
            "workflow",
            "pack",
            "fix",
            "-o",
            ".aibus/workflows/kit.aibus",
        ],
    );
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
    fs::remove_file(dir.join("fix.toml")).unwrap();
    let plan = aibus(h, &["--json", "workflow", "plan", "fix", "--dry-run"]);
    assert_eq!(plan.status.code(), Some(0), "{}", stderr(&plan));
    assert_eq!(json_of(&plan)["workflow"]["name"], "fix");
}

// #353: an attached step is in the plan like any other, and a run that has no attached session to
// give it to starts nothing.
#[test]
fn an_attached_step_plans_and_a_headless_run_refuses_it_before_anything_starts() {
    let home = xdg_temp();
    let h = home.path();
    install(
        h,
        "fixtures/workflows/attached/review-attached.toml",
        "review-attached",
    );
    let text = aibus(h, &["workflow", "plan", "review-attached", "--dry-run"]);
    assert_eq!(text.status.code(), Some(0), "{}", stderr(&text));
    let shown = stdout(&text);
    for want in [
        "attached",
        "the attached session",
        "next_step",
        "runs now: yes",
    ] {
        assert!(shown.contains(want), "{want} missing from:\n{shown}");
    }
    let plan = json_of(&aibus(
        h,
        &["--json", "workflow", "plan", "review-attached", "--dry-run"],
    ));
    assert_eq!(plan["steps"][1]["runtime"], "attached");
    assert_eq!(plan["steps"][1]["invocation"]["kind"], "attached_session");
    assert_eq!(
        plan["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "data")
            .count(),
        3
    );

    let script = h.join("s.ron");
    fs::write(&script, "[Text(\"should not be asked\"), Stop(EndTurn)]").unwrap();
    let out = command(h)
        .args([
            "--json",
            "workflow",
            "run",
            "review-attached",
            "check it",
            "--script",
        ])
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("--control-attach"),
        "{}",
        stderr(&out)
    );
}
