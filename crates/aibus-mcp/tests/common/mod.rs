//! A server over a throwaway home, called directly (no transport).

#![allow(dead_code)]

use aibus_core::Redactor;
use aibus_mcp::{Caps, Server};
use aibus_testkit::{xdg_temp, Xdg};

pub fn caps() -> Caps {
    Caps::default()
}

/// The server, the home it lives in, and the project directory.
pub fn server(caps: Caps, redactor: Redactor) -> (Server, Xdg) {
    let home = xdg_temp();
    let project = home.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let server = Server::new(caps, project, &home.paths, Some(home.path().join("home")), redactor)
        .expect("a server");
    (server, home)
}

/// The lines of the one audit file the server wrote.
pub fn audit_lines(home: &Xdg) -> Vec<serde_json::Value> {
    let dir = home.paths.state.join("logs/mcp");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "one file per process in {}", dir.display());
    std::fs::read_to_string(files[0].path())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
