//! The account commands against an in-memory keychain and a temporary home: no real keychain,
//! no network. (keyring's own mock store gives every `Entry` a private, empty value, so a key
//! stored through one entry is never found through another; `MemoryKeychain` keeps what is
//! stored, which is what these tests need.)

use std::collections::HashMap;
use std::sync::Arc;

use aibus_core::{
    AibusError, CredentialError, FakeOAuth, Keychain, MemoryKeychain, OAuthFlow, Redactor,
};
use aibus_provider::fake::{FakeProvider, Script};
use aibus_provider::{
    AuthMethodKind, ConfigError, Provider, ProviderConfig, ProviderFactory, ProviderId,
};
use tempfile::TempDir;

use super::*;

const KEY: &str = "sk-live-0123456789abcdef";

/// The fake, but it accepts only `KEY` for the account `work`.
struct Strict;

impl ProviderFactory for Strict {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }
    fn build(&self, _: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(FakeProvider::new(Script::new()).with_account("work", KEY)))
    }
    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey]
    }
}

/// The fake with an OAuth login: it accepts the first token `FakeOAuth` hands out.
struct Oauthy;

impl ProviderFactory for Oauthy {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }
    fn build(&self, _: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(FakeProvider::new(Script::new()).with_account("work", TOKEN)))
    }
    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey, AuthMethodKind::OAuth]
    }
}

const TOKEN: &str = "oauth-access-1-abcdef";

struct Rig {
    home: TempDir,
    keychain: Arc<MemoryKeychain>,
    env: HashMap<String, String>,
    oauth: Option<Arc<dyn OAuthFlow>>,
    offers_oauth: bool,
}

struct Ran {
    result: Result<u8, AibusError>,
    out: String,
}

impl Rig {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
            keychain: Arc::new(MemoryKeychain::default()),
            env: HashMap::new(),
            oauth: None,
            offers_oauth: false,
        }
    }

    fn with_keychain(keychain: MemoryKeychain) -> Self {
        Self {
            keychain: Arc::new(keychain),
            ..Self::new()
        }
    }

    fn config(&self) -> PathBuf {
        self.home.path().join("config.toml")
    }

    fn run(&self, cmd: &AccountCmd, stdin: &str, prompt: Option<&str>) -> Ran {
        self.run_as(cmd, stdin, prompt, false)
    }

    fn run_as(&self, cmd: &AccountCmd, stdin: &str, prompt: Option<&str>, json: bool) -> Ran {
        let mut input = stdin.as_bytes();
        let mut out = Vec::new();
        let asked = prompt.map(|p| move || Ok(p.to_owned()));
        let env = self.env.clone();
        let mut io = Io {
            paths: Paths::resolve(|k| match k {
                "HOME" => Some(self.home.path().display().to_string()),
                "AIBUS_CONFIG" => Some(self.config().display().to_string()),
                _ => None,
            })
            .unwrap(),
            keychain: self.keychain.clone(),
            factories: if self.offers_oauth { vec![Box::new(Oauthy)] } else { vec![Box::new(Strict)] },
            env: Arc::new(move |v| env.get(v).cloned()),
            redactor: Redactor::new(),
            oauth: self.oauth.clone(),
            stdin: &mut input,
            prompt: asked.as_ref().map(|f| f as &dyn Fn() -> std::io::Result<String>),
            out: &mut out,
        };
        let result = run(cmd, json, &mut io);
        Ran {
            result,
            out: String::from_utf8(out).unwrap(),
        }
    }

    fn json(&self, cmd: &AccountCmd) -> Value {
        let out = self.run_as(cmd, "", None, true).out;
        serde_json::from_str(&out).unwrap_or_else(|_| panic!("not JSON: {out}"))
    }

    fn secret_in_files(&self, needle: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, needle, found);
                } else if String::from_utf8_lossy(&std::fs::read(&p).unwrap()).contains(needle) {
                    found.push(p);
                }
            }
        }
        let mut found = Vec::new();
        walk(self.home.path(), needle, &mut found);
        found
    }
}

fn add(name: &str) -> AccountAdd {
    AccountAdd {
        name: name.into(),
        provider: "fake".into(),
        key_stdin: false,
        env: None,
        file: None,
        oauth: false,
        base_url: None,
        label: None,
    }
}

fn add_cmd(f: impl FnOnce(&mut AccountAdd)) -> AccountCmd {
    let mut a = add("work");
    f(&mut a);
    AccountCmd::Add(a)
}

fn stdin_cmd() -> AccountCmd {
    add_cmd(|a| a.key_stdin = true)
}

fn name_cmd(f: fn(String) -> AccountCmd) -> AccountCmd {
    f("work".into())
}

fn exit(r: &Result<u8, AibusError>) -> u8 {
    match r {
        Ok(c) => *c,
        Err(e) => e.exit_code(),
    }
}

#[test]
fn add_with_key_stdin_stores_in_the_keychain_and_writes_no_key_anywhere() {
    let rig = Rig::new();
    let ran = rig.run(
        &add_cmd(|a| {
            a.key_stdin = true;
            a.label = Some("Work".into());
        }),
        &format!("{KEY}\n"),
        None,
    );
    assert_eq!(exit(&ran.result), 0, "{:?}", ran.result.err());
    assert_eq!(ran.out, "added work\n");

    let kc = rig.keychain.get("aibus", "account:work").unwrap().unwrap();
    assert_eq!(kc.expose(), KEY);

    let text = std::fs::read_to_string(rig.config()).unwrap();
    assert!(text.contains("auth = { keyring = true }"), "{text}");
    assert!(text.contains("label = \"Work\""));

    // What `config show` prints, and every file under the temporary home.
    let loaded = config::load(&rig.config(), None).unwrap();
    assert!(!toml::to_string_pretty(&loaded.config).unwrap().contains(KEY));
    assert_eq!(rig.secret_in_files(KEY), Vec::<PathBuf>::new());
}

#[test]
fn add_asks_with_the_hidden_prompt_when_no_source_is_given() {
    let rig = Rig::new();
    let ran = rig.run(&add_cmd(|_| {}), "", Some(KEY));
    assert_eq!(exit(&ran.result), 0);
    assert!(rig.keychain.get("aibus", "account:work").unwrap().is_some());

    let none = Rig::new().run(&add_cmd(|_| {}), "", None);
    assert!(matches!(none.result, Err(AibusError::Usage(_))));
}

#[test]
fn add_verifies_first_and_a_rejected_key_is_not_stored_or_written() {
    let rig = Rig::new();
    let ran = rig.run(&stdin_cmd(), "sk-wrong-key-9999\n", None);
    let Err(AibusError::Auth(CredentialError::Rejected(a))) = &ran.result else {
        panic!("{:?}", ran.result)
    };
    assert_eq!(a.to_string(), "work");
    assert_eq!(exit(&ran.result), 4);
    assert!(rig.keychain.get("aibus", "account:work").unwrap().is_none());
    assert!(!rig.config().exists());
}

#[test]
fn add_without_a_keychain_offers_env_and_file_and_writes_nothing() {
    let rig = Rig::with_keychain(MemoryKeychain::unavailable("no Secret Service"));
    let ran = rig.run(&stdin_cmd(), &format!("{KEY}\n"), None);
    assert_eq!(exit(&ran.result), 4);
    let msg = ran.result.unwrap_err().to_string();
    assert!(msg.contains("--env") && msg.contains("--file"), "{msg}");
    assert!(msg.contains("never stored in plaintext"));
    assert!(!rig.config().exists());
    assert_eq!(rig.secret_in_files(KEY), Vec::<PathBuf>::new());
}

#[test]
fn add_with_env_records_the_variable_and_stores_nothing() {
    let mut rig = Rig::new();
    rig.env.insert("WORK_KEY".into(), KEY.into());
    let ran = rig.run(&add_cmd(|a| a.env = Some("WORK_KEY".into())), "", None);
    assert_eq!(exit(&ran.result), 0, "{:?}", ran.result.err());
    let text = std::fs::read_to_string(rig.config()).unwrap();
    assert!(text.contains("auth = { env = \"WORK_KEY\" }"), "{text}");
    assert!(rig.keychain.get("aibus", "account:work").unwrap().is_none());
    assert_eq!(rig.secret_in_files(KEY), Vec::<PathBuf>::new());

    let unset = Rig::new().run(&add_cmd(|a| a.env = Some("NOT_SET".into())), "", None);
    assert!(matches!(unset.result, Err(AibusError::Auth(CredentialError::Missing(_)))));
}

#[cfg(unix)]
#[test]
fn add_with_file_refuses_a_file_others_can_read_and_says_how_to_fix_it() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    let key = rig.home.path().join("key");
    std::fs::write(&key, format!("{KEY}\n")).unwrap();
    let cmd = add_cmd(|a| a.file = Some(key.clone()));

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let ran = rig.run(&cmd, "", None);
    assert_eq!(exit(&ran.result), 4);
    let msg = ran.result.unwrap_err().to_string();
    assert!(msg.contains(&format!("chmod 600 {}", key.display())), "{msg}");
    assert!(!rig.config().exists());

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let ran = rig.run(&cmd, "", None);
    assert_eq!(exit(&ran.result), 0, "{:?}", ran.result.err());
    let text = std::fs::read_to_string(rig.config()).unwrap();
    assert!(text.contains(&format!("file = \"{}\"", key.display())), "{text}");
}

#[test]
fn add_refuses_a_second_account_with_the_name_and_an_unknown_provider() {
    let rig = Rig::new();
    assert_eq!(exit(&rig.run(&stdin_cmd(), &format!("{KEY}\n"), None).result), 0);
    let again = rig.run(&stdin_cmd(), &format!("{KEY}\n"), None);
    assert!(matches!(again.result, Err(AibusError::Usage(_))));

    let other = rig.run(&add_cmd(|a| {
        a.name = "x".into();
        a.provider = "nope".into();
        a.key_stdin = true;
    }), "k\n", None);
    assert_eq!(exit(&other.result), 3);

    let bad = rig.run(&add_cmd(|a| a.name = "has space".into()), "", None);
    assert!(matches!(bad.result, Err(AibusError::Usage(_))));
}

#[test]
fn add_keeps_the_comments_and_the_other_accounts_in_the_file() {
    let rig = Rig::new();
    std::fs::write(
        rig.config(),
        "# my config\n[defaults]\nmodel = \"sonnet\" # the usual\n\n[[account]]\nname = \"old\"\nprovider = \"fake\"\nauth = { none = true }\n",
    )
    .unwrap();
    assert_eq!(exit(&rig.run(&stdin_cmd(), &format!("{KEY}\n"), None).result), 0);
    let text = std::fs::read_to_string(rig.config()).unwrap();
    assert!(text.starts_with("# my config\n"));
    assert!(text.contains("model = \"sonnet\" # the usual"));
    let accounts = config::load(&rig.config(), None).unwrap().config.accounts;
    let names: Vec<_> = accounts.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["old", "work"]);
}

#[test]
fn list_reports_ok_missing_credential_and_disabled_and_never_a_key() {
    let mut rig = Rig::new();
    rig.env.insert("HAVE".into(), KEY.into());
    std::fs::write(
        rig.config(),
        r#"
[[account]]
name = "a-ok"
provider = "fake"
auth = { env = "HAVE" }
label = "Fine"

[[account]]
name = "b-missing"
provider = "fake"
auth = { keyring = true }

[[account]]
name = "c-off"
provider = "fake"
enabled = false
auth = { none = true }
"#,
    )
    .unwrap();

    let text = rig.run(&AccountCmd::List { names: false }, "", None).out;
    let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split("  ").filter(|c| !c.is_empty()).map(str::trim).collect()).collect();
    assert_eq!(rows[0], ["a-ok", "fake", "Fine", "env", "-", "ok"]);
    assert_eq!(rows[1][5], "missing credential");
    assert_eq!(rows[2][5], "disabled");

    let json = rig.json(&AccountCmd::List { names: false });
    let statuses: Vec<&str> = json.as_array().unwrap().iter().map(|r| r["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, ["ok", "missing credential", "disabled"]);
    assert!(!json.to_string().contains(KEY));
    assert_eq!(json[1]["key_ref"], "keyring:aibus/b-missing");
}

#[test]
fn show_prints_where_the_key_lives_and_exits_4_for_an_unknown_account() {
    let rig = Rig::new();
    assert_eq!(exit(&rig.run(&stdin_cmd(), &format!("{KEY}\n"), None).result), 0);
    let ran = rig.run(&name_cmd(|name| AccountCmd::Show { name }), "", None);
    assert!(ran.out.contains("key_ref: keyring:aibus/work"), "{}", ran.out);
    assert!(ran.out.contains("status: ok"));
    assert!(!ran.out.contains(KEY));

    let json = rig.json(&name_cmd(|name| AccountCmd::Show { name }));
    assert_eq!(json["auth"], "keyring");
    assert_eq!(json["rate"]["max_concurrent"], 4);

    let missing = rig.run(&AccountCmd::Show { name: "ghost".into() }, "", None);
    assert_eq!(exit(&missing.result), 4);
}

#[test]
fn test_reports_latency_as_json_and_exits_4_for_a_key_the_provider_rejects() {
    let rig = Rig::new();
    assert_eq!(exit(&rig.run(&stdin_cmd(), &format!("{KEY}\n"), None).result), 0);
    let ok = rig.json(&name_cmd(|name| AccountCmd::Test { name }));
    assert_eq!(ok["ok"], true);
    assert!(ok["latency_ms"].is_u64(), "{ok}");

    // The keychain now holds a different key than the provider accepts.
    rig.keychain
        .set("aibus", "account:work", &Secret::new("sk-rotated-and-wrong".into()))
        .unwrap();
    let bad = rig.run(&name_cmd(|name| AccountCmd::Test { name }), "", None);
    assert_eq!(exit(&bad.result), 4);
    let json = rig.json(&name_cmd(|name| AccountCmd::Test { name }));
    assert_eq!(json["ok"], false);
    assert!(!json.to_string().contains("sk-rotated"));
}

#[test]
fn remove_deletes_the_keychain_entry_and_the_table_only() {
    let rig = Rig::new();
    std::fs::write(rig.config(), "# keep me\n").unwrap();
    assert_eq!(exit(&rig.run(&stdin_cmd(), &format!("{KEY}\n"), None).result), 0);
    let ran = rig.run(&name_cmd(|name| AccountCmd::Remove { name }), "", None);
    assert_eq!(ran.out, "removed work\n");
    assert!(rig.keychain.get("aibus", "account:work").unwrap().is_none());
    assert_eq!(std::fs::read_to_string(rig.config()).unwrap(), "# keep me\n");
    assert_eq!(exit(&rig.run(&name_cmd(|name| AccountCmd::Remove { name }), "", None).result), 4);
}

#[test]
fn disable_and_enable_flip_the_flag_in_the_file() {
    let rig = Rig::new();
    assert_eq!(exit(&rig.run(&stdin_cmd(), &format!("{KEY}\n"), None).result), 0);
    let enabled = || config::load(&rig.config(), None).unwrap().config.accounts[0].enabled;
    assert!(enabled());
    rig.run(&name_cmd(|name| AccountCmd::Disable { name }), "", None);
    assert!(!enabled());
    rig.run(&name_cmd(|name| AccountCmd::Enable { name }), "", None);
    assert!(enabled());
    assert_eq!(exit(&rig.run(&AccountCmd::Enable { name: "ghost".into() }, "", None).result), 4);
}

fn oauth_rig() -> Rig {
    Rig {
        oauth: Some(Arc::new(FakeOAuth::new(1_000, 3_600))),
        offers_oauth: true,
        ..Rig::new()
    }
}

fn oauth_cmd() -> AccountCmd {
    add_cmd(|a| a.oauth = true)
}

#[test]
fn add_oauth_logs_in_verifies_and_keeps_tokens_only_in_the_keychain() {
    let rig = oauth_rig();
    let ran = rig.run(&oauth_cmd(), "", None);
    assert_eq!(exit(&ran.result), 0, "{:?}", ran.result.err());
    assert!(ran.out.contains("enter code ABCD-1234") && ran.out.ends_with("added work\n"), "{}", ran.out);
    assert!(!ran.out.contains("oauth-access"));

    let kept = rig.keychain.get("aibus", "account:work").unwrap().unwrap();
    assert!(kept.expose().contains(TOKEN) && kept.expose().contains("oauth-refresh-1-abcdef"));
    assert!(std::fs::read_to_string(rig.config()).unwrap().contains("auth = { oauth = true }"));
    // Neither token is in any file under the home, nor in what `account show --json` prints.
    for needle in [TOKEN, "oauth-refresh-1-abcdef"] {
        assert_eq!(rig.secret_in_files(needle), Vec::<PathBuf>::new());
    }
    let shown = rig.json(&AccountCmd::Show { name: "work".into() });
    assert_eq!(shown["auth"], "oauth");
    assert!(!shown.to_string().contains("oauth-"));
    assert_eq!(shown["status"], "ok");
}

#[test]
fn add_oauth_is_refused_for_a_provider_that_does_not_offer_it() {
    let rig = Rig { oauth: Some(Arc::new(FakeOAuth::new(1_000, 3_600))), ..Rig::new() };
    let ran = rig.run(&oauth_cmd(), "", None);
    assert!(matches!(&ran.result, Err(AibusError::Usage(m)) if m.contains("no OAuth login")), "{:?}", ran.result);
    assert!(rig.keychain.get("aibus", "account:work").unwrap().is_none());
    assert!(!rig.config().exists());

    // And with no flow at all, as in the shipped binary.
    let none = Rig { offers_oauth: true, ..Rig::new() }.run(&oauth_cmd(), "", None);
    assert!(matches!(none.result, Err(AibusError::Usage(_))));
}

#[test]
fn a_rejected_oauth_token_is_not_stored_or_written() {
    let rig = Rig { oauth: Some(Arc::new(FakeOAuth::new(1_000, 3_600))), offers_oauth: true, ..Rig::new() };
    // Burn the first token so the login hands out `-2`, which the provider does not accept.
    let flow = rig.oauth.clone().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let _ = rt.block_on(flow.authorize(&|_| {}, &tokio_util::sync::CancellationToken::new()));
    let ran = rig.run(&oauth_cmd(), "", None);
    assert_eq!(exit(&ran.result), 4, "{:?}", ran.result);
    assert!(rig.keychain.get("aibus", "account:work").unwrap().is_none());
    assert!(!rig.config().exists());
}

#[test]
fn add_oauth_for_a_login_that_returns_a_key_stores_it_as_a_plain_keyring_account() {
    // OpenRouter-style: the login is only the way to get a durable key, nothing to refresh.
    let mut flow = FakeOAuth::new(1_000, 3_600);
    flow.yields_key = true;
    let rig = Rig { oauth: Some(Arc::new(flow)), offers_oauth: true, ..Rig::new() };
    let ran = rig.run(&oauth_cmd(), "", None);
    assert_eq!(exit(&ran.result), 0, "{:?}", ran.result.err());
    let cfg = std::fs::read_to_string(rig.config()).unwrap();
    assert!(cfg.contains("auth = { keyring = true }") && !cfg.contains("oauth"), "{cfg}");
    let kept = rig.keychain.get("aibus", "account:work").unwrap().unwrap();
    assert_eq!(kept.expose(), TOKEN, "the key itself, not a JSON token set");
    assert_eq!(rig.secret_in_files(TOKEN), Vec::<PathBuf>::new());
    assert!(!ran.out.contains(TOKEN));
}
