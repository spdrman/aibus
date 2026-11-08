//! The aibus binary: wiring for the TUI and headless modes.
#![forbid(unsafe_code)]

#[allow(dead_code)] // the engine wiring in A2.4 is its first caller
mod providers;

fn main() {
    println!("aibus {}", env!("CARGO_PKG_VERSION"));
}
