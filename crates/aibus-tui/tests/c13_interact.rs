//! C1.3: what keys and the mouse do on the prompt, the transcript, the permission modal, the
//! palette, the agents and accounts screens and the outcome question. Each mouse action is
//! tested against the key with the same meaning (05 section 6.1): the messages must be equal.

mod common;

use aibus_core::state::value::as_str;
use aibus_core::*;
use aibus_graph::{Graph, Value};
use aibus_testkit::tui::run_id;
use aibus_tui::hit::HitId;
use aibus_tui::input::{AccountOp, Clip, Job};
use aibus_tui::Theme;
use common::*;
use crossterm::event::{KeyCode as K, KeyModifiers as M, MouseEventKind as Mk};

fn rig() -> Rig {
    Rig::new(session(), (98, 46), Theme::recording())
}

fn rig_with(app: App<aibus_graph::mini::MiniGraph>) -> Rig {
    Rig::new(app, (98, 46), Theme::recording())
}

fn has(msgs: &[Msg], needle: &str) -> bool {
    names(msgs).iter().any(|m| m.contains(needle))
}

// ---- the prompt -----------------------------------------------------------------------------

#[test]
fn the_launch_command_is_dim_until_the_first_submit() {
    let mut r = rig();
    r.ui.prompt.placeholder = "aibus --model sonnet".into();
    r.settle();
    assert!(r.text().contains("aibus --model sonnet"));
    assert_eq!(r.cell_of("aibus --model sonnet").fg, Theme::recording().dim_text);
    r.press('a');
    assert!(!r.text().contains("aibus --model sonnet"));
    r.key(K::Backspace);
    assert!(r.text().contains("aibus --model sonnet"), "still nothing sent");
    r.type_text("go");
    r.key(K::Enter);
    assert!(!r.text().contains("aibus --model sonnet"), "gone for good after the first submit");
}

#[test]
fn the_cursor_blinks_on_a_two_second_period_from_the_tick_counter() {
    let mut r = rig();
    r.type_text("hi");
    let ticks_now = r.int("eng:tick").unwrap();
    // Walk a whole period from a multiple of 30 and record when the block shows.
    let to_boundary = (30 - ticks_now % 30) % 30;
    ticks(&mut r.app, to_boundary as u32);
    r.settle();
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(r.text().contains("hi\u{258c}"));
        ticks(&mut r.app, 30);
        r.settle();
    }
    assert_eq!(seen, [true, false, true, false]);
}

#[test]
fn alt_enter_grows_the_prompt_to_five_rows() {
    let mut r = rig();
    let rows_before = r.ui.drawn.areas.prompt.height;
    for i in 0..7 {
        r.type_text(&format!("line {i}"));
        r.key_with(K::Enter, M::ALT);
    }
    assert_eq!(r.ui.prompt.rows(), 5);
    assert_eq!(r.ui.drawn.areas.prompt.height, 5);
    assert_eq!(rows_before, 1);
    // The frame still holds a dashboard under the taller prompt, and the last line is in view.
    assert!(r.text().contains("session log"));
    assert!(r.text().contains("line 6"));
    assert!(r.key_with(K::Enter, M::NONE).iter().any(|m| matches!(m, Msg::SubmitPrompt { text } if text.contains("line 0\nline 1"))));
    assert_eq!(r.ui.drawn.areas.prompt.height, 1);
}

#[test]
fn up_and_down_walk_the_history_and_esc_clears() {
    let mut r = rig();
    for line in ["one", "two"] {
        r.type_text(line);
        r.key(K::Enter);
    }
    r.key(K::Up);
    assert_eq!(r.ui.prompt.text, "two");
    r.key(K::Up);
    assert_eq!(r.ui.prompt.text, "one");
    r.key(K::Down);
    assert_eq!(r.ui.prompt.text, "two");
    r.key(K::Esc);
    assert_eq!(r.ui.prompt.text, "");
}

#[test]
fn tab_opens_a_completion_popup_for_commands_and_agents() {
    let mut app = session();
    with_agents(&mut app, &["search", "grep"]);
    let mut r = rig_with(app);
    r.press('/');
    r.key(K::Tab);
    let text = r.text();
    for c in ["/fork", "/delegate", "/compact", "/clear", "/handoff", "/ledger", "/model", "/advisor"] {
        assert!(text.contains(c), "the popup lacks {c}");
    }
    for _ in 0..11 {
        r.key(K::Tab);
    }
    for c in ["/workflow", "/agents", "/accounts", "/help"] {
        assert!(r.text().contains(c), "the popup lacks {c} after walking down");
    }
    r.key(K::Tab);
    r.key(K::Tab);
    r.key(K::Enter);
    assert_eq!(r.ui.prompt.text, "/delegate ");
    r.ui.prompt.escape();
    r.type_text("@agent-gr");
    r.key(K::Tab);
    assert_eq!(r.ui.prompt.text, "@agent-grep ");
    // The popup can be clicked.
    r.ui.prompt.escape();
    r.press('/');
    r.key(K::Tab);
    r.click(HitId::PopupItem(2));
    assert_eq!(r.ui.prompt.text, "/compact ");
}

#[test]
fn every_slash_command_becomes_a_command() {
    let mut app = session();
    with_agents(&mut app, &["search"]);
    let mut r = rig_with(app);
    let mut run = |line: &str| {
        r.ui.prompt.set(line);
        let msgs = r.key(K::Enter);
        names(&msgs).join("|")
    };
    assert!(run("/fork try again").contains("SubmitPrompt { text: \"/fork try again\" }"));
    assert!(run("/compact").contains("/compact"));
    assert!(run("/delegate search map it").contains("Delegate"));
    assert_eq!(run("/handoff"), "Command(CreateHandoff { outcome: None })");
    assert!(run("/ledger now ship").contains("LedgerUpdate"));
    assert!(run("/model opus").contains("/model opus"));
    assert!(run("/advisor opus").contains("/advisor opus"));
    assert!(run("/workflow review the diff").contains("Workflow"));
    assert_eq!(run("/agents"), "Command(OpenScreen(Agents))");
    assert_eq!(modal(&r).as_deref(), Some("agents"));
    r.key(K::Esc);
    let mut r2 = rig();
    r2.ui.prompt.set("/accounts");
    assert_eq!(names(&r2.key(K::Enter)), ["Command(OpenScreen(Accounts))"]);
    r2.key(K::Esc);
    r2.ui.prompt.set("/help");
    assert_eq!(names(&r2.key(K::Enter)), ["Command(OpenHelp)"]);
}

#[test]
fn clear_asks_first() {
    let mut r = rig();
    r.ui.prompt.set("/clear");
    r.key(K::Enter);
    assert_eq!(modal(&r).as_deref(), Some("confirm:clear"));
    assert!(r.text().contains("Clear the session log?"));
    assert!(!has(&r.key(K::Esc), "Clear)"), "Esc cancels");
    r.ui.prompt.set("/clear");
    r.key(K::Enter);
    let by_key = names(&r.press('y'));
    assert!(by_key.contains(&"Command(Clear)".to_owned()));
    r.ui.prompt.set("/clear");
    r.key(K::Enter);
    let by_mouse = names(&r.click(HitId::Button("clear_yes")));
    assert_eq!(by_key, by_mouse, "keyboard parity");
}

#[test]
fn an_unknown_command_says_so_under_the_prompt_and_the_hint_row_is_for_new_users() {
    let mut r = rig();
    r.ui.prompt.set("/nope");
    assert!(r.key(K::Enter).is_empty());
    assert!(r.text().contains("no command /nope"), "{}", r.text());
    let mut r = rig();
    assert!(!r.text().contains("Alt+Enter new line"));
    r.ui.prompt.hints = true;
    r.settle();
    assert!(r.text().contains("Alt+Enter new line"));
}

#[test]
fn the_prompt_takes_click_drag_wheel_and_pasted_paths() {
    let mut r = rig();
    r.type_text("hello world");
    let (x, y) = r.find("hello world").unwrap();
    r.down(x + 6, y);
    r.up(x + 6, y);
    assert_eq!(r.ui.prompt.at(), 6, "click places the cursor");
    r.down(x, y);
    r.drag(x + 5, y);
    r.up(x + 5, y);
    assert_eq!(r.ui.prompt.selected_text().as_deref(), Some("hello"));
    // Ctrl+C with a selection copies; it is not the cancel.
    let msgs = r.key_with(K::Char('c'), M::CONTROL);
    assert!(msgs.is_empty());
    assert_eq!(r.copied, [Clip::Text("hello".into())]);
    // The wheel scrolls a long prompt.
    r.ui.prompt.set("a\nb\nc\nd\ne\nf\ng");
    r.settle();
    let (px, py) = r.centre(HitId::PromptRow(0));
    r.mouse(Mk::ScrollUp, px, py);
    assert_eq!(r.ui.prompt.pos(r.ui.prompt.at()).0, 5);
    // A dropped path arrives as pasted text: it is inserted, not run.
    r.ui.prompt.set("read ");
    let msgs = r.paste("/home/me/a file.txt\n");
    assert!(!msgs.iter().any(|m| matches!(m, Msg::SubmitPrompt { .. })));
    assert_eq!(r.ui.prompt.text, "read '/home/me/a file.txt'");
}

// ---- the permission modal -------------------------------------------------------------------

#[test]
fn a_permission_request_opens_the_modal_and_y_n_a_answer_it() {
    let mut app = session();
    ask(&mut app, 1, "lead", 7, "bash: cargo test");
    let r = rig_with(app);
    assert_eq!(modal(&r).as_deref(), Some("permission"));
    let text = r.text();
    assert!(text.contains("permission · lead (run "), "{text}");
    assert!(text.contains("cargo test") && text.contains("Allow once") && text.contains("Allow always") && text.contains("Deny"), "{text}");
    assert_eq!(r.cell_of("permission").fg, Theme::recording().warn, "the border is yellow");
    for (key, want) in [('y', "Allow"), ('n', "Deny"), ('a', "AllowSession")] {
        let mut app = session();
        ask(&mut app, 1, "lead", 7, "bash: cargo test");
        let mut r = rig_with(app);
        let msgs = names(&r.press(key));
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains(&format!("decision: {want}")), "{key}: {msgs:?}");
        // The answer reaches the reducer through the graph in the real loop; here it is stepped.
        assert_eq!(modal(&r), None, "{key}: the queue is empty, the modal is gone");
    }
}

#[test]
fn several_prompts_queue_and_a_background_run_is_named() {
    let mut app = session();
    worker(&mut app, 2, "grep", "finds call sites", Some(0.3), "running");
    ask(&mut app, 2, "grep", 8, "bash: rg foo");
    ask(&mut app, 1, "lead", 9, "bash: ls");
    let mut r = rig_with(app);
    let text = r.text();
    assert!(text.contains("1 of 2"), "{text}");
    assert!(text.contains("background run grep"), "{text}");
    assert!(r.text().contains("! 2 permission waiting (p)"));
    r.press('y');
    assert!(r.text().contains("permission · lead"), "the next prompt is up");
    assert_eq!(modal(&r).as_deref(), Some("permission"));
}

#[test]
fn esc_hides_the_modal_and_p_or_the_alert_brings_it_back() {
    let mut app = session();
    ask(&mut app, 1, "lead", 7, "bash: ls");
    let mut r = rig_with(app);
    r.key(K::Esc);
    assert_eq!(modal(&r), None);
    assert!(r.text().contains("! 1 permission waiting (p)"), "the alert stays until the queue is empty");
    // `p` is global but the prompt has the keys: focus the cards first.
    r.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
    r.settle();
    let by_key = names(&r.press('p'));
    assert_eq!(modal(&r).as_deref(), Some("permission"));
    r.key(K::Esc);
    let by_click = names(&r.click(HitId::Status(aibus_tui::hit::Segment::Alert)));
    assert_eq!(by_key, by_click, "the clickable alert does what p does");
    assert_eq!(modal(&r).as_deref(), Some("permission"));
}

#[test]
fn buttons_do_what_the_keys_do_and_an_outside_click_never_denies() {
    let table = [("allow", 'y'), ("deny", 'n'), ("always", 'a')];
    for (button, key) in table {
        let mk = || {
            let mut app = session();
            ask(&mut app, 1, "lead", 7, "bash: ls");
            rig_with(app)
        };
        let mut by_key = mk();
        let mut by_mouse = mk();
        let k = by_key.press(key);
        let m = by_mouse.click(HitId::Button(button));
        assert_eq!(unid(&names(&k)), unid(&names(&m)), "{button}");
    }
    let mut app = session();
    ask(&mut app, 1, "lead", 7, "bash: ls");
    let mut r = rig_with(app);
    let msgs = r.down(0, 0);
    r.up(0, 0);
    assert!(msgs.is_empty());
    assert_eq!(modal(&r).as_deref(), Some("permission"), "an outside click leaves the modal alone");
    // Cancel hides it without an answer, like Esc.
    r.click(HitId::Button("cancel"));
    assert_eq!(modal(&r), None);
    assert_eq!(r.app.graph.read_key("model:permissions:open").map(as_list_len), Some(1));
}

/// The random ids of a permission prompt, blanked so two sessions compare equal.
fn unid(msgs: &[String]) -> Vec<String> {
    msgs.iter()
        .map(|m| match (m.find("PermissionId("), m.find("), decision")) {
            (Some(a), Some(b)) => format!("{}PermissionId(){}", &m[..a], &m[b + 1..]),
            _ => m.clone(),
        })
        .collect()
}

fn as_list_len(v: &Value) -> usize {
    aibus_core::state::value::as_list(v).len()
}

#[test]
fn e_edits_the_input_first() {
    let mut app = session();
    ask(&mut app, 1, "lead", 7, "bash: cargo test");
    let mut r = rig_with(app);
    assert!(r.press('e').is_empty());
    assert!(r.text().contains("run instead: cargo test"), "{}", r.text());
    r.type_text(" -p core");
    let msgs = names(&r.key(K::Enter));
    assert!(msgs.iter().any(|m| m.contains("Permission") && m.contains("Deny")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("Run this instead: cargo test -p core")), "{msgs:?}");
}

// ---- the transcript -------------------------------------------------------------------------

fn tr_rig() -> Rig {
    rig_with(tool_app())
}

#[test]
fn a_collapsed_call_is_one_line_and_enter_expands_it() {
    let mut r = tr_rig();
    let text = r.text();
    assert!(text.contains("▸ bash  cargo test  (1.2 s, 4 KiB)"), "{text}");
    assert!(text.contains("⌁ compacted at 167k"), "{text}");
    assert!(text.contains("lead · sonnet · acct"), "{text}");
    r.key(K::Tab);
    r.key(K::Enter);
    let text = r.text();
    assert!(text.contains("▾ bash  cargo test"), "{text}");
    assert!(text.contains("\"command\": \"cargo test\""), "the input is shown when open: {text}");
    // The same by clicking the row.
    let mut r = tr_rig();
    let (x, y) = r.find("▸ bash").unwrap();
    r.down(x + 2, y);
    r.up(x + 2, y);
    assert!(r.text().contains("▾ bash"));
}

#[test]
fn r_x_m_c_and_capital_r_follow_the_state_of_the_run() {
    // A running lead: x stops it, m writes to it, c compacts it; r and R do nothing.
    let mut r = tr_rig();
    assert!(r.press('r').is_empty(), "r only shows on a retryable failure");
    assert!(has(&r.press('x'), "Cancel"));
    assert!(has(&r.press('c'), "Compact"));
    assert!(r.press('R').is_empty());
    assert!(r.press('m').is_empty());
    r.type_text("check the lockfile");
    let sent = r.key(K::Enter);
    assert!(has(&sent, "SendMessage") && has(&sent, "check the lockfile"));
    // A retryable failure: r retries; a fatal one does not.
    for (retryable, want) in [(true, true), (false, false)] {
        let mut app = tool_app();
        fail(&mut app, 1, retryable);
        let mut r = rig_with(app);
        assert_eq!(r.text().contains("Retry"), want, "the button only shows when it applies");
        assert_eq!(has(&r.press('r'), "Retry"), want, "retryable = {retryable}");
    }
    // A partial run: R resumes it with a message you can change.
    let mut app = tool_app();
    finish(&mut app, 1, RunStatus::Partial);
    let mut r = rig_with(app);
    assert!(r.press('R').is_empty());
    assert!(r.text().contains("resume with: continue"));
    let msgs = r.key(K::Enter);
    assert!(has(&msgs, "Resume") && has(&msgs, "continue"));
}

#[test]
fn square_brackets_step_the_view_through_graph_time() {
    let mut r = tr_rig();
    assert!(r.text().contains("Done."));
    r.press('[');
    assert!(!r.text().contains("Done."), "one change back, the last words are not there yet");
    assert!(r.text().contains("stepped back to t="));
    r.press(']');
    assert!(r.text().contains("Done.") && !r.text().contains("stepped back"));
    // Past the retained window the view says where the window starts.
    let mut app = tool_app();
    ticks(&mut app, 4200);
    let mut r = rig_with(app);
    for _ in 0..12 {
        r.press('[');
    }
    let text = r.text();
    assert!(text.contains("evicted, oldest retained t="), "{text}");
}

#[test]
fn a_finished_runs_transcript_streams_back_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = session();
    worker(&mut app, 2, "grep", "finds call sites", Some(1.0), "running");
    text_stream(&mut app, 2, "in memory");
    finish(&mut app, 2, RunStatus::Completed);
    let run = run_id(2);
    let lines = [
        r#"{"type":"message","role":"user","blocks":[{"type":"text","text":"finds call sites"}]}"#,
        r#"{"type":"message","role":"assistant","blocks":[{"type":"text","text":"Searching."},{"type":"tool_use","id":"c1","name":"grep","input":{"pattern":"fn main"}}]}"#,
        r#"{"type":"tool_result","id":"c1","is_error":false,"bytes":2048,"content":"src/main.rs:1"}"#,
        r#"{"type":"message","role":"assistant","blocks":[{"type":"text","text":"Found one call site."}]}"#,
    ];
    let path = dir.path().join(run.to_string());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("transcript.jsonl"), lines.join("\n")).unwrap();
    app.step(Msg::Command(Command::OpenTranscript(run))).unwrap();
    assert!(aibus_tui::app::evicted_open(&app.graph, &app.store).is_some(), "the content went when the run finished");
    let n = aibus_tui::app::reload_open(&mut app, dir.path()).unwrap();
    assert_eq!(n, 4);
    let r = rig_with(app);
    let text = r.text();
    for want in ["finds call sites", "Searching.", "▸ grep  fn main  (0 ms, 2 KiB)", "Found one call site."] {
        assert!(text.contains(want), "{want}: {text}");
    }
}

#[test]
fn the_lines_of_the_open_transcript_are_a_store_handle_that_moves_with_the_text() {
    let mut r = tr_rig();
    let key = format!("view:transcript:{}:lines", run_id(1));
    let handle = |r: &Rig| match r.app.graph.read_key(&key).and_then(|v| aibus_core::state::value::get(v, "lines")) {
        Some(Value::Handle(h)) => h.clone(),
        other => panic!("not a handle: {other:?}"),
    };
    let before = handle(&r);
    text_stream(&mut r.app, 1, " more");
    assert_ne!(handle(&r).version, before.version);
    assert_eq!(handle(&r).store, "lines");
    assert!(r.app.graph.read_key(&format!("view:transcript:{}:lines", run_id(2))).is_none(), "only the open run has one");
}

#[test]
fn a_stream_commit_wraps_only_the_tail_of_a_100k_line_transcript() {
    let mut app = aibus_testkit::tui::heavy_app();
    app.step(Msg::Command(Command::OpenTranscript(run_id(1)))).unwrap();
    let mut r = rig_with(app);
    assert!(r.text().contains("line 099999"), "the newest line is at the bottom");
    let before = r.app.store.wrap_calls();
    for i in 0..5 {
        text_stream(&mut r.app, 1, &format!("one more token {i}"));
        r.settle();
    }
    let used = r.app.store.wrap_calls() - before;
    assert!(used <= 5, "one wrap per commit (the tail), got {used}");
    assert!(r.text().contains("one more token 4"));
}

#[test]
fn scrolling_away_pauses_following_and_a_button_jumps_back() {
    let mut app = aibus_testkit::tui::heavy_app();
    app.step(Msg::Command(Command::OpenTranscript(run_id(1)))).unwrap();
    let mut r = rig_with(app);
    assert!(!r.text().contains("jump to latest"));
    let (x, y) = (10, 10);
    r.mouse(Mk::ScrollUp, x, y);
    assert!(r.text().contains("jump to latest"), "{}", r.text());
    assert!(!r.text().contains("line 099999"));
    // New output does not move the view while it is held.
    let shown = r.text();
    text_stream(&mut r.app, 1, "\nnew line\n");
    r.settle();
    assert_eq!(r.text(), shown);
    let by_mouse = r.click(HitId::JumpLatest);
    assert!(!r.text().contains("jump to latest"));
    assert!(r.text().contains("new line"));
    // The keyboard does the same.
    r.mouse(Mk::ScrollUp, x, y);
    let by_key = r.press('f');
    assert_eq!(names(&by_mouse), names(&by_key).iter().map(|s| s.replace("Live", "Bottom")).collect::<Vec<_>>());
    assert!(!r.text().contains("jump to latest"));
}

#[test]
fn drag_double_and_triple_click_select_and_ctrl_c_copies() {
    let mut r = tr_rig();
    let (x, y) = r.find("I'll run the tests first.").unwrap();
    // Drag across the first words.
    r.down(x, y);
    r.drag(x + 7, y);
    r.up(x + 7, y);
    assert!(r.text().contains("copy") || r.text().contains("Copy"));
    r.key_with(K::Char('c'), M::CONTROL);
    assert_eq!(r.copied, [Clip::Text("I'll run".into())]);
    // Double click: a word. Triple click: the line.
    let mut r = tr_rig();
    r.down(x + 5, y);
    r.up(x + 5, y);
    r.down(x + 5, y);
    r.up(x + 5, y);
    r.key_with(K::Char('c'), M::CONTROL);
    assert_eq!(r.copied, [Clip::Text("run".into())]);
    let mut r = tr_rig();
    for _ in 0..3 {
        r.down(x + 5, y);
        r.up(x + 5, y);
    }
    r.key_with(K::Char('c'), M::CONTROL);
    assert_eq!(r.copied, [Clip::Text("I'll run the tests first.".into())]);
    // The `copy` hint copies too, and Ctrl+C with nothing selected copies nothing.
    let mut r = tr_rig();
    r.down(x, y);
    r.drag(x + 7, y);
    r.up(x + 7, y);
    r.click(HitId::CopyHint);
    assert_eq!(r.copied, [Clip::Text("I'll run".into())]);
    let mut r = tr_rig();
    r.key_with(K::Char('c'), M::CONTROL);
    assert!(r.copied.is_empty());
}

#[test]
fn ctrl_c_with_nothing_selected_still_cancels_on_the_dashboard() {
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.2), "running");
    let mut r = rig_with(app);
    r.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
    r.settle();
    assert!(has(&r.key_with(K::Char('c'), M::CONTROL), "Cancel"));
    assert!(r.copied.is_empty());
}

#[test]
fn transcript_buttons_do_what_their_keys_do() {
    let table = [("close", K::Esc), ("older", K::Char('[')), ("stop", K::Char('x')), ("compact", K::Char('c'))];
    for (button, key) in table {
        let mut by_key = tr_rig();
        let mut by_mouse = tr_rig();
        let k = by_key.key(key);
        let m = by_mouse.click(HitId::Button(button));
        assert_eq!(names(&k), names(&m), "{button}");
        assert_eq!(modal(&by_key), modal(&by_mouse), "{button}");
    }
}

// ---- palette, agents, accounts, outcome -----------------------------------------------------

#[test]
fn the_palette_is_fuzzy_over_commands_agents_and_runs() {
    let mut app = session();
    with_agents(&mut app, &["search", "grep", "audit"]);
    app.step(Msg::Command(Command::OpenPalette)).unwrap();
    let mut r = rig_with(app);
    let text = r.text();
    for want in ["agents screen", "delegate to audit", "run lead"] {
        assert!(text.contains(want), "{want}: {text}");
    }
    r.type_text("aud");
    let text = r.text();
    assert!(text.contains("delegate to audit") && !text.contains("agents screen"), "{text}");
    let msgs = names(&r.key(K::Enter));
    assert!(msgs.iter().any(|m| m.contains("CloseModal")));
    assert_eq!(r.ui.prompt.text, "@agent-audit ");
    // A command from the palette, by key and by click.
    let open = |r: &mut Rig| {
        r.app.step(Msg::Command(Command::OpenPalette)).unwrap();
        r.settle();
    };
    open(&mut r);
    r.type_text("accounts");
    let by_key = names(&r.key(K::Enter));
    assert_eq!(modal(&r).as_deref(), Some("accounts"));
    r.key(K::Esc);
    open(&mut r);
    r.type_text("accounts");
    assert!(r.click(HitId::ListRow(0)).iter().all(|m| matches!(m, Msg::Ui(UiIntent::Pick(_)))), "a click only selects the row");
    assert!(by_key.iter().any(|m| m.contains("OpenScreen(Accounts)")));
    r.click(HitId::Button("run"));
    assert_eq!(modal(&r).as_deref(), Some("accounts"));
    // An outside click closes a dismissible modal.
    r.key(K::Esc);
    open(&mut r);
    r.down(0, 0);
    assert_eq!(modal(&r), None);
}

#[test]
fn the_agents_screen_has_n_e_v_d() {
    let mut app = session();
    with_agents(&mut app, &["search", "grep"]);
    app.step(Msg::Command(Command::OpenScreen(Screen::Agents))).unwrap();
    let mut r = rig_with(app);
    let text = r.text();
    for want in ["search", "project", "haiku", "read, grep", "You are search."] {
        assert!(text.contains(want), "{want}: {text}");
    }
    r.press('j');
    assert!(r.text().contains("You are grep."), "the preview follows the selection");
    r.press('k');
    r.press('e');
    assert_eq!(r.jobs, [Job::EditFile("/p/.aibus/agents/search.md".into())]);
    r.jobs.clear();
    r.press('v');
    assert!(matches!(&r.jobs[..], [Job::Validate { name, .. }] if name == "search"));
    r.jobs.clear();
    r.press('n');
    r.type_text("planner");
    r.key(K::Enter);
    assert_eq!(r.jobs, [Job::NewAgent("planner".into())]);
    // `d` leaves for the prompt with the delegate typed.
    r.jobs.clear();
    r.press('d');
    assert_eq!(modal(&r), None);
    assert_eq!(r.ui.prompt.text, "/delegate search ");
    // The buttons are the same actions.
    let mut app = session();
    with_agents(&mut app, &["search", "grep"]);
    app.step(Msg::Command(Command::OpenScreen(Screen::Agents))).unwrap();
    let mut r = rig_with(app);
    r.click(HitId::ListRow(1));
    r.click(HitId::Button("edit"));
    assert_eq!(r.jobs, [Job::EditFile("/p/.aibus/agents/grep.md".into())]);
}

#[test]
fn new_agents_are_written_from_a_template_and_a_bad_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut validate, mut account) = (None, None);
    let mut note = String::new();
    let opened = aibus_tui::app::run_job(Job::NewAgent("planner".into()), Some(dir.path()), &mut validate, &mut account, &mut note);
    let path = dir.path().join("planner.md");
    assert_eq!(opened.as_deref(), Some(path.as_path()));
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("---\nname: planner\n"), "{body}");
    assert!(aibus_core::agent::def::parse(&body, DefSource { path: Some(path.clone()), scope: Scope::User }).is_ok(), "the template is a valid definition");
    assert!(aibus_tui::app::run_job(Job::NewAgent("../x".into()), Some(dir.path()), &mut validate, &mut account, &mut note).is_none());
    assert!(note.contains("is not a name"));
    assert_eq!(aibus_tui::app::editor(None, Some("nano -w")), "nano -w");
    assert_eq!(aibus_tui::app::editor(Some("code"), Some("nano")), "code");
    assert_eq!(aibus_tui::app::editor(Some(" "), None), "vi");
}

#[test]
fn the_accounts_screen_has_t_a_d_and_a_key_never_shows() {
    let mut app = session();
    app.step(Msg::AccountsReloaded {
        accounts: vec![
            AccountView { name: AccountName("work".into()), provider: "anthropic".into(), label: "work".into(), default_model: None, key_ref: "keyring:aibus/work".into() },
            AccountView { name: AccountName("spare".into()), provider: "openai".into(), label: "spare".into(), default_model: None, key_ref: "none".into() },
        ],
    })
    .unwrap();
    app.step(Msg::Command(Command::OpenScreen(Screen::Accounts))).unwrap();
    let mut r = rig_with(app);
    let text = r.text();
    assert!(text.contains("work") && text.contains("anthropic") && text.contains("ready") && text.contains("no key"), "{text}");
    r.press('t');
    assert_eq!(r.jobs, [Job::Account(AccountOp::Test("work".into()))]);
    r.jobs.clear();
    r.press('j');
    r.press('d');
    assert_eq!(r.jobs, [Job::Account(AccountOp::Disable("spare".into()))]);
    r.jobs.clear();
    r.press('a');
    r.type_text("third");
    r.key(K::Enter);
    r.type_text("anthropic");
    r.key(K::Enter);
    r.type_text("sk-secret-123");
    let shown = r.text();
    assert!(!shown.contains("sk-secret-123") && shown.contains("•••••••••••••"), "the key is masked: {shown}");
    r.key(K::Enter);
    assert!(matches!(&r.jobs[..], [Job::Account(AccountOp::Add { name, provider, key })] if name == "third" && provider == "anthropic" && key == "sk-secret-123"));
    assert!(!format!("{:?}", r.jobs).contains("sk-secret-123"), "Debug hides the key");
    // The job runs through the hooks the binary gives it, and the answer is a line of text.
    let mut note = String::new();
    let mut validate = None;
    let mut account: Option<aibus_tui::app::AccountHook> = Some(Box::new(|op| format!("did {op:?}")));
    aibus_tui::app::run_job(r.jobs.remove(0), None, &mut validate, &mut account, &mut note);
    assert!(note.starts_with("did Add") && !note.contains("sk-secret"), "{note}");
}

#[test]
fn quitting_asks_how_it_went_unless_nothing_finished() {
    // Nothing completed: q quits.
    let mut r = rig();
    r.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
    r.settle();
    r.press('q');
    assert!(r.app.should_quit());
    // A completed run: q asks; the four outcomes, `!` and Esc.
    let mk = || {
        let mut app = session();
        worker(&mut app, 2, "search", "maps", Some(1.0), "completed");
        let mut r = rig_with(app);
        r.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
        r.settle();
        r.press('q');
        r
    };
    let r = mk();
    assert_eq!(modal(&r).as_deref(), Some("outcome"));
    assert!(r.text().contains("how did this session go?"));
    assert!(!r.app.should_quit());
    for (key, button, outcome) in [('1', "outcome:1", "Succeeded"), ('2', "outcome:2", "PartialPlus"), ('3', "outcome:3", "PartialMinus"), ('4', "outcome:4", "Failed")] {
        let mut a = mk();
        let mut b = mk();
        let k = names(&a.press(key));
        let m = names(&b.click(HitId::Button(button)));
        assert_eq!(k, m, "{button}");
        assert!(k[0].contains(outcome), "{k:?}");
        assert!(a.app.should_quit() && b.app.should_quit());
    }
    let mut r = mk();
    r.press('!');
    assert!(r.app.should_quit());
    let mut r = mk();
    r.key(K::Esc);
    assert!(!r.app.should_quit() && modal(&r).is_none());
    // An outside click does not close it either: the answer is wanted.
    let mut r = mk();
    r.down(0, 0);
    assert_eq!(modal(&r).as_deref(), Some("outcome"));
}

#[test]
fn only_the_tick_that_changes_the_blink_redraws_an_idle_prompt() {
    let mut r = rig();
    r.ui.animating = false;
    r.settle();
    let draws = r.ui.draws;
    ticks(&mut r.app, 20);
    r.settle();
    assert_eq!(r.ui.draws, draws, "same blink phase, nothing to draw");
    ticks(&mut r.app, 20);
    r.settle();
    assert_eq!(r.ui.draws, draws + 1, "the cursor went off");
    let _ = as_str;
}
