//! The Vertex token source (`auth = { gcloud = true }`): `GOOGLE_OAUTH_ACCESS_TOKEN` when it is
//! set, else `gcloud auth print-access-token`. The command is run with an argument list and no
//! shell, found on `PATH`, killed after ten seconds, with stderr dropped. Its output is a secret:
//! it goes straight into the credential store, which keeps it fifty minutes under the account's
//! name and registers it with the redactor, and it is never written to disk or into an error.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibus_core::CredentialStore;
use aibus_provider::{Credential, ProviderError, ProviderErrorKind, Secret};
use aibus_provider_anthropic::TokenSource;
use async_trait::async_trait;
use zeroize::Zeroizing;

/// Reads one variable of the process environment.
type EnvFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Runs `gcloud auth print-access-token`.
type RunFn = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

pub struct GcloudTokens {
    store: Arc<CredentialStore>,
    env: EnvFn,
    run: RunFn,
}

impl GcloudTokens {
    /// `env` reads the process environment and `run` is `gcloud` itself in a real run; a test
    /// hands in fakes.
    pub fn new(store: Arc<CredentialStore>, env: EnvFn, run: RunFn) -> Self {
        Self { store, env, run }
    }
}

#[async_trait]
impl TokenSource for GcloudTokens {
    async fn token(&self, creds: &Credential) -> Result<Secret<String>, ProviderError> {
        let (store, env, run, account) = (self.store.clone(), self.env.clone(), self.run.clone(), creds.account.clone());
        // Starting `gcloud` blocks, so it happens off the runtime's threads.
        let fetched = tokio::task::spawn_blocking(move || {
            store.cached_token(&account, Duration::from_secs(50 * 60), || {
                match env("GOOGLE_OAUTH_ACCESS_TOKEN").filter(|t| !t.is_empty()) {
                    Some(t) => Ok(t),
                    None => run(),
                }
            })
        })
        .await
        .map_err(|e| ProviderError::new(ProviderErrorKind::Unknown, format!("the token task failed: {e}")))?;
        fetched.map_err(|e| {
            ProviderError::new(ProviderErrorKind::Auth, format!("could not get a Google access token ({e}); run: gcloud auth login"))
        })
    }
}

/// `<program> auth print-access-token`, its trimmed stdout. The errors say what went wrong with
/// the process and never repeat what it printed.
pub fn run_gcloud(program: &str, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(["auth", "print-access-token"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run gcloud ({}); is it installed and on PATH?", e.kind()))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("gcloud timed out after {}s", timeout.as_secs().max(1)));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("gcloud could not be waited for ({})", e.kind())),
        }
    };
    if !status.success() {
        return Err(format!("gcloud exited with {status}"));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut bytes).map_err(|e| format!("gcloud's output could not be read ({})", e.kind()))?;
    }
    let token = Zeroizing::new(String::from_utf8_lossy(&bytes).trim().to_owned());
    match token.is_empty() {
        true => Err("gcloud printed no token".to_owned()),
        false => Ok(token.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use aibus_core::{CredentialStore, MemoryKeychain, Redactor};
    use aibus_provider::fake::FAKE_KEY;
    use aibus_provider::{AccountName, AuthMethodKind, Credential, ProviderErrorKind, Secret};
    use aibus_provider_anthropic::TokenSource;
    use aibus_testkit::{assert_no_secret_logged, capture_logs};

    use super::*;

    fn cred() -> Credential {
        Credential {
            account: AccountName("vertex".into()),
            secret: Secret::new(String::new()),
            kind: AuthMethodKind::None,
            resolved_at: Instant::now(),
        }
    }

    type Env = Arc<Mutex<Vec<(String, String)>>>;

    /// A token source whose `gcloud` is a counter, with the redactor it registers tokens with.
    fn source(env: &Env, runs: &Arc<AtomicUsize>, out: Result<String, String>) -> (GcloudTokens, Redactor) {
        let redactor = Redactor::new();
        let store = Arc::new(CredentialStore::new(Arc::new(MemoryKeychain::default()), redactor.clone()));
        let env = env.clone();
        let runs = runs.clone();
        let tokens = GcloudTokens::new(
            store,
            Arc::new(move |v| env.lock().unwrap().iter().find(|(k, _)| k == v).map(|(_, v)| v.clone())),
            Arc::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
                out.clone()
            }),
        );
        (tokens, redactor)
    }

    #[tokio::test]
    async fn the_token_comes_from_gcloud_once_and_is_then_cached_under_the_account() {
        let runs = Arc::new(AtomicUsize::new(0));
        let (tokens, redactor) = source(&Env::default(), &runs, Ok(FAKE_KEY.to_owned()));
        let logs = capture_logs();
        for _ in 0..3 {
            assert_eq!(tokens.token(&cred()).await.unwrap().expose(), FAKE_KEY);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "gcloud is not run again inside the 50 minutes");
        assert!(!redactor.redact(&format!("token {FAKE_KEY}")).contains(FAKE_KEY));
        assert_no_secret_logged();
        drop(logs);
    }

    #[tokio::test]
    async fn google_oauth_access_token_wins_and_gcloud_is_not_run() {
        let runs = Arc::new(AtomicUsize::new(0));
        let env: Env = Arc::new(Mutex::new(vec![("GOOGLE_OAUTH_ACCESS_TOKEN".into(), "ya29.from-the-environment-0001".into())]));
        let (tokens, redactor) = source(&env, &runs, Ok("ya29.from-gcloud".into()));
        assert_eq!(tokens.token(&cred()).await.unwrap().expose(), "ya29.from-the-environment-0001");
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert!(!redactor.redact("t ya29.from-the-environment-0001").contains("ya29.from"));
    }

    #[tokio::test]
    async fn a_gcloud_that_fails_is_an_auth_error_that_repeats_nothing_gcloud_said() {
        let runs = Arc::new(AtomicUsize::new(0));
        let (tokens, _) = source(&Env::default(), &runs, Err("gcloud exited with status 1".into()));
        let e = tokens.token(&cred()).await.unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::Auth);
        assert!(e.message.contains("gcloud auth login"), "tell them the fix: {}", e.message);
        // Nothing was cached, so the next request tries again.
        let _ = tokens.token(&cred()).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[cfg(unix)]
    fn script(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("gcloud-fake");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn the_real_runner_takes_stdout_trimmed_with_no_shell_and_drops_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let ok = script(dir.path(), "echo \"ya29.printed-0001\"; echo 'chatter on stderr' >&2");
        assert_eq!(run_gcloud(&ok, Duration::from_secs(5)).unwrap(), "ya29.printed-0001");

        // Its argv is exactly `auth print-access-token`, so a script can check it was not given more.
        let argv = script(dir.path(), "[ \"$#\" = 2 ] && [ \"$1\" = auth ] && [ \"$2\" = print-access-token ] && echo ya29.argv-ok-0002");
        assert_eq!(run_gcloud(&argv, Duration::from_secs(5)).unwrap(), "ya29.argv-ok-0002");

        // A name with shell syntax in it is a program name and nothing else.
        let marker = dir.path().join("pwned");
        let evil = format!("true; touch {}", marker.display());
        assert!(run_gcloud(&evil, Duration::from_secs(5)).is_err());
        assert!(!marker.exists(), "a shell ran the command");
    }

    #[cfg(unix)]
    #[test]
    fn the_real_runner_gives_up_after_its_timeout_and_never_echoes_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let slow = script(dir.path(), "sleep 30");
        let began = Instant::now();
        let e = run_gcloud(&slow, Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(began.elapsed() < Duration::from_secs(5), "{:?}", began.elapsed());

        let failing = script(dir.path(), "echo 'SECRET-looking-stderr-text' >&2; exit 1");
        let e = run_gcloud(&failing, Duration::from_secs(5)).unwrap_err();
        assert!(!e.contains("SECRET-looking"), "{e}");

        let e = run_gcloud("/nonexistent/gcloud", Duration::from_secs(1)).unwrap_err();
        assert!(e.contains("gcloud"), "{e}");
    }
}
