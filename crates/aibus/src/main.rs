//! The aibus binary: wiring for the TUI and headless modes.
#![forbid(unsafe_code)]

mod headless;
#[allow(dead_code)] // the engine wiring in A2.4 is its first caller
mod providers;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        println!("aibus {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    match headless::run(&args[1..]) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(e) => {
            eprintln!("aibus: {e}");
            std::process::ExitCode::from(e.exit_code())
        }
    }
}
