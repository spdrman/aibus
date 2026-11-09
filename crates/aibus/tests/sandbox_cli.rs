//! C2.1: what the command line says about the shell sandbox. `doctor` names the active one and
//! fails when the configured program is missing; `--help` for `bypass` says the allow-list is
//! not a sandbox.

mod common;

use aibus_testkit::TempDir;
use common::{aibus, command};
use serde_json::Value;

fn doctor(home: &std::path::Path, config: &str, path: &std::path::Path) -> (Option<i32>, String) {
    let cfg = home.join("config.toml");
    std::fs::write(&cfg, config).unwrap();
    let out = command(home)
        .env("AIBUS_CONFIG", &cfg)
        .env("PATH", path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = v["checks"].as_array().unwrap().iter().find(|c| c["name"] == "shell sandbox").unwrap().clone();
    (out.status.code(), row["detail"].as_str().unwrap().to_owned())
}

#[test]
fn doctor_names_the_active_sandbox_and_fails_when_its_program_is_missing() {
    let home = TempDir::new().unwrap();
    let empty = home.path().join("empty-path");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("bwrap"), "#!/bin/sh\n").unwrap();

    let (code, detail) = doctor(home.path(), "", &empty);
    assert_eq!((code, detail.as_str()), (Some(0), "none"));

    let (code, detail) = doctor(home.path(), "[tools]\nsandbox = \"none\"\n", &empty);
    assert_eq!((code, detail.as_str()), (Some(0), "none"));

    let (code, detail) = doctor(home.path(), "[tools]\nsandbox = \"bwrap\"\n", &empty);
    assert_eq!(code, Some(11));
    assert_eq!(detail, "bwrap is configured but not on PATH");

    let (code, detail) = doctor(home.path(), "[tools]\nsandbox = \"bwrap\"\n", &bin);
    assert_eq!((code, detail.as_str()), (Some(0), "bwrap"));

    let (code, detail) = doctor(home.path(), "[tools]\nsandbox = \"bwrap\"\nsandbox_network = false\n", &bin);
    assert_eq!((code, detail.as_str()), (Some(0), "bwrap, network off"));
}

#[test]
fn an_unknown_sandbox_name_is_a_config_error() {
    let home = TempDir::new().unwrap();
    let cfg = home.path().join("config.toml");
    std::fs::write(&cfg, "[tools]\nsandbox = \"docker\"\n").unwrap();
    let out = command(home.path()).env("AIBUS_CONFIG", &cfg).args(["config", "show"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("sandbox"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_bypass_help_says_the_allow_list_is_not_a_sandbox() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["--help"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("process boundary inside aibus and not a sandbox"),
        "{text}"
    );
    assert!(flat.contains("bypass needs AIBUS_I_KNOW=1"), "{text}");
}
