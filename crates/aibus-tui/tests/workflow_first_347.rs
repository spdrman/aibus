//! #347: a workflow can be the first thing a fresh session does. There is no lead until the first
//! prompt, so `/workflow run` and the bare form start it the way a prompt does (the launcher opens
//! the lead with the `workflow` call), and with no account to run it on they say what to add.

mod common;

use aibus_core::state::value::as_list;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;

/// A session with the views registered and no run at all, which is what `aibus` opens on.
fn fresh(with_account: bool) -> Rig {
    let mut app = App::new(MiniGraph::new(), || aibus_testkit::tui::NOW);
    let keys = aibus_tui::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    if with_account {
        app.step(Msg::AccountsReloaded {
            accounts: vec![AccountView {
                name: AccountName("work".into()),
                provider: "anthropic".into(),
                label: "work".into(),
                default_model: None,
                key_ref: "keyring:aibus/work".into(),
                models: vec![],
            }],
        })
        .unwrap();
    }
    Rig::new(app, (100, 40), Theme::recording())
}

fn line(r: &mut Rig, text: &str) -> Vec<Msg> {
    r.type_text(text);
    r.key(K::Enter)
}

fn runs(r: &Rig) -> usize {
    r.app
        .graph
        .read_key(aibus_core::state::RUNS_ORDER)
        .map_or(0, |v| as_list(v).len())
}

#[test]
fn run_as_the_first_line_starts_the_lead_with_the_workflow_call() {
    for text in ["/workflow run review the diff", "/workflow review the diff"] {
        let mut r = fresh(true);
        let sent = line(&mut r, text);
        assert!(
            r.ui.prompt.notice.is_none(),
            "{text}: {:?}",
            r.ui.prompt.notice
        );
        // The launcher reads `/workflow <name> <input>` as the lead's opening call.
        assert!(
            names(&sent)
                .iter()
                .any(|m| m == "SubmitPrompt { text: \"/workflow review the diff\" }"),
            "{text}: {sent:?}"
        );
        assert_eq!(runs(&r), 1, "{text}: the lead is queued");
    }
}

#[test]
fn a_name_with_no_input_still_starts_the_lead() {
    let mut r = fresh(true);
    let sent = line(&mut r, "/workflow run review");
    assert!(
        names(&sent)
            .iter()
            .any(|m| m == "SubmitPrompt { text: \"/workflow review\" }"),
        "{sent:?}"
    );
    assert_eq!(runs(&r), 1);
}

#[test]
fn no_account_says_to_add_one_and_starts_nothing() {
    let mut r = fresh(false);
    let sent = line(&mut r, "/workflow run review the diff");
    let notice = r.ui.prompt.notice.clone().unwrap_or_default();
    assert!(notice.contains("aibus account add"), "{notice:?}");
    assert!(!notice.contains("no lead is running"), "{notice:?}");
    assert!(
        sent.iter().all(|m| !matches!(m, Msg::SubmitPrompt { .. })),
        "{sent:?}"
    );
    assert_eq!(runs(&r), 0);
}

#[test]
fn new_and_edit_still_wait_for_a_lead_and_say_so() {
    let mut r = fresh(true);
    for text in [
        "/workflow new review a pull request",
        "/workflow edit review add a step",
    ] {
        let sent = line(&mut r, text);
        let notice = r.ui.prompt.notice.clone().unwrap_or_default();
        assert!(notice.contains("no lead is running"), "{text}: {notice:?}");
        assert!(sent.is_empty(), "{text}: {sent:?}");
        assert_eq!(runs(&r), 0);
    }
}
