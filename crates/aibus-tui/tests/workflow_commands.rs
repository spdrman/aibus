//! G7: `/workflow list`, `show`, `save` and `run`. The prompt turns each into a `Command`, the
//! terminal loop answers the file ones through `workflows::apply`, and the modals draw from the
//! graph. The rig stands in for the loop: it steps what `apply` returns.

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use aibus_core::state::value::{as_list, as_str, get, map, s};
use aibus_core::state::WORKFLOW_DRAFT;
use aibus_core::workflow::EXAMPLES;
use aibus_core::*;
use aibus_graph::Graph;
use aibus_testkit::assert_frame;
use aibus_testkit::tui::poke;
use aibus_tui::hit::HitId;
use aibus_tui::workflows::{apply, WorkflowFiles};
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;

const AT: u64 = 1_700_000_000;
const REVIEW: &str = EXAMPLES[2].1;

fn rig_in(root: &Path) -> (Rig, WorkflowFiles) {
    let mut app = session();
    with_agents(&mut app, &["explore", "plan", "sleuth"]);
    let files = WorkflowFiles {
        dirs: workflow::dirs(&root.join("project"), Some(&root.join("config"))),
        cap: 32_768,
        redactor: Redactor::default(),
    };
    (Rig::new(app, (100, 40), Theme::recording()), files)
}

fn draft(r: &mut Rig, toml: &str) {
    poke(
        &mut r.app,
        WORKFLOW_DRAFT,
        map([("toml", s(toml)), ("summary", s("valid"))]),
    );
}

/// What the loop does with the messages a key press produced.
fn answer(r: &mut Rig, files: &WorkflowFiles, msgs: &[Msg]) {
    for m in msgs {
        if let Msg::Command(Command::WorkflowFile(op)) = m {
            for a in apply(
                op,
                &r.app,
                files,
                SystemTime::UNIX_EPOCH + Duration::from_secs(AT),
            ) {
                r.app.step(a).unwrap();
            }
        }
    }
    r.settle();
}

fn line(r: &mut Rig, files: &WorkflowFiles, text: &str) {
    r.type_text(text);
    let msgs = r.key(K::Enter);
    answer(r, files, &msgs);
}

fn log_has(r: &Rig, needle: &str) -> bool {
    r.text().contains(needle)
}

#[test]
fn save_writes_a_valid_file_with_the_two_header_lines() {
    let root = tempfile::tempdir().unwrap();
    let (mut r, files) = rig_in(root.path());
    draft(&mut r, REVIEW);
    line(&mut r, &files, "/workflow save review");
    let path = root.path().join("project/.aibus/workflows/review.toml");
    let saved = std::fs::read_to_string(&path).unwrap();
    let mut lines = saved.lines();
    assert_eq!(lines.next(), Some("# schema: aibus://schema/workflow v1"));
    assert_eq!(
        lines.next(),
        Some("# written through aibus, 2023-11-14T22:13:20.000Z")
    );
    assert!(workflow::Workflow::parse("review", &saved).is_ok());
    // The list the prompt's shorthands read now has it, so `/review` is a command.
    let rows = r
        .app
        .graph
        .read_key("model:workflows")
        .map(as_list)
        .unwrap_or(&[])
        .to_vec();
    assert_eq!(rows.len(), 1);
    assert_eq!(get(&rows[0], "name").and_then(as_str), Some("review"));
    r.type_text("/review the diff");
    let sent = r.key(K::Enter);
    assert!(
        names(&sent)
            .iter()
            .any(|m| m.contains("Workflow { name: \"review\"")),
        "{sent:?}"
    );
    // `--user` goes to the user's directory.
    line(&mut r, &files, "/workflow save mine --user");
    assert!(root.path().join("config/workflows/mine.toml").exists());
}

#[test]
fn save_refuses_errors_and_existing_names_and_bad_names_before_touching_disk() {
    let root = tempfile::tempdir().unwrap();
    let (mut r, files) = rig_in(root.path());
    // No draft: a notice in the log, nothing on disk.
    line(&mut r, &files, "/workflow save review");
    assert!(!root.path().join("project").exists());
    assert!(log_has(&r, "no drafted workflow"));
    // A draft naming an agent that does not exist is shown with its step, and not written.
    draft(
        &mut r,
        &REVIEW.replace("agent = \"plan\"", "agent = \"ghost\""),
    );
    line(&mut r, &files, "/workflow save review");
    assert!(!root.path().join("project").exists());
    assert_eq!(modal(&r).as_deref(), Some("workflow_show"));
    assert!(log_has(&r, "step 2"), "{}", r.text());
    assert!(log_has(&r, "ghost"));
    r.key(K::Esc);
    // An existing name is refused without --force, replaced with it.
    draft(&mut r, REVIEW);
    line(&mut r, &files, "/workflow save review");
    let path = root.path().join("project/.aibus/workflows/review.toml");
    std::fs::write(&path, "# mine\nname = \"review\"\n").unwrap();
    line(&mut r, &files, "/workflow save review");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# mine\nname = \"review\"\n"
    );
    line(&mut r, &files, "/workflow save review --force");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .starts_with("# schema: aibus://schema/workflow v1\n"));
    // A bad name is a notice on the prompt and never reaches the loop.
    let before = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
    r.type_text("/workflow save ../escape");
    let msgs = r.key(K::Enter);
    assert!(
        msgs.iter()
            .all(|m| !matches!(m, Msg::Command(Command::WorkflowFile(_)))),
        "{msgs:?}"
    );
    assert!(r
        .ui
        .prompt
        .notice
        .as_deref()
        .is_some_and(|n| n.contains("not a workflow name")));
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        before
    );
    assert!(!root.path().join("project/.aibus/escape.toml").exists());
}

#[test]
fn list_and_show_render_and_enter_fills_the_prompt() {
    let root = tempfile::tempdir().unwrap();
    let (mut r, files) = rig_in(root.path());
    let dir = root.path().join("project/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("review.toml"), REVIEW).unwrap();
    std::fs::write(dir.join("broken.toml"), "name = [").unwrap();
    line(&mut r, &files, "/workflow list");
    assert_eq!(modal(&r).as_deref(), Some("workflows"));
    assert_frame!(r.frame(), "workflow_list");
    // Rows are sorted by name: broken, review. Enter on the broken one says so and stays.
    r.key(K::Enter);
    assert_eq!(modal(&r).as_deref(), Some("workflows"));
    r.key(K::Down);
    r.key(K::Enter);
    assert_eq!(modal(&r), None);
    assert_eq!(r.ui.prompt.text, "/workflow run review ");
    // `line` types, so the filled prompt has to be cleared first.
    r.ui.prompt.set("");
    line(&mut r, &files, "/workflow show review");
    assert_eq!(modal(&r).as_deref(), Some("workflow_show"));
    assert!(r.text().contains("valid, 2 steps"));
    assert_frame!(r.frame(), "workflow_show");
}

#[test]
fn a_row_can_be_clicked_through_to_run() {
    let root = tempfile::tempdir().unwrap();
    let (mut r, files) = rig_in(root.path());
    let dir = root.path().join("project/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("review.toml"), REVIEW).unwrap();
    line(&mut r, &files, "/workflow list");
    r.click(HitId::Button("wf_run"));
    assert_eq!(r.ui.prompt.text, "/workflow run review ");
}

#[test]
fn run_is_the_existing_workflow_command() {
    let root = tempfile::tempdir().unwrap();
    let (mut r, _) = rig_in(root.path());
    r.type_text("/workflow run review the diff");
    let a = names(&r.key(K::Enter));
    r.type_text("/workflow review the diff");
    let b = names(&r.key(K::Enter));
    assert_eq!(a, b);
    assert!(a
        .iter()
        .any(|m| m.contains("Workflow { name: \"review\", input: \"the diff\" }")));
}
