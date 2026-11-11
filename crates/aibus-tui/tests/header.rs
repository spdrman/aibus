//! G2: the header's model label follows the graph, so a switch shows up without a restart.

use aibus_core::state::value::{as_i64, as_list, get};
use aibus_core::*;
use aibus_graph::Graph;
use aibus_testkit::tui::{render, start};

#[test]
fn the_model_label_follows_set_model() {
    let mut app = App::new(aibus_graph::mini::MiniGraph::new(), || 0);
    start(&mut app, 1, "lead", Role::Lead, "sonnet");
    assert!(render(&mut app, (98, 46), 0).text.contains("lead · sonnet"));

    app.step(Msg::Command(Command::SetModel { target: ModelTarget::Lead, model: "haiku".into() })).unwrap();
    let assist = app.graph.read_key("model:session:assist").unwrap().clone();
    let id = as_i64(get(&as_list(get(&assist, "pending").unwrap())[0], "id").unwrap()).unwrap() as u64;
    app.step(Msg::ModelSwitched {
        id,
        target: ModelTarget::Lead,
        to: Ok(Some((ModelId("haiku".into()), AccountName("acct".into())))),
    })
    .unwrap();

    let text = render(&mut app, (98, 46), 0).text;
    assert!(text.contains("lead · haiku"), "{text}");
    assert!(!text.contains("lead · sonnet"), "{text}");
}
