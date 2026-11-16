//! The helper mode of the `aibus` binary, as a binary of its own for this crate's tests.
#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        let mut args = std::env::args_os().skip(1);
        if args
            .next()
            .is_some_and(|a| a == aibus_supervisor::reaper::MARKER)
        {
            return std::process::ExitCode::from(aibus_supervisor::reaper::run(args.collect()));
        }
    }
    eprintln!("aibus-subreaper: Linux only, and only as the supervisor starts it");
    std::process::ExitCode::from(2)
}
