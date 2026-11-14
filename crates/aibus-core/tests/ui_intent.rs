//! `Msg::Ui` goes through `reduce` like any other message (05 section 6.1).

mod common;

use aibus_core::state::value::{as_i64, as_str, get};
use aibus_core::*;
use aibus_graph::Graph;
use common::*;

fn ui(a: &mut A, i: UiIntent) {
    a.step(Msg::Ui(i)).unwrap();
}

fn focus(a: &A) -> String {
    as_str(a.graph.read_key("ctrl:focus").unwrap())
        .unwrap()
        .to_owned()
}

#[test]
fn tab_cycles_prompt_cards_log_rail_prompt_and_back() {
    let mut a = app();
    let mut seen = vec![focus(&a)];
    for _ in 0..4 {
        ui(&mut a, UiIntent::FocusNext);
        seen.push(focus(&a));
    }
    assert_eq!(seen, ["input", "cards", "log", "advisor", "input"]);
    ui(&mut a, UiIntent::FocusPrev);
    assert_eq!(focus(&a), "advisor");
}

#[test]
fn select_and_scroll_match_the_commands() {
    let mut a = app();
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Index(2)));
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:selected:card").unwrap()),
        Some(2)
    );
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Prev));
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:selected:card").unwrap()),
        Some(1)
    );
    // The log has no rows, so it cannot be scrolled; the card row can.
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Log,
            lines: 3,
            sideways: false,
        },
    );
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()),
        Some(0)
    );
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Cards,
            lines: 2,
            sideways: true,
        },
    );
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:cards:x").unwrap()),
        Some(2)
    );
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Cards,
            lines: -9,
            sideways: true,
        },
    );
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:cards:x").unwrap()),
        Some(0)
    );
}

#[test]
fn the_log_scrolls_no_further_than_its_oldest_row() {
    let mut a = app();
    for i in 0..5 {
        a.step(Msg::IoError {
            context: format!("c{i}"),
            error: "e".into(),
        })
        .unwrap();
    }
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Log,
            lines: 99,
            sideways: false,
        },
    );
    let rows = a.store.log_len() as i64;
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()),
        Some(rows - 1)
    );
}

#[test]
fn open_on_cards_names_the_selected_workers_transcript() {
    let mut a = app();
    for (seq, role) in [(1, Role::Lead), (2, Role::Worker), (3, Role::Worker)] {
        a.step(Msg::RunStarted {
            run: rid(seq),
            agent: "search".parse().unwrap(),
            role,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("acct".into()),
        })
        .unwrap();
    }
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Index(1)));
    ui(&mut a, UiIntent::Open(Pane::Cards));
    let modal = as_str(a.graph.read_key("ctrl:modal").unwrap())
        .unwrap()
        .to_owned();
    assert_eq!(modal, format!("transcript:{}", rid(3)));
}

#[test]
fn rail_toggle_and_dragged_sizes_are_kept_and_clamped() {
    let mut a = app();
    ui(&mut a, UiIntent::ToggleRail);
    assert_eq!(
        a.graph.read_key("ctrl:rail:expanded"),
        Some(&aibus_graph::Value::Bool(true))
    );
    ui(&mut a, UiIntent::ToggleRail);
    assert_eq!(
        a.graph.read_key("ctrl:rail:expanded"),
        Some(&aibus_graph::Value::Bool(false))
    );
    ui(&mut a, UiIntent::SetRailWidth(3));
    ui(&mut a, UiIntent::SetLogRows(500));
    let layout = a.graph.read_key("ctrl:layout").unwrap();
    assert_eq!(as_i64(get(layout, "rail").unwrap()), Some(22));
    assert_eq!(as_i64(get(layout, "log").unwrap()), Some(80));
}

#[test]
fn a_ui_message_summary_names_the_intent_and_nothing_else() {
    let m = Msg::Ui(UiIntent::Open(Pane::Log));
    assert_eq!(m.name(), "Ui");
    let text = format!("{:?}", m.summary());
    assert!(text.contains("open"), "{text}");
}

fn bool_at(a: &A, key: &str) -> bool {
    matches!(a.graph.read_key(key), Some(aibus_graph::Value::Bool(true)))
}

#[test]
fn scrolling_away_from_the_newest_row_turns_follow_off_and_f_turns_it_back_on() {
    let mut a = app();
    for i in 0..6 {
        a.step(Msg::IoError {
            context: format!("c{i}"),
            error: "e".into(),
        })
        .unwrap();
    }
    assert!(bool_at(&a, "ctrl:log:follow"), "follow is on by default");
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Log,
            lines: 2,
            sideways: false,
        },
    );
    assert!(!bool_at(&a, "ctrl:log:follow"));
    // A row that arrives while the log holds its place pushes the held row up with it.
    a.step(Msg::IoError {
        context: "late".into(),
        error: "e".into(),
    })
    .unwrap();
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()),
        Some(3)
    );
    ui(&mut a, UiIntent::ToggleFollow);
    assert!(bool_at(&a, "ctrl:log:follow"));
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()),
        Some(0)
    );
    ui(&mut a, UiIntent::ToggleFollow);
    assert!(!bool_at(&a, "ctrl:log:follow"));
    // Scrolling back down to the newest row follows again.
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Log,
            lines: 1,
            sideways: false,
        },
    );
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Log,
            lines: -1,
            sideways: false,
        },
    );
    assert!(bool_at(&a, "ctrl:log:follow"));
}

#[test]
fn the_log_filter_is_a_command_and_scrolls_back_to_the_newest_row() {
    let mut a = app();
    a.step(Msg::IoError {
        context: "c".into(),
        error: "e".into(),
    })
    .unwrap();
    ui(
        &mut a,
        UiIntent::Scroll {
            pane: Pane::Log,
            lines: 1,
            sideways: false,
        },
    );
    a.step(Msg::Command(Command::LogFilter("grep".into())))
        .unwrap();
    assert_eq!(
        as_str(a.graph.read_key("ctrl:log:filter").unwrap()),
        Some("grep")
    );
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()),
        Some(0)
    );
    assert_eq!(
        command_name(&Command::LogFilter(String::new())),
        "log_filter"
    );
}

#[test]
fn the_context_menu_opens_for_a_run_moves_its_selection_and_closes() {
    let mut a = app();
    ui(
        &mut a,
        UiIntent::OpenMenu {
            run: rid(7),
            col: 12,
            row: 9,
        },
    );
    let m = a.graph.read_key("ctrl:menu").unwrap().clone();
    assert_eq!(
        as_str(get(&m, "run").unwrap()),
        Some(rid(7).to_string().as_str())
    );
    assert_eq!(
        (
            as_i64(get(&m, "col").unwrap()),
            as_i64(get(&m, "row").unwrap()),
            as_i64(get(&m, "selected").unwrap())
        ),
        (Some(12), Some(9), Some(0))
    );
    ui(&mut a, UiIntent::MenuSelect(3));
    assert_eq!(
        as_i64(get(a.graph.read_key("ctrl:menu").unwrap(), "selected").unwrap()),
        Some(3)
    );
    ui(&mut a, UiIntent::CloseMenu);
    assert_eq!(
        a.graph.read_key("ctrl:menu"),
        Some(&aibus_graph::Value::Null)
    );
    // Moving the selection of a menu that is not open opens nothing.
    ui(&mut a, UiIntent::MenuSelect(2));
    assert_eq!(
        a.graph.read_key("ctrl:menu"),
        Some(&aibus_graph::Value::Null)
    );
}

#[test]
fn memory_for_the_status_bar_and_the_advice_tick_are_kept() {
    let mut a = app();
    assert_eq!(
        a.graph.read_key("ctrl:mem"),
        Some(&aibus_graph::Value::Null)
    );
    ui(&mut a, UiIntent::Mem(212));
    assert_eq!(as_i64(a.graph.read_key("ctrl:mem").unwrap()), Some(212));
    for _ in 0..5 {
        a.step(Msg::Tick).unwrap();
    }
    a.step(Msg::AdvisorSpoke {
        checkpoint: Checkpoint::BeforePlan,
        advice: "plan skips auth".into(),
        tokens_read: 1,
    })
    .unwrap();
    let st = a.graph.read_key("model:advisor:state").unwrap();
    assert_eq!(
        as_i64(get(st, "advice_tick").unwrap()),
        Some(5),
        "the rail types from this tick on"
    );
}

#[test]
fn startup_labels_reach_the_graph_in_one_commit() {
    let mut a = app();
    a.set_session_view(Some("--model sonnet"), (Some("opus"), true), (None, false))
        .unwrap();
    assert_eq!(
        as_str(a.graph.read_key("model:session:model_flag").unwrap()),
        Some("--model sonnet")
    );
    let seats = a.graph.read_key("model:session:assist").unwrap();
    let advisor = get(seats, "advisor").unwrap();
    assert_eq!(as_str(get(advisor, "label").unwrap()), Some("opus"));
    assert_eq!(
        get(get(seats, "router").unwrap(), "enabled"),
        Some(&aibus_graph::Value::Bool(false))
    );
}

#[test]
fn log_rows_carry_the_actor_the_run_and_for_the_router_p_and_the_verdict() {
    let mut a = app();
    a.step(Msg::RunStarted {
        run: rid(1),
        agent: "search".parse().unwrap(),
        role: Role::Worker,
        parent: None,
        model: ModelId("m".into()),
        account: AccountName("acct".into()),
    })
    .unwrap();
    a.step(Msg::RouterAnswered {
        fork: ForkId(1),
        question: RouterQuestion {
            kind: "which_file".into(),
            context: String::new(),
            choices: Vec::new(),
        },
        answer: RouterAnswer {
            choice: "src/auth.ts".into(),
            p: 0.94,
            verdict: Verdict::Sharp,
            latency: std::time::Duration::ZERO,
            source: PSource::Logprob,
        },
    })
    .unwrap();
    let rows = a.store.log_rows(2);
    let text = |r: &aibus_graph::Value, k: &str| get(r, k).and_then(as_str).map(str::to_owned);
    assert_eq!(text(&rows[0], "actor").as_deref(), Some("search"));
    assert_eq!(text(&rows[0], "run"), Some(rid(1).to_string()));
    assert_eq!(text(&rows[1], "actor").as_deref(), Some("router"));
    assert_eq!(
        text(&rows[1], "text").as_deref(),
        Some("which file → src/auth.ts")
    );
    assert_eq!(text(&rows[1], "verdict").as_deref(), Some("sharp"));
    assert!(
        matches!(get(&rows[1], "p"), Some(aibus_graph::Value::F64(p)) if (*p - 0.94).abs() < 1e-9)
    );
}

#[test]
fn cancel_on_a_finished_run_dismisses_its_card_and_the_indexes_follow_the_cards() {
    let mut a = app();
    for (seq, agent) in [(1, "search"), (2, "grep"), (3, "audit")] {
        a.step(Msg::RunStarted {
            run: rid(seq),
            agent: agent.parse().unwrap(),
            role: Role::Worker,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("acct".into()),
        })
        .unwrap();
    }
    a.step(Msg::RunFailed {
        run: rid(1),
        error: RunError {
            kind: RunErrorKind::Tool,
            message: "e".into(),
            retryable: false,
        },
    })
    .unwrap();
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Index(2)));
    // The run is over, so `x` dismisses it instead of asking it to stop.
    a.step(Msg::Command(Command::Cancel { run: rid(1) }))
        .unwrap();
    let meta = a
        .graph
        .read_key(&format!("model:run:{}:meta", rid(1)))
        .unwrap()
        .clone();
    assert_eq!(
        get(&meta, "dismissed"),
        Some(&aibus_graph::Value::Bool(true))
    );
    assert_eq!(
        a.graph
            .read_key(&format!("model:run:{}:cancel_requested", rid(1))),
        Some(&aibus_graph::Value::Bool(false)),
        "nothing was asked to stop"
    );
    // Two cards are left, so the selection moved up onto one of them, and card 0 is the second run.
    assert_eq!(
        as_i64(a.graph.read_key("ctrl:selected:card").unwrap()),
        Some(1)
    );
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Index(0)));
    ui(&mut a, UiIntent::Open(Pane::Cards));
    let modal = as_str(a.graph.read_key("ctrl:modal").unwrap())
        .unwrap()
        .to_owned();
    assert_eq!(modal, format!("transcript:{}", rid(2)));
}

#[test]
fn a_finished_card_gives_way_to_a_later_run_of_the_same_agent() {
    let mut a = app();
    for seq in [1, 2] {
        a.step(Msg::RunStarted {
            run: rid(seq),
            agent: "audit".parse().unwrap(),
            role: Role::Worker,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("acct".into()),
        })
        .unwrap();
        if seq == 1 {
            a.step(Msg::RunFailed {
                run: rid(1),
                error: RunError {
                    kind: RunErrorKind::Tool,
                    message: "e".into(),
                    retryable: false,
                },
            })
            .unwrap();
        }
    }
    ui(&mut a, UiIntent::Open(Pane::Cards));
    let modal = as_str(a.graph.read_key("ctrl:modal").unwrap())
        .unwrap()
        .to_owned();
    assert_eq!(
        modal,
        format!("transcript:{}", rid(2)),
        "card 0 is the newer run"
    );
}
