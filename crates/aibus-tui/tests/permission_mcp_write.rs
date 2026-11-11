//! G8: the permission modal for a workflow write by the author. It shows the one-line summary
//! the gate made, and it does not offer "always": the button is not drawn, and the key does
//! nothing.

mod common;

use aibus_core::*;
use aibus_testkit::assert_frame;
use aibus_testkit::tui::run_id;
use aibus_tui::hit::HitId;
use aibus_tui::Theme;
use common::*;

fn write_ask() -> Rig {
    let mut app = session();
    app.step(Msg::PermissionNeeded {
        request: PermissionRequest {
            id: PermissionId::new(),
            agent: "workflow-author".parse().unwrap(),
            run: run_id(1),
            call: call_id(7),
            tool: "mcp__aibus-write__workflow_create".into(),
            summary: "mcp__aibus-write__workflow_create: create review (project), 4 steps, no gate".into(),
        },
    })
    .unwrap();
    Rig::new(app, (98, 46), Theme::recording())
}

#[test]
fn the_modal_summarises_the_write_and_hides_always() {
    let mut r = write_ask();
    assert_eq!(modal(&r).as_deref(), Some("permission"));
    let text = r.text();
    assert!(text.contains("create review (project), 4 steps, no gate"), "{text}");
    assert!(text.contains("Allow once") && text.contains("Deny"), "{text}");
    assert!(!text.contains("Allow always"), "{text}");
    assert!(r.ui.drawn.hits.find(HitId::Button("always")).is_none());
    assert_frame!(r.frame(), "permission_mcp_write");

    // The key is not a way around the missing button.
    let msgs = r.press('a');
    assert!(names(&msgs).iter().all(|m| !m.contains("AllowSession")), "{msgs:?}");
    // The other answers still work.
    let allowed = r.press('y');
    assert!(names(&allowed).iter().any(|m| m.contains("Allow")), "{allowed:?}");
}

#[test]
fn an_ordinary_tool_keeps_its_always_button() {
    let mut app = session();
    ask(&mut app, 1, "lead", 7, "bash: ls");
    let r = Rig::new(app, (98, 46), Theme::recording());
    assert!(r.text().contains("Allow always"));
    assert!(r.ui.drawn.hits.find(HitId::Button("always")).is_some());
}
