//! Resident memory, and the live numbers of 08 section 5 that `aibus doctor --session` prints.

use aibus_graph::Graph;
use serde_json::{json, Value};

use crate::app::App;

/// How often `eng:mem` is sampled (08 section 5).
pub const SAMPLE_SECS: u64 = 10;

/// Resident memory in MiB: `/proc` where there is one, else `ps`. The TUI samples it for the
/// status bar under `-v` and a headless run writes it into `memory.json`, so it lives here.
pub fn rss_mib() -> Option<u32> {
    if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        return u32::try_from(pages * 4096 / (1024 * 1024)).ok();
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    u32::try_from(kib / 1024).ok()
}

/// The numbers the 08 section 5 table is about, as `doctor --session` prints them: resident
/// memory, what the store holds, the rings and the wrapped-line cache.
pub fn numbers<G: Graph>(app: &App<G>) -> Value {
    let s = app.store.stats();
    json!({
        "rss_mb": rss_mib(),
        "store_kib": s.content_bytes / 1024,
        "transcripts": s.transcripts,
        "log_rows": s.log_rows,
        "log_cap": crate::state::store::LOG_RING,
        "commits": app.graph.commit_log().len(),
        "commit_cap": aibus_graph::COMMIT_HISTORY_CAP,
        "wrapped_blocks": s.wrap_blocks,
        "wrap_width": s.wrap_width,
    })
}

#[cfg(test)]
mod tests {
    use aibus_graph::mini::MiniGraph;

    use super::*;

    #[test]
    fn memory_is_read_in_mib() {
        // This test process is some megabytes resident, on `/proc` and under `ps` alike.
        assert!(rss_mib().is_some_and(|m| m > 0));
    }

    #[test]
    fn the_numbers_name_every_cap() {
        let app = App::new(MiniGraph::new(), || 0);
        let n = numbers(&app);
        assert_eq!(n["log_cap"], 2000);
        assert_eq!(n["commit_cap"], 4096);
        assert_eq!(n["log_rows"], 0);
        assert!(n["rss_mb"].as_u64().is_some_and(|m| m > 0));
    }
}
