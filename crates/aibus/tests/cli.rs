//! The command line surface: what runs, what says "not yet", and what the config commands do.

mod common;

use std::fs;

use common::{aibus, command, stderr};
use tempfile::TempDir;

const KEY_MSG: &str = "credentials do not go in config; run: aibus account add <name> --key-stdin";

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn version_prints_and_exits_0() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), format!("aibus {}\n", env!("CARGO_PKG_VERSION")));
    let json = aibus(home.path(), &["version", "--json"]);
    assert_eq!(stdout(&json).trim(), format!("{{\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_lists_every_top_level_command() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    for c in [
        "run", "agent", "account", "session", "handoff", "ledger", "memory", "workflow",
        "config", "doctor", "completions", "version", "--permission-mode", "--trace",
    ] {
        assert!(text.contains(c), "--help does not mention {c}");
    }
}

#[test]
fn everything_else_says_not_yet_and_exits_2() {
    let home = TempDir::new().unwrap();
    let cases: &[&[&str]] = &[
        &[],
        &["a prompt for the tui"],
        &["agent", "list"],
        &["agent", "show", "explore"],
        &["session", "list"],
        &["session", "resume", "01ABC", "carry on"],
        &["handoff", "list"],
        &["ledger", "show"],
        &["memory", "recall", "auth", "-k", "3"],
        &["workflow", "list"],
        &["config", "edit"],
        &["doctor"],
        &["completions", "zsh"],
    ];
    for args in cases {
        let out = aibus(home.path(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(stderr(&out).contains("not yet"), "{args:?}: {}", stderr(&out));
    }
}

#[test]
fn an_unknown_subcommand_or_flag_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    assert_eq!(aibus(home.path(), &["frobnicate", "--x"]).status.code(), Some(2));
    assert_eq!(aibus(home.path(), &["--no-such-flag", "version"]).status.code(), Some(2));
}

#[test]
fn config_path_shows_both_files_and_honours_the_overrides() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["config", "path"]);
    let h = home.path().display();
    assert_eq!(
        stdout(&out),
        format!("user\t{h}/.config/aibus/config.toml\nproject\t{h}/proj/.aibus/settings.toml\n")
    );
    let out = command(home.path())
        .env("XDG_CONFIG_HOME", "/xdg/c")
        .args(["config", "path", "--cwd", "/some/where"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), "user\t/xdg/c/aibus/config.toml\nproject\t/some/where/.aibus/settings.toml\n");
    let out = command(home.path())
        .env("AIBUS_CONFIG", "/etc/a.toml")
        .args(["config", "path"])
        .output()
        .unwrap();
    assert!(stdout(&out).starts_with("user\t/etc/a.toml\n"));
}

#[test]
fn config_init_writes_defaults_once_and_validate_and_show_agree() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    assert_eq!(aibus(h, &["config", "init"]).status.code(), Some(0));
    let file = h.join(".config/aibus/config.toml");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("[limits]") && text.contains("max_turns = 50"));
    // A second init refuses to overwrite.
    assert_eq!(aibus(h, &["config", "init"]).status.code(), Some(1));
    assert_eq!(fs::read_to_string(&file).unwrap(), text);

    let v = aibus(h, &["config", "validate"]);
    assert_eq!((v.status.code(), stdout(&v).as_str()), (Some(0), "ok\n"));

    let shown = aibus(h, &["config", "show"]);
    assert_eq!(shown.status.code(), Some(0));
    assert!(stdout(&shown).contains("tick_hz = 30"));
    let json = aibus(h, &["config", "show", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(v["limits"]["max_turns"], 50);
    assert_eq!(v["router"]["threshold"], 0.85);
}

#[test]
fn an_empty_config_is_valid_and_project_settings_override_it() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".config/aibus")).unwrap();
    fs::write(h.join(".config/aibus/config.toml"), "").unwrap();
    assert_eq!(aibus(h, &["config", "validate"]).status.code(), Some(0));

    fs::write(
        h.join(".config/aibus/config.toml"),
        "[limits]\nmax_turns = 10\n[hooks]\nstop = [{ command = \"a.sh\" }]\n",
    )
    .unwrap();
    fs::create_dir_all(h.join("proj/.aibus")).unwrap();
    fs::write(
        h.join("proj/.aibus/settings.toml"),
        "[limits]\nmax_turns = 20\n[hooks]\nstop = [{ command = \"b.sh\" }]\n",
    )
    .unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&stdout(&aibus(h, &["config", "show", "--json"]))).unwrap();
    assert_eq!(v["limits"]["max_turns"], 20);
    assert_eq!(v["hooks"]["stop"].as_array().unwrap().len(), 2);
    assert_eq!(v["hooks"]["stop"][1]["command"], "b.sh");
}

#[test]
fn validate_exits_3_with_path_and_line_and_the_exact_key_message() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".config/aibus")).unwrap();
    let file = h.join(".config/aibus/config.toml");

    fs::write(
        &file,
        "[defaults]\nmodel = \"x\"\n\n[[account]]\nname = \"w\"\nprovider = \"anthropic\"\nauth = { key = \"sk-ant-abc\" }\n",
    )
    .unwrap();
    let out = aibus(h, &["config", "validate"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        stderr(&out),
        format!("aibus: config: {}:7: {KEY_MSG}\n", file.display())
    );

    fs::write(&file, "[limits\nmax_turns = 1\n").unwrap();
    let out = aibus(h, &["config", "validate"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("config.toml:1:"), "{}", stderr(&out));

    fs::write(&file, "[router]\nthreshold = 7.0\n").unwrap();
    let out = aibus(h, &["config", "validate"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("config.toml:2: router.threshold"), "{}", stderr(&out));
}
