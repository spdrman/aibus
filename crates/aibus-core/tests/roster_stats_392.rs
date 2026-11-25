//! H1.5: the roster suffix is made from counts and nothing else, and is off unless a caller asks.

use aibus_core::agent::discover::discover;
use aibus_core::agent::roster::{self, RECENT_RUNS};
use aibus_testkit::xdg_temp;

fn defs() -> (aibus_testkit::Xdg, Vec<aibus_core::agent::AgentDef>) {
    let dir = xdg_temp();
    let agents = dir.path().join(".aibus/agents");
    std::fs::create_dir_all(&agents).unwrap();
    for n in ["scribe", "newcomer"] {
        std::fs::write(
            agents.join(format!("{n}.md")),
            format!("---\nname: {n}\ndescription: Writes notes.\nrole: worker\n---\nWrite.\n"),
        )
        .unwrap();
    }
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let defs = found.agents.values().cloned().collect();
    (dir, defs)
}

#[test]
fn without_a_lookup_the_roster_is_the_plain_name_and_description_list() {
    let (_dir, defs) = defs();
    let text = roster::text(defs.iter());
    assert!(text.contains("- scribe: Writes notes.\n"), "{text}");
    assert!(!text.contains("last"), "{text}");
}

#[test]
fn the_suffix_is_counts_only_and_bounded() {
    let (_dir, defs) = defs();
    let text = roster::text_with(defs.iter(), |a| match a.as_str() {
        "scribe" => Some((18, 15)),
        _ => None,
    });
    assert!(
        text.contains("- scribe: Writes notes. (last 18 runs: 15 completed)\n"),
        "{text}"
    );
    assert!(
        text.contains("- newcomer: Writes notes.\n"),
        "no runs yet, no suffix: {text}"
    );

    // A table that disagrees with itself still gives a line inside the bound.
    let wild = roster::text_with(defs.iter(), |_| Some((u32::MAX, u32::MAX)));
    let line = format!("(last {RECENT_RUNS} runs: {RECENT_RUNS} completed)");
    assert!(wild.contains(&line), "{wild}");
    let none = roster::text_with(defs.iter(), |_| Some((0, 0)));
    assert!(!none.contains("last"), "{none}");
}