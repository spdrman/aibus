//! Source-level rules: no colour literals outside the theme, no widget reads a `model:` node.

use std::fs;
use std::path::Path;

fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            out.push((name, fs::read_to_string(p).unwrap()));
        }
    }
    out
}

fn code_lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines().filter(|l| !l.trim_start().starts_with("//"))
}

#[test]
fn no_colour_literals_outside_the_theme() {
    let banned = [
        "Color::",
        "Rgb(",
        "Indexed(",
        ".red()",
        ".green()",
        ".blue()",
        ".yellow()",
        ".white()",
        ".black()",
        ".cyan()",
        ".magenta()",
        ".gray()",
        ".on_",
    ];
    for (name, src) in sources() {
        if name == "theme.rs" {
            continue;
        }
        for line in code_lines(&src) {
            for b in banned {
                assert!(
                    !line.contains(b),
                    "{name}: colour literal `{b}` in `{line}`"
                );
            }
            let hex = line.match_indices('#').any(|(i, _)| {
                line.get(i + 1..i + 7)
                    .is_some_and(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
            });
            assert!(!hex, "{name}: hex colour in `{line}`");
        }
    }
}

#[test]
fn widgets_cannot_reach_the_graph() {
    for (name, src) in sources() {
        match name.as_str() {
            "widgets.rs" => {
                for banned in [
                    "model:", "ctrl:", "eng:", "Graph", "Tracker", "read_key", "mini::",
                ] {
                    assert!(
                        !code_lines(&src).any(|l| l.contains(banned)),
                        "widgets.rs mentions {banned}"
                    );
                }
            }
            "ui.rs" => {
                assert!(!src.contains("\"model:"), "ui.rs names a model: node");
                assert!(!src.contains("state::run_key"), "ui.rs reads run nodes");
            }
            _ => {}
        }
    }
}

/// There is one way to draw the centre column, so no source file gates on a feature.
#[test]
fn no_feature_gate_left() {
    for (name, src) in sources() {
        for banned in ["flow-diagram", "cfg(feature", "cfg!(feature"] {
            assert!(!src.contains(banned), "{name} still mentions `{banned}`");
        }
    }
}

/// rataflow is a 0.1 crate with one maintainer: one file knows it, so swapping it out is one file.
#[test]
fn rataflow_is_named_only_in_diagram_rs() {
    for (name, src) in sources() {
        if name == "diagram.rs" {
            continue;
        }
        assert!(
            !code_lines(&src).any(|l| l.contains("rataflow")),
            "{name} names rataflow, only diagram.rs may"
        );
    }
}
