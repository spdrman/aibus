//! The `tracing` subscriber: the rotating log file (never the terminal) through the redactor, and
//! the Chrome trace writer when `--trace` asks for one. `-v`, `-vv`, `-q` and `AIBUS_LOG` pick
//! what reaches the log file; the trace spans (target `aibus_trace`) never do.

use std::time::SystemTime;

use aibus_core::persist::fs::create_private_dir;
use aibus_core::persist::paths::Paths;
use aibus_core::Redactor;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::cli::Global;
use crate::logfile::{RotatingLog, MAX_BYTES};
use crate::trace::{self, TraceGuard, TARGET};

/// The file's filter: the level `-q`, `-v` and `-vv` give, then `AIBUS_LOG`'s comma separated
/// directives on top (`debug`, or `aibus_core=debug`). Directives that do not parse are
/// returned so they can be logged once the subscriber exists.
pub(crate) fn filter(g: &Global, directives: Option<&str>) -> (Targets, Vec<String>) {
    let base = match (g.quiet, g.verbose) {
        (true, _) => LevelFilter::ERROR,
        (_, 0) => LevelFilter::WARN,
        (_, 1) => LevelFilter::INFO,
        (_, 2) => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let mut targets = Targets::new().with_default(base);
    let mut bad = Vec::new();
    for d in directives
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        match d.split_once('=') {
            Some((target, level)) => match level.trim().parse::<LevelFilter>() {
                Ok(l) => targets = targets.with_target(target.trim(), l),
                Err(_) => bad.push(d.to_owned()),
            },
            None => match d.parse::<LevelFilter>() {
                Ok(l) => targets = targets.with_default(l),
                Err(_) => bad.push(d.to_owned()),
            },
        }
    }
    // The spans for `--trace` are not log lines, whatever the level.
    (targets.with_target(TARGET, LevelFilter::OFF), bad)
}

/// Installs the subscriber. A second call in one process (the tests) keeps the first. The guard
/// closes the trace file when it is dropped.
pub fn init(
    g: &Global,
    paths: &Paths,
    keep: u32,
    redactor: &Redactor,
    directives: Option<&str>,
) -> Option<TraceGuard> {
    let (targets, bad) = filter(g, directives);
    let dir = paths.state.join("logs");
    let file = create_private_dir(&dir)
        .and_then(|()| RotatingLog::open(&dir, keep, MAX_BYTES, SystemTime::now));
    let log = match file {
        Ok(w) => Some(aibus_core::redact::layer(redactor.clone(), w).with_filter(targets)),
        Err(e) => {
            eprintln!("aibus: warning: no log file in {}: {e}", dir.display());
            None
        }
    };
    let (spans, guard) = match g.trace.as_deref().map(trace::create) {
        Some(Ok((layer, guard))) => (
            Some(layer.with_filter(Targets::new().with_target(TARGET, LevelFilter::TRACE))),
            Some(guard),
        ),
        Some(Err(e)) => {
            eprintln!("aibus: warning: --trace: {e}");
            (None, None)
        }
        None => (None, None),
    };
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(log).with(spans),
    );
    for d in bad {
        tracing::warn!("AIBUS_LOG: ignoring directive {d:?}");
    }
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    fn global(verbose: u8, quiet: bool) -> Global {
        Global {
            verbose,
            quiet,
            ..Global::default()
        }
    }

    #[test]
    fn flags_pick_the_level() {
        let on = |g: &Global, l| filter(g, None).0.would_enable("aibus_core", &l);
        assert!(on(&global(0, false), Level::WARN));
        assert!(!on(&global(0, false), Level::INFO));
        assert!(on(&global(1, false), Level::INFO));
        assert!(!on(&global(1, false), Level::DEBUG));
        assert!(on(&global(2, false), Level::DEBUG));
        assert!(on(&global(3, false), Level::TRACE));
        assert!(on(&global(2, true), Level::ERROR));
        assert!(!on(&global(2, true), Level::WARN));
    }

    #[test]
    fn aibus_log_adds_directives_and_bad_ones_are_reported() {
        let (t, bad) = filter(
            &global(0, false),
            Some("aibus_core=debug, hyper=off ,loud, x=nope"),
        );
        assert!(t.would_enable("aibus_core", &Level::DEBUG));
        assert!(t.would_enable("aibus", &Level::WARN));
        assert!(!t.would_enable("aibus", &Level::INFO));
        assert!(!t.would_enable("hyper", &Level::ERROR));
        assert_eq!(bad, ["loud", "x=nope"]);
        // A bare level moves the default.
        let (t, bad) = filter(&global(0, false), Some("trace"));
        assert!(bad.is_empty());
        assert!(t.would_enable("anything", &Level::TRACE));
    }

    #[test]
    fn the_trace_spans_never_reach_the_log() {
        let (t, _) = filter(&global(3, false), Some("trace"));
        assert!(!t.would_enable(TARGET, &Level::TRACE));
        assert!(!t.would_enable(TARGET, &Level::ERROR));
    }
}
