//! The file side of `/runs` (#284). The reducer cannot read the trace store, so a `Command::Runs`
//! reaches the terminal loop, which calls `apply` and steps what comes back: the text of the
//! fleet, a run's graph and timeline, an inspector or a replay (`Msg::RunsLoaded`), and the screen
//! to open. The text is `aibus_trace::view`'s, the same the `aibus runs` commands print, cleaned
//! of anything a terminal would act on. Nothing here starts a process or reaches a provider.

use std::path::Path;

use aibus_core::{Command, Msg, RunsQuery, Screen};
use aibus_trace::causal::{render_reproducibility, reproducibility};
use aibus_trace::compare::{compare_runs, render, RunInput};
use aibus_trace::view::{self, Filter, Recorded};

/// What `query` shows, read from the run directories under `root`.
pub fn apply(query: &RunsQuery, root: &Path) -> Vec<Msg> {
    let open = |run: &str| -> Result<(String, Recorded), String> {
        let dir = view::resolve(root, run)?;
        let name = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let rec = Recorded::load(&dir).map_err(|e| format!("run {name}: {e}"))?;
        Ok((name, rec))
    };
    let shown = match query {
        RunsQuery::Fleet => Ok((
            "recorded runs".to_owned(),
            view::render_fleet(&view::fleet(root)),
        )),
        RunsQuery::Inspect { run, node: None } => open(run).map(|(name, rec)| {
            let mut t = view::graph(&rec, &name);
            t.push_str(&view::timeline(&rec, &Filter::default()));
            (name, t)
        }),
        RunsQuery::Inspect {
            run,
            node: Some(id),
        } => open(run).and_then(|(name, rec)| {
            view::inspect(&rec, id)
                .map(|t| (format!("{name}: {}", view::clean(id, 60)), t))
                .ok_or_else(|| {
                    format!(
                        "run {name} has no agent, tool use or attempt {}",
                        view::clean(id, 160)
                    )
                })
        }),
        RunsQuery::Replay(run) => {
            open(run).map(|(name, rec)| (format!("replay {name}"), view::replay(&rec, &name)))
        }
        RunsQuery::Compare { a, b } => open(a).and_then(|(name_a, rec_a)| {
            let (name_b, rec_b) = open(b)?;
            let c = compare_runs(
                &RunInput {
                    id: &name_a,
                    rec: &rec_a,
                    result: None,
                },
                &RunInput {
                    id: &name_b,
                    rec: &rec_b,
                    result: None,
                },
            );
            Ok((format!("compare {name_a} {name_b}"), render(&c)))
        }),
        RunsQuery::Repro(run) => open(run).map(|(name, rec)| {
            (
                format!("repro {name}"),
                render_reproducibility(&reproducibility(&name, &rec)),
            )
        }),
    };
    match shown {
        Ok((title, text)) => vec![
            Msg::RunsLoaded { title, text },
            Msg::Command(Command::OpenScreen(Screen::Runs)),
        ],
        // The reducer logs `io error in <context>`, so the sentence goes in the context.
        Err(why) => vec![Msg::IoError {
            context: format!("/runs: {why}"),
            error: String::new(),
        }],
    }
}
