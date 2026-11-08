//! The account commands through the real binary on the built-in fake provider. The OS keychain
//! is not touched: every key here comes from an environment variable or a file.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{aibus, command, ndjson, stderr};
use tempfile::TempDir;

const KEY: &str = "sk-cli-0123456789abcdef";

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn with_key(home: &Path, args: &[&str]) -> std::process::Output {
    command(home).env("WORK_KEY", KEY).args(args).output().unwrap()
}

fn any_file_holds(dir: &Path, needle: &str) -> bool {
    fs::read_dir(dir).unwrap().any(|e| {
        let p = e.unwrap().path();
        if p.is_dir() {
            any_file_holds(&p, needle)
        } else {
            String::from_utf8_lossy(&fs::read(&p).unwrap()).contains(needle)
        }
    })
}

#[test]
fn add_list_show_test_disable_enable_remove_with_a_key_from_the_environment() {
    let home = TempDir::new().unwrap();
    let h = home.path();

    let out = with_key(
        h,
        &["account", "add", "work", "--provider", "fake", "--env", "WORK_KEY", "--label", "Work"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let config = fs::read_to_string(h.join(".config/aibus/config.toml")).unwrap();
    assert!(config.contains("auth = { env = \"WORK_KEY\" }"), "{config}");

    // Listed as ok while the variable is set, and as missing without it.
    let list = with_key(h, &["account", "list", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(rows[0]["name"], "work");
    assert_eq!(rows[0]["status"], "ok");
    assert_eq!(rows[0]["key_ref"], "env:WORK_KEY");
    let bare = aibus(h, &["account", "list"]);
    assert!(stdout(&bare).contains("missing credential"), "{}", stdout(&bare));

    let show = with_key(h, &["account", "show", "work"]);
    assert!(stdout(&show).contains("label: Work"));

    let test = with_key(h, &["account", "test", "work", "--json"]);
    assert_eq!(test.status.code(), Some(0), "{}", stderr(&test));
    let report: serde_json::Value = serde_json::from_slice(&test.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert!(report["latency_ms"].is_u64());

    // `config show` and every file under the home: the key is nowhere.
    let shown = aibus(h, &["config", "show"]);
    assert!(!stdout(&shown).contains(KEY));
    assert!(!any_file_holds(h, KEY));

    assert_eq!(aibus(h, &["account", "disable", "work"]).status.code(), Some(0));
    let off = with_key(h, &["account", "list"]);
    assert!(stdout(&off).trim_end().ends_with("disabled"), "{}", stdout(&off));
    assert_eq!(aibus(h, &["account", "enable", "work"]).status.code(), Some(0));

    assert_eq!(aibus(h, &["account", "remove", "work"]).status.code(), Some(0));
    assert!(stdout(&with_key(h, &["account", "list"])).is_empty());
    assert_eq!(aibus(h, &["account", "show", "work"]).status.code(), Some(4));
}

#[test]
fn a_key_file_that_others_can_read_exits_4_with_the_chmod_hint() {
    let home = TempDir::new().unwrap();
    let key = home.path().join("key");
    fs::write(&key, format!("{KEY}\n")).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    let args = ["account", "add", "work", "--provider", "fake", "--file", key.to_str().unwrap()];

    let out = aibus(home.path(), &args);
    assert_eq!(out.status.code(), Some(4));
    assert!(stderr(&out).contains(&format!("chmod 600 {}", key.display())), "{}", stderr(&out));
    assert!(!home.path().join(".config/aibus/config.toml").exists());

    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(aibus(home.path(), &args).status.code(), Some(0));
}

#[test]
fn an_unset_variable_is_exit_4_and_an_unknown_provider_exit_3() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    let unset = aibus(h, &["account", "add", "w", "--provider", "fake", "--env", "NOT_SET_ANYWHERE"]);
    assert_eq!(unset.status.code(), Some(4));
    let unknown = with_key(h, &["account", "add", "w", "--provider", "nope", "--env", "WORK_KEY"]);
    assert_eq!(unknown.status.code(), Some(3));
}

#[test]
fn run_uses_the_named_account_and_exits_4_when_its_key_is_missing() {
    let home = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".config/aibus")).unwrap();
    fs::write(
        h.join(".config/aibus/config.toml"),
        "[[account]]\nname = \"work\"\nprovider = \"fake\"\nauth = { env = \"WORK_KEY\" }\n\n\
         [[account]]\nname = \"other\"\nprovider = \"fake\"\nauth = { env = \"OTHER_KEY\" }\n",
    )
    .unwrap();

    let ok = with_key(h, &["run", "--json", "--agent", "explore", "--account", "work", "map this repo"]);
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));
    assert_eq!(ndjson(&ok)[0]["account"], "work");
    assert!(!stdout(&ok).contains(KEY));

    // The default account is the first that serves the model when nothing says otherwise.
    let none = with_key(h, &["run", "--json", "--agent", "explore", "map this repo"]);
    assert_eq!(none.status.code(), Some(0), "{}", stderr(&none));
    assert_eq!(ndjson(&none)[0]["account"], "work");

    let missing = with_key(h, &["run", "--json", "--agent", "explore", "--account", "other", "x"]);
    assert_eq!(missing.status.code(), Some(4));
    assert!(stderr(&missing).contains("no credential for account other"));

    let unknown = with_key(h, &["run", "--json", "--agent", "explore", "--account", "ghost", "x"]);
    assert_eq!(unknown.status.code(), Some(4));
}
