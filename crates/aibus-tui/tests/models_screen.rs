//! G3: the `/model` picker. The rows come from what the connected accounts serve (their tables,
//! prices and capabilities), the target decides which rows are offered, and each key sends the
//! `SetModel` of the target it names. Nothing here touches a provider: the screen reads the graph.

mod common;

use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::*;
use aibus_graph::{Graph, Value};
use aibus_testkit::assert_frame;
use aibus_testkit::tui::run_id;
use aibus_tui::hit::HitId;
use aibus_tui::input::DOUBLE_CLICK;
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;

fn model(id: &str, alias: Option<&str>, price: Option<(f64, f64)>, tools: bool) -> ModelView {
    ModelView {
        id: ModelId(id.into()),
        alias: alias.map(str::to_owned),
        context_window: 200_000,
        input_per_mtok: price.map(|p| p.0),
        output_per_mtok: price.map(|p| p.1),
        tools,
        thinking: tools,
        images: false,
        cache: price.is_some(),
        logprobs: id == "gpt-x",
    }
}

fn account(name: &str, provider: &str, models: Vec<ModelView>) -> AccountView {
    AccountView {
        name: AccountName(name.into()),
        provider: provider.into(),
        label: name.into(),
        default_model: None,
        key_ref: "none".into(),
        models,
    }
}

/// The lead runs `Sonnet 5.5` on `acct`; `shaped` is an OpenAI-shaped account whose table has no
/// price for one model and no tool calling for another.
fn app() -> App<aibus_graph::mini::MiniGraph> {
    let mut app = session();
    worker(&mut app, 2, "grep", "finds call sites", Some(0.4), "running");
    app.step(Msg::AccountsReloaded {
        accounts: vec![
            account("acct", "fake", vec![model("Sonnet 5.5", Some("sonnet"), Some((3.0, 15.0)), true), model("Haiku 5.5", None, Some((0.8, 4.0)), true)]),
            account("shaped", "openai", vec![model("gpt-x", Some("fast"), None, true), model("plain-chat", None, None, false)]),
        ],
    })
    .unwrap();
    app
}

fn rig() -> Rig {
    Rig::new(app(), (98, 46), Theme::recording())
}

fn open(r: &mut Rig, target: ModelTarget) {
    r.app.step(Msg::Command(Command::OpenScreen(Screen::Models(target)))).unwrap();
    r.settle();
}

fn rows(r: &Rig) -> Vec<Value> {
    let v = r.app.graph.read_key("view:models").expect("the view is registered");
    get(v, "rows").map(as_list).unwrap_or(&[]).to_vec()
}

fn word(row: &Value, k: &str) -> String {
    get(row, k).and_then(as_str).unwrap_or("").to_owned()
}

fn refs(r: &Rig) -> Vec<String> {
    rows(r).iter().map(|row| word(row, "ref")).collect()
}

fn set_models(msgs: &[Msg]) -> Vec<(ModelTarget, String)> {
    msgs.iter()
        .filter_map(|m| match m {
            Msg::Command(Command::SetModel { target, model }) => Some((*target, model.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn models_rows_join_accounts_models_prices_and_aliases() {
    let mut r = rig();
    open(&mut r, ModelTarget::Advisor);
    let all = rows(&r);
    assert_eq!(refs(&r), ["acct/Sonnet 5.5", "acct/Haiku 5.5", "shaped/gpt-x", "shaped/plain-chat"]);
    assert_eq!((word(&all[0], "input"), word(&all[0], "output")), ("$3.00".into(), "$15.00".into()));
    assert_eq!(word(&all[1], "input"), "$0.80");
    assert_eq!((word(&all[2], "input"), word(&all[2], "output")), ("n/a".into(), "n/a".into()), "no price in the table is n/a, never a guess");
    assert_eq!(word(&all[0], "alias"), "sonnet");
    assert_eq!(word(&all[2], "alias"), "fast");
    assert_eq!(word(&all[0], "context"), "200k");
    assert_eq!(word(&all[0], "caps"), "TR-C");
    let text = r.text();
    assert!(text.contains("Sonnet 5.5") && text.contains("sonnet") && text.contains("$15.00") && text.contains("n/a"), "{text}");
}

#[test]
fn the_row_of_the_current_model_is_marked() {
    let mut r = rig();
    open(&mut r, ModelTarget::Lead);
    let current: Vec<String> = rows(&r).iter().filter(|row| matches!(get(row, "current"), Some(Value::Bool(true)))).map(|row| word(row, "ref")).collect();
    assert_eq!(current, ["acct/Sonnet 5.5"], "the lead runs Sonnet 5.5 on acct");
}

#[test]
fn models_rows_filter_by_the_targets_role() {
    let mut r = rig();
    open(&mut r, ModelTarget::Lead);
    assert!(!refs(&r).contains(&"shaped/plain-chat".to_owned()), "a lead needs tools");
    r.app.step(Msg::Command(Command::CloseModal)).unwrap();
    open(&mut r, ModelTarget::Run(run_id(2)));
    assert!(!refs(&r).contains(&"shaped/plain-chat".to_owned()), "so does a worker");
    r.app.step(Msg::Command(Command::CloseModal)).unwrap();
    open(&mut r, ModelTarget::Advisor);
    assert!(refs(&r).contains(&"shaped/plain-chat".to_owned()), "the advisor has no requirement");
    r.app.step(Msg::Command(Command::CloseModal)).unwrap();
    open(&mut r, ModelTarget::Router);
    let dimmed: Vec<String> = rows(&r).iter().filter(|row| matches!(get(row, "dim"), Some(Value::Bool(true)))).map(|row| word(row, "ref")).collect();
    assert_eq!(dimmed, ["acct/Sonnet 5.5", "acct/Haiku 5.5", "shaped/plain-chat"], "the router prefers logprobs and shows the rest dimmed");
}

#[test]
fn models_rows_text_filter_uses_the_palette_fuzzy() {
    let mut r = rig();
    open(&mut r, ModelTarget::Advisor);
    r.press('/');
    r.type_text("hku");
    assert_eq!(refs(&r), ["acct/Haiku 5.5"], "the letters match in order, as in the palette");
    r.type_text("zz");
    assert!(rows(&r).is_empty());
    assert!(r.text().contains("no model matches the filter"));
    r.key(K::Backspace);
    r.key(K::Backspace);
    r.key(K::Enter);
    assert_eq!(refs(&r), ["acct/Haiku 5.5"], "Enter keeps the filter and gives the keys back");
    r.key(K::Enter);
    assert_eq!(r.app.graph.read_key("view:models"), Some(&Value::Null), "Enter then picked the row and closed the screen");
}

#[test]
fn enter_sets_the_lead_model() {
    let mut r = rig();
    open(&mut r, ModelTarget::Lead);
    r.key(K::Down);
    let msgs = r.key(K::Enter);
    assert_eq!(set_models(&msgs), [(ModelTarget::Lead, "acct/Haiku 5.5".to_owned())]);
    assert!(rows(&r).is_empty(), "the screen closed");
    // The executor's answer is what changes the label, as for a typed /model.
    let pending = get(r.app.graph.read_key("model:session:assist").unwrap(), "pending").map(as_list).unwrap().len();
    assert_eq!(pending, 1);
}

#[test]
fn a_and_r_set_the_seats() {
    let mut r = rig();
    open(&mut r, ModelTarget::Lead);
    let msgs = r.press('a');
    assert_eq!(set_models(&msgs), [(ModelTarget::Advisor, "acct/Sonnet 5.5".to_owned())]);
    open(&mut r, ModelTarget::Lead);
    r.key(K::Down);
    let msgs = r.press('r');
    assert_eq!(set_models(&msgs), [(ModelTarget::Router, "acct/Haiku 5.5".to_owned())]);
}

#[test]
fn s_sets_only_the_targeted_run() {
    let mut r = rig();
    open(&mut r, ModelTarget::Run(run_id(2)));
    let msgs = r.press('s');
    assert_eq!(set_models(&msgs), [(ModelTarget::Run(run_id(2)), "acct/Sonnet 5.5".to_owned())]);

    // Opened for the lead there is no run to narrow it to.
    open(&mut r, ModelTarget::Lead);
    let msgs = r.press('s');
    assert!(set_models(&msgs).is_empty());
    assert!(r.text().contains("open the picker with /model @agent"), "{}", r.text());
}

#[test]
fn click_selects_and_double_click_picks() {
    let mut r = rig();
    open(&mut r, ModelTarget::Lead);
    let (c, row) = r.centre(HitId::ListRow(1));
    let msgs = r.down(c, row);
    assert!(set_models(&msgs).is_empty(), "one click only selects");
    assert_eq!(r.int("ctrl:screen:sel"), Some(1));
    r.up(c, row);
    r.advance(DOUBLE_CLICK.as_millis() as u64 / 2);
    let msgs = r.down(c, row);
    assert_eq!(set_models(&msgs), [(ModelTarget::Lead, "acct/Haiku 5.5".to_owned())]);
}

#[test]
fn the_screen_opens_with_no_request() {
    let mut r = rig();
    r.press('/');
    r.type_text("model");
    let msgs = r.key(K::Enter);
    assert!(
        matches!(&msgs[..], [Msg::SubmitPrompt { .. }] | [Msg::Command(Command::OpenScreen(Screen::Models(ModelTarget::Lead)))]),
        "/model with no reference is a screen, not a request: {msgs:?}"
    );
    assert!(r.jobs.is_empty(), "no job reaches the loop, so no provider is asked anything");
    open(&mut r, ModelTarget::Lead);
    assert!(r.jobs.is_empty());
}

#[test]
fn the_answer_changes_the_header() {
    let mut r = rig();
    open(&mut r, ModelTarget::Lead);
    r.key(K::Down);
    r.key(K::Enter);
    let assist = r.app.graph.read_key("model:session:assist").unwrap().clone();
    let id = aibus_core::state::value::as_i64(get(&as_list(get(&assist, "pending").unwrap())[0], "id").unwrap()).unwrap() as u64;
    r.app
        .step(Msg::ModelSwitched { id, target: ModelTarget::Lead, to: Ok(Some((ModelId("Haiku 5.5".into()), AccountName("acct".into())))) })
        .unwrap();
    r.settle();
    assert!(r.text().contains("lead · Haiku 5.5"), "{}", r.text());
    assert!(!r.text().contains("lead · Sonnet 5.5"), "{}", r.text());
}

// ---- frames: roomy and compact, filtered and unfiltered ---------------------------------------

fn frame(name: &str, size: (u16, u16), filter: Option<&str>) {
    let mut r = Rig::new(app(), size, Theme::recording());
    open(&mut r, ModelTarget::Lead);
    if let Some(text) = filter {
        r.press('/');
        r.type_text(text);
    }
    assert_frame!(r.frame(), name);
}

#[test]
fn models_roomy() {
    frame("g3_models_roomy", (120, 40), None);
}

#[test]
fn models_roomy_filtered() {
    frame("g3_models_roomy_filtered", (120, 40), Some("son"));
}

#[test]
fn models_compact() {
    frame("g3_models_compact", (80, 24), None);
}

#[test]
fn models_compact_filtered() {
    frame("g3_models_compact_filtered", (80, 24), Some("son"));
}
