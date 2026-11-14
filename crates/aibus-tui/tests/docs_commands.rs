//! G10: the command table is written once, in the registry, and copied nowhere by hand. The README
//! and `docs/spec/05-ui.md` each carry the table `commands::table_markdown()` renders between two
//! marker comments. A change to the registry without the docs fails with the diff; run with
//! `AIBUS_UPDATE_DOCS=1` to rewrite the blocks (in a container, like every other run), the same
//! shape as the snapshots and the golden transcripts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aibus_tui::commands::{help_lines, table_markdown, REGISTRY};

const START: &str = "<!-- aibus:commands:start -->";
const END: &str = "<!-- aibus:commands:end -->";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `text` with whatever sits between the markers replaced by `table`.
fn splice(text: &str, table: &str) -> String {
    let start = text.find(START).expect("the start marker is in the file");
    let end = text.find(END).expect("the end marker is in the file");
    assert!(start < end, "the start marker comes first");
    format!("{}{START}\n{table}{}", &text[..start], &text[end..])
}

/// The lines only one side has, `-` for the file and `+` for the registry.
fn diff(file: &str, want: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (file.lines().collect(), want.lines().collect());
    let mut out = String::new();
    for l in a.iter().filter(|l| !b.contains(l)) {
        out.push_str(&format!("- {l}\n"));
    }
    for l in b.iter().filter(|l| !a.contains(l)) {
        out.push_str(&format!("+ {l}\n"));
    }
    out
}

fn check(rel: &str) {
    let path = root().join(rel);
    let have = std::fs::read_to_string(&path).unwrap();
    let want = splice(&have, &table_markdown());
    if have == want {
        return;
    }
    if std::env::var("AIBUS_UPDATE_DOCS").as_deref() == Ok("1") {
        std::fs::write(&path, want).unwrap();
        return;
    }
    panic!(
        "{rel} does not match the command registry (AIBUS_UPDATE_DOCS=1 rewrites it):\n{}",
        diff(&have, &want)
    );
}

#[test]
fn readme_table_matches_the_registry() {
    check("README.md");
}

#[test]
fn spec_05_table_matches_the_registry() {
    check("docs/spec/05-ui.md");
}

/// `name -> help line` for each row of the block between the markers of `rel`.
fn rows_of(rel: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(root().join(rel)).unwrap();
    let block = &text[text.find(START).unwrap()..text.find(END).unwrap()];
    block
        .lines()
        .filter_map(|l| l.strip_prefix("| `/"))
        .map(|l| {
            // Cells are split on unescaped pipes: `/name` | args | help | asks.
            let mut cells = Vec::new();
            let mut cur = String::new();
            let mut chars = l.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '\\' if chars.peek() == Some(&'|') => {
                        cur.push('|');
                        chars.next();
                    }
                    '|' => cells.push(std::mem::take(&mut cur)),
                    c => cur.push(c),
                }
            }
            let name = cells[0].trim().trim_end_matches('`').to_owned();
            (name, cells[2].trim().to_owned())
        })
        .collect()
}

/// `/help`'s lines (the registry's, which is what the completion popup and the docs are built
/// from), the README and the spec say the same thing about each command.
#[test]
fn help_spec_and_readme_share_one_help_line_per_command() {
    let help: BTreeMap<String, String> = help_lines()
        .into_iter()
        .map(|(entry, line)| {
            (
                entry
                    .trim_start_matches('/')
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned(),
                line.to_owned(),
            )
        })
        .collect();
    let slash: Vec<&str> = REGISTRY
        .iter()
        .filter(|s| s.surfaces.slash > 0)
        .map(|s| s.name)
        .collect();
    assert_eq!(help.len(), slash.len(), "one /help line per slash command");
    for rel in ["README.md", "docs/spec/05-ui.md"] {
        let rows = rows_of(rel);
        for name in &slash {
            assert_eq!(
                rows.get(*name),
                help.get(*name),
                "{rel}: /{name} has another help line than /help"
            );
        }
        assert_eq!(
            rows.len(),
            slash.len(),
            "{rel}: a row for a command the registry does not have"
        );
    }
}
