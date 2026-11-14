//! `aibus account add|list|show|test|remove|enable|disable` (06 section 1, 04 section 5).
//!
//! The config file gets `auth = { keyring = true }` (or `env`, `file`) and never a key. A key goes
//! from stdin or a hidden prompt to the keychain, and only after the provider accepted it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibus_core::config::{self, Account};
use aibus_core::persist::fs::{create_private_dir, write_private};
use aibus_core::persist::paths::Paths;
use aibus_core::{
    AibusError, CredentialError, CredentialStore, KeyRef, Keychain, OAuthFlow, Redactor,
    StoreHealth,
};
use aibus_provider::{
    AccountInfo, AccountName, AuthMethodKind, Credential, ProviderErrorKind, ProviderFactory,
    ProviderRegistry, Secret,
};
use serde_json::{json, Value};
use toml_edit::{value, ArrayOfTables, DocumentMut, InlineTable, Item, Table};
use zeroize::Zeroize;

use crate::cli::{AccountAdd, AccountCmd};
use crate::providers::provider_configs;
use aibus_provider::presets::resolve;

/// Everything the commands reach outside the process for, so a test can stand in for each.
pub struct Io<'a> {
    pub paths: Paths,
    pub keychain: Arc<dyn Keychain>,
    pub factories: Vec<Box<dyn ProviderFactory>>,
    #[allow(clippy::type_complexity)]
    pub env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub redactor: Redactor,
    /// The login and refresh exchange for `--oauth`. `None` when no provider has one.
    pub oauth: Option<Arc<dyn OAuthFlow>>,
    /// Ends the `--oauth` login from outside (`Esc` in the terminal's login modal). `None` for a
    /// command line, where Ctrl-C ends the process.
    pub cancel: Option<tokio_util::sync::CancellationToken>,
    pub stdin: &'a mut dyn Read,
    /// Asks for a key with the echo off. `None` when there is no terminal to ask on.
    pub prompt: Option<&'a dyn Fn() -> std::io::Result<String>>,
    pub out: &'a mut dyn Write,
}

pub fn run(cmd: &AccountCmd, json: bool, io: &mut Io) -> Result<u8, AibusError> {
    let file = io.paths.config_file();
    match cmd {
        AccountCmd::Add(a) => add(a, &file, io)?,
        AccountCmd::List { names } => list(&file, json, *names, io)?,
        AccountCmd::Show { name } => show(name, &file, json, io)?,
        AccountCmd::Test { name } => test(name, &file, json, io)?,
        AccountCmd::Remove { name } => remove(name, &file, io)?,
        AccountCmd::Enable { name } => set_enabled(name, true, &file, io)?,
        AccountCmd::Disable { name } => set_enabled(name, false, &file, io)?,
    }
    Ok(0)
}

fn store(io: &Io) -> CredentialStore {
    let env = io.env.clone();
    let s =
        CredentialStore::new(io.keychain.clone(), io.redactor.clone()).with_env(move |v| env(v));
    match &io.oauth {
        Some(flow) => s.with_oauth(flow.clone()),
        None => s,
    }
}

fn accounts(file: &Path) -> Result<Vec<Account>, AibusError> {
    // Accounts are user-level: the project file is not read.
    Ok(config::load(file, None)?.config.accounts)
}

fn find(file: &Path, name: &str) -> Result<Account, AibusError> {
    accounts(file)?
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| CredentialError::UnknownAccount(AccountName(name.into())).into())
}

/// The key as the provider will see it, checked against the provider. The registry is built per
/// account, so each one can have its own `base_url`.
fn verify(
    io: &Io,
    account: &Account,
    cred: &Credential,
) -> Result<(AccountInfo, Duration), AibusError> {
    let defs = config::load(&io.paths.config_file(), None)?
        .config
        .providers;
    let configs = provider_configs(std::slice::from_ref(account), &defs, &io.paths)?;
    let provider = ProviderRegistry::new(&io.factories, &configs)?.get(&account.provider)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let t0 = Instant::now();
    let info = rt
        .block_on(provider.verify(cred))
        .map_err(|e| match e.kind {
            ProviderErrorKind::Auth => CredentialError::Rejected(account.account_name()).into(),
            _ => AibusError::Provider(e),
        })?;
    let took = t0.elapsed();
    // The model cache is refreshed on `account test` (07 section 4). A failure only leaves the
    // list as it was.
    let _ = rt.block_on(provider.refresh_models(cred));
    Ok((info, took))
}

fn read_key(from_stdin: bool, io: &mut Io) -> Result<Secret<String>, AibusError> {
    let mut raw = if from_stdin {
        let mut s = String::new();
        io.stdin.read_to_string(&mut s)?;
        s
    } else if let Some(ask) = io.prompt {
        ask()?
    } else {
        return Err(AibusError::Usage(
            "no terminal to ask for the key on; pass --key-stdin, --env or --file".into(),
        ));
    };
    let key = Secret::new(raw.trim().to_owned());
    raw.zeroize();
    if key.expose().is_empty() {
        return Err(AibusError::Usage("the key is empty".into()));
    }
    Ok(key)
}

fn add(a: &AccountAdd, file: &Path, io: &mut Io) -> Result<(), AibusError> {
    if a.name.is_empty()
        || !a
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AibusError::Usage(
            "an account name is letters, digits, - and _".into(),
        ));
    }
    if accounts(file)?.iter().any(|x| x.name == a.name) {
        return Err(AibusError::Usage(format!(
            "account {} exists; remove it first: aibus account remove {}",
            a.name, a.name
        )));
    }
    let mut account = Account {
        name: a.name.clone(),
        provider: a.provider.clone(),
        label: a.label.clone(),
        base_url: a.base_url.clone(),
        region: a.region.clone(),
        project: a.project.clone(),
        ..Account::default()
    };
    let store = store(io);
    let name = account.account_name();

    // `--env` and `--file` name where the key already is; nothing is stored for them.
    let cred = if a.oauth {
        let offered = crate::oauth::definition(&account.provider, account.base_url.as_deref())
            .is_some()
            || io.factories.iter().any(|f| {
                f.id().0 == account.provider && f.auth_methods().contains(&AuthMethodKind::OAuth)
            });
        let Some(flow) = io.oauth.clone().filter(|_| offered) else {
            return Err(AibusError::Usage(format!(
                "provider {} has no OAuth login; use a key (--key-stdin, --env or --file)",
                account.provider
            )));
        };
        // A login that returns a durable key leaves a plain key account behind: the OAuth is
        // only the way the key was obtained.
        let key_only = flow.yields_key();
        account.auth.oauth = !key_only;
        account.auth.keyring = key_only;
        if let StoreHealth::Unavailable(why) = store.probe() {
            return Err(CredentialError::NoKeychain(why).into());
        }
        // The flow talks on its own tasks; its lines come back over a channel and are printed
        // here, as they arrive, because `io.out` is not `Send`.
        let (lines, mut shown) = tokio::sync::mpsc::unbounded_channel::<String>();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let cancel = io.cancel.clone().unwrap_or_default();
        let tokens = rt.block_on(async {
            let notify = move |line: &str| {
                let _ = lines.send(line.to_owned());
            };
            let login = flow.authorize(&notify, &cancel);
            tokio::pin!(login);
            loop {
                tokio::select! {
                    done = &mut login => break done,
                    Some(line) = shown.recv() => { let _ = writeln!(io.out, "{line}"); }
                }
            }
        });
        while let Ok(line) = shown.try_recv() {
            let _ = writeln!(io.out, "{line}");
        }
        let tokens = tokens.map_err(CredentialError::OAuth)?;
        io.redactor.register(&name, tokens.access.expose());
        let cred = Credential {
            account: name.clone(),
            secret: Secret::new(tokens.access.expose().clone()),
            kind: if key_only {
                AuthMethodKind::ApiKey
            } else {
                AuthMethodKind::OAuth
            },
            resolved_at: Instant::now(),
        };
        if !key_only {
            verify(io, &account, &cred)?;
            store.store_oauth(&name, &tokens)?;
        }
        cred
    } else if let Some(var) = &a.env {
        account.auth.env = Some(var.clone());
        store.resolve(&account)?
    } else if let Some(path) = &a.file {
        account.auth.file = Some(path.to_string_lossy().into_owned());
        store.resolve(&account)?
    } else {
        account.auth.keyring = true;
        // Before asking for a key that has nowhere to go.
        if let StoreHealth::Unavailable(why) = store.probe() {
            return Err(CredentialError::NoKeychain(why).into());
        }
        // A preset knows what the vendor's key is usually called: offer the environment as the
        // alternative to typing it in, since an env account keeps it out of the keychain.
        let defs = config::load(file, None)?.config.providers;
        let def = resolve(
            &account.provider,
            defs.iter().find(|d| d.id == account.provider),
        );
        if let (Ok(Some(def)), false) = (def, a.key_stdin) {
            if let Some(var) = def.env_vars.first() {
                writeln!(
                    io.out,
                    "Enter the {} key. To read it from the environment instead, run: aibus account add {} --provider {} --env {var}",
                    account.provider, a.name, account.provider
                )?;
            }
        }
        let key = read_key(a.key_stdin, io)?;
        io.redactor.register(&name, key.expose());
        Credential {
            account: name.clone(),
            secret: key,
            kind: AuthMethodKind::ApiKey,
            resolved_at: Instant::now(),
        }
    };

    if !account.auth.oauth {
        verify(io, &account, &cred)?;
    }
    if account.auth.keyring {
        store.store(&name, &cred.secret)?;
    }
    let written = edit(file, |accounts| {
        let mut t = Table::new();
        t.insert("name", value(account.name.as_str()));
        t.insert("provider", value(account.provider.as_str()));
        for (k, v) in [
            ("label", &account.label),
            ("base_url", &account.base_url),
            ("region", &account.region),
            ("project", &account.project),
        ] {
            if let Some(v) = v {
                t.insert(k, value(v.as_str()));
            }
        }
        let mut auth = InlineTable::new();
        match (&account.auth.env, &account.auth.file) {
            _ if account.auth.oauth => auth.insert("oauth", true.into()),
            (Some(var), _) => auth.insert("env", var.as_str().into()),
            (_, Some(path)) => auth.insert("file", path.as_str().into()),
            _ => auth.insert("keyring", true.into()),
        };
        t.insert("auth", Item::Value(auth.into()));
        accounts.push(t);
        Ok(())
    });
    if let Err(e) = written {
        if account.auth.keyring || account.auth.oauth {
            // Do not leave a key behind that no account points to.
            let _ = store.remove(&name);
        }
        return Err(e);
    }
    writeln!(io.out, "added {}", account.name)?;
    Ok(())
}

fn auth_kind(a: &Account) -> &'static str {
    if a.auth.oauth {
        return "oauth";
    }
    match a.key_ref() {
        Some(KeyRef::Keyring { .. }) => "keyring",
        Some(KeyRef::Env { .. }) => "env",
        Some(KeyRef::File { .. }) => "file",
        Some(KeyRef::AwsEnv) => "aws_env",
        Some(KeyRef::AwsProfile { .. }) => "aws_profile",
        None if a.auth.gcloud => "gcloud",
        None => "none",
    }
}

fn status(a: &Account, store: &CredentialStore) -> &'static str {
    if !a.enabled {
        "disabled"
    } else if store.resolve(a).is_ok() {
        "ok"
    } else {
        "missing credential"
    }
}

/// What `list --json` and `show --json` print: where the key lives, never the key.
fn view(a: &Account, status: &str) -> Value {
    json!({
        "name": a.name,
        "provider": a.provider,
        "label": a.label.as_deref().unwrap_or(&a.name),
        "auth": auth_kind(a),
        "key_ref": a.key_ref().map(|k| k.to_string()),
        "base_url": a.base_url,
        "default_model": a.default_model,
        "enabled": a.enabled,
        "rate": a.rate,
        "tags": a.tags,
        "aliases": a.aliases,
        "status": status,
    })
}

fn list(file: &Path, json: bool, names: bool, io: &mut Io) -> Result<(), AibusError> {
    if names {
        for a in accounts(file)? {
            writeln!(io.out, "{}", a.name)?;
        }
        return Ok(());
    }
    let store = store(io);
    let rows: Vec<(Account, &str)> = accounts(file)?
        .into_iter()
        .map(|a| {
            let s = status(&a, &store);
            (a, s)
        })
        .collect();
    if json {
        let all: Vec<Value> = rows.iter().map(|(a, s)| view(a, s)).collect();
        writeln!(io.out, "{}", Value::Array(all))?;
        return Ok(());
    }
    let cells: Vec<[String; 6]> = rows
        .iter()
        .map(|(a, s)| {
            [
                a.name.clone(),
                a.provider.clone(),
                a.label.clone().unwrap_or_else(|| a.name.clone()),
                auth_kind(a).into(),
                a.default_model.clone().unwrap_or_else(|| "-".into()),
                (*s).into(),
            ]
        })
        .collect();
    let width = |i: usize| {
        cells
            .iter()
            .map(|r| r[i].chars().count())
            .max()
            .unwrap_or(0)
    };
    for row in &cells {
        let line: Vec<String> = (0..6)
            .map(|i| format!("{:<w$}", row[i], w = if i == 5 { 0 } else { width(i) }))
            .collect();
        writeln!(io.out, "{}", line.join("  "))?;
    }
    Ok(())
}

fn show(name: &str, file: &Path, json: bool, io: &mut Io) -> Result<(), AibusError> {
    let a = find(file, name)?;
    let v = view(&a, status(&a, &store(io)));
    if json {
        writeln!(io.out, "{v}")?;
        return Ok(());
    }
    if let Value::Object(fields) = &v {
        for (k, v) in fields {
            let text = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            writeln!(io.out, "{k}: {text}")?;
        }
    }
    Ok(())
}

fn test(name: &str, file: &Path, json: bool, io: &mut Io) -> Result<(), AibusError> {
    let a = find(file, name)?;
    let outcome = store(io)
        .resolve(&a)
        .map_err(AibusError::from)
        .and_then(|cred| verify(io, &a, &cred));
    match outcome {
        Ok((info, took)) => {
            let ms = took.as_millis() as u64;
            if json {
                let models: Vec<&str> = info.models.iter().map(|m| m.0.as_str()).collect();
                let v = json!({
                    "name": a.name, "ok": true, "latency_ms": ms,
                    "label": info.label, "org": info.org, "models": models,
                    "cost_usd": info.cost_usd,
                });
                writeln!(io.out, "{v}")?;
            } else {
                match info.cost_usd {
                    Some(usd) => writeln!(
                        io.out,
                        "ok {} ({}) {ms}ms cost ${usd:.4}",
                        a.name, info.label
                    )?,
                    None => writeln!(io.out, "ok {} ({}) {ms}ms", a.name, info.label)?,
                }
            }
            Ok(())
        }
        Err(e) => {
            if json {
                let message = io.redactor.redact(&e.to_string()).into_owned();
                let kind = match &e {
                    AibusError::Auth(_) => "auth",
                    AibusError::Provider(_) => "provider",
                    AibusError::Config(_) | AibusError::ConfigFile(_) => "config",
                    AibusError::Io(_) => "io",
                    _ => "error",
                };
                let error = json!({ "kind": kind, "message": message });
                writeln!(
                    io.out,
                    "{}",
                    json!({ "name": a.name, "ok": false, "error": error })
                )?;
            }
            Err(e)
        }
    }
}

fn remove(name: &str, file: &Path, io: &mut Io) -> Result<(), AibusError> {
    let a = find(file, name)?;
    // Only a keychain entry is ours to delete; an env var or a file is left alone.
    if matches!(a.key_ref(), Some(KeyRef::Keyring { .. })) {
        match store(io).remove(&a.account_name()) {
            Ok(()) => {}
            Err(CredentialError::NoKeychain(why)) => {
                eprintln!("aibus: warning: no keychain ({why}), the account is removed from the config only");
            }
            Err(e) => return Err(e.into()),
        }
    }
    edit(file, |accounts| {
        let at = accounts.iter().position(|t| name_of(t) == Some(name));
        if let Some(i) = at {
            accounts.remove(i);
        }
        Ok(())
    })?;
    writeln!(io.out, "removed {name}")?;
    Ok(())
}

fn set_enabled(name: &str, enabled: bool, file: &Path, io: &mut Io) -> Result<(), AibusError> {
    find(file, name)?;
    edit(file, |accounts| {
        if let Some(t) = accounts.iter_mut().find(|t| name_of(t) == Some(name)) {
            t.insert("enabled", value(enabled));
        }
        Ok(())
    })?;
    writeln!(
        io.out,
        "{} {name}",
        if enabled { "enabled" } else { "disabled" }
    )?;
    Ok(())
}

fn name_of(t: &Table) -> Option<&str> {
    t.get("name").and_then(Item::as_str)
}

/// Edits the `[[account]]` tables in place, so the rest of the file keeps its comments and order.
/// Written through a temporary file, so a crash cannot leave half a config.
fn edit(
    file: &Path,
    change: impl FnOnce(&mut ArrayOfTables) -> Result<(), AibusError>,
) -> Result<(), AibusError> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| AibusError::Usage(format!("{}: {e}", file.display())))?;
    let item = doc
        .entry("account")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    let tables = item.as_array_of_tables_mut().ok_or_else(|| {
        AibusError::Usage(format!(
            "{}: write accounts as [[account]] tables",
            file.display()
        ))
    })?;
    change(tables)?;
    if tables.is_empty() {
        doc.remove("account");
    }
    if let Some(dir) = file.parent() {
        create_private_dir(dir)?;
    }
    let tmp: PathBuf = file.with_extension("toml.tmp");
    write_private(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, file)?;
    Ok(())
}

#[cfg(test)]
mod tests;
