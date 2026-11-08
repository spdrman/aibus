//! ratatui widgets, view model derivations and key handling.
#![forbid(unsafe_code)]

pub mod app;
pub mod theme;
pub mod ui;
pub mod view;
pub mod widgets;

pub use theme::Theme;
pub use ui::{areas, render, Ui, Views};
pub use view::register;
