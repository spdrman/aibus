//! ratatui widgets, view model derivations, key handling and the mouse model.
#![forbid(unsafe_code)]

pub mod app;
pub mod commands;
pub mod demo;
pub mod diagram;
pub mod glyphs;
pub mod hit;
pub mod input;
pub mod keys;
pub mod layout;
pub mod prompt;
pub mod remote;
pub mod screens;
pub mod theme;
pub mod transcript;
pub mod ui;
pub mod view;
pub mod views;
pub mod widgets;
pub mod workflows;

pub use layout::{areas, Areas, Layout};
pub use theme::Theme;
pub use ui::{render, Chrome, Drawn, Pointer, Prompt, Scene, Ui, Views};
pub use view::register;

/// Replays `msgs.jsonl` (10 section 5) on `graph` with the dashboard's views registered and
/// returns every `view:` node by key. `views.json` of a replay fixture holds this, and the
/// recorder and the replay test both call it so they cannot drift.
pub fn replay_views<G: aibus_graph::Graph>(
    graph: G,
    jsonl: &str,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
    let mut app = aibus_core::App::new(graph, || 0);
    let keys = register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    aibus_core::replay::replay_into(&mut app, jsonl)?;
    Ok(app
        .keys
        .iter()
        .filter(|k| k.starts_with("view:"))
        .map(|k| {
            let v = app.graph.read_key(k).map_or(serde_json::Value::Null, |v| {
                serde_json::to_value(v).expect("a view value serialises")
            });
            (k.to_string(), v)
        })
        .collect())
}
