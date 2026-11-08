//! ratatui widgets, view model derivations, key handling and the mouse model.
#![forbid(unsafe_code)]

pub mod app;
pub mod glyphs;
pub mod hit;
pub mod input;
pub mod keys;
pub mod layout;
pub mod theme;
pub mod ui;
pub mod view;
pub mod widgets;

pub use layout::{areas, Areas, Layout};
pub use theme::Theme;
pub use ui::{render, Chrome, Drawn, Pointer, Prompt, Scene, Ui, Views};
pub use view::register;
