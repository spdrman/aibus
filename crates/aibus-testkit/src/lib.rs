//! Fixtures, fake clock, TestBackend helpers and snapshot macros.
#![forbid(unsafe_code)]

pub mod capability_matrix;
pub mod catalog_sync;
pub mod conformance;
pub mod oauth;
pub mod replay;
mod session;
pub mod tui;

pub use session::{big_session, xdg_temp, Xdg};

use std::cell::RefCell;
use std::io;

use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use aibus_provider::fake::FAKE_KEY;
use aibus_provider::{AccountName, AuthMethodKind, Credential, Secret};


/// Virtual time over `tokio::time::pause()`. Sleeps, timeouts and backoffs in the code under
/// test finish as soon as every task is idle, and the clock jumps forward by exactly what
/// they asked for, so five 30 s backoffs cost milliseconds of wall time.
///
/// Only works on a current-thread runtime (`#[tokio::test]` is one); `new` panics otherwise.
/// Use it instead of `start_paused = true` when a test wants the clock as a value.
pub struct FakeClock {
    start: tokio::time::Instant,
}

impl FakeClock {
    pub fn new() -> Self {
        tokio::time::pause();
        Self {
            start: tokio::time::Instant::now(),
        }
    }

    /// Virtual time since `new`.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Moves the clock forward and lets timers that fall due run.
    pub async fn advance(&self, by: Duration) {
        tokio::time::advance(by).await;
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Every credential kind the code can hold, each planted as a made-up value of the real shape:
/// `(kind, value, the pattern that knows its shape)`. A kind with no pattern (an AWS secret key,
/// a refresh token, the MCP bearer) is only caught once it is registered, and
/// `register_credential_kinds` does that for those. `capture_logs` and `xdg_temp` forbid all of
/// them, so a test that plants one and lets it reach a log line or a file fails on its own.
/// `fixtures/redact/provider-errors.toml` must have a row for each (a test in `aibus-core` checks).
pub const CREDENTIAL_KINDS: &[(&str, &str, Option<&str>)] = &[
    ("anthropic-api-key", "sk-ant-api03-Zq8wLm2Xv5Rt7Yb1Nc4Hd6Gf9Jk3P", Some("anthropic-key")),
    ("openai-api-key", "sk-proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12", Some("openai-project-key")),
    ("oauth-access-token", "oat-Hq7Zk2Vm9Xc4Lp1Ws8Nd3Rf6Tb0Yg5Jx", None),
    ("oauth-refresh-token", "rt-Zq81KpLw0Nn5Vd3Ey7Ux2Hc9Ta4Rb6Mf", None),
    ("oauth-pkce-verifier", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", None),
    ("oauth-authorization-code", "auth-code-Zq81KpLw0Nn5Vd3Ey7Ux2Hc9", None),
    ("oauth-device-code", "dc-Zq81KpLw0Nn5Vd3Ey7Ux2Hc9Ta4Rb6Mf", None),
    ("aws-access-key-id", "AKIAIOSFODNN7EXAMPLE", Some("aws-access-key")),
    ("aws-secret-access-key", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", None),
    ("aws-session-token", "IQoJb3JpZ2luX2VjEJr//////////wEaCXVzLWVhc3QtMSJHMEUCIQD3ExampleSessionTokenOnly", None),
    ("bedrock-api-key", "ABSKQmVkcm9ja0FQSUtleS1leGFtcGxlLTAxMjM0NTY3ODlhYmNkZWY", None),
    ("vertex-access-token", "ya29.a0AfB_byC-Example0123456789abcdefGHIJKLmnopQRSTuv", Some("google-access-token")),
    ("gemini-api-key", "AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q", Some("google-api-key")),
    ("openrouter-key", "sk-or-v1-Zq81KpLw0Nn5Vd3Ey7Ux2Hc9Ta4Rb6MfZq81KpLw0Nn5Vd3Ey7Ux2Hc9Ta4Rb6Mf", Some("openrouter-key")),
    ("mcp-http-token", "3b9f1c0a77d24e58a6c1f0e9d8b7a6954c3d2e1f00a9b8c7d6e5f4a3b2c1d0e9", None),
];

/// Registers the kinds no pattern knows with `r`, the way the credential store does when it
/// resolves them.
pub fn register_credential_kinds(r: &aibus_core::Redactor) {
    for (kind, value, pattern) in CREDENTIAL_KINDS {
        if pattern.is_none() {
            r.register(&AccountName((*kind).into()), value);
        }
    }
}

/// A credential for `account` carrying `key`, for handing to a provider under test.
pub fn credential(account: &str, key: &str) -> Credential {
    Credential {
        account: AccountName(account.into()),
        secret: Secret::new(key.to_owned()),
        kind: AuthMethodKind::ApiKey,
        resolved_at: std::time::Instant::now(),
    }
}

/// The default fake credential, `FAKE_KEY`, for an account called `fake`.
pub fn fake_credential() -> Credential {
    credential("fake", FAKE_KEY)
}

type Buffer = Arc<Mutex<Vec<u8>>>;

/// Writes into the buffer of the capture active on the calling thread, and drops the line
/// when there is none.
struct Writer;

impl io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        ACTIVE.with(|a| {
            if let Some(active) = a.borrow().as_ref() {
                active.buf.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Active {
    buf: Buffer,
    forbidden: Forbidden,
}

/// The secrets a capture checks for; shared between the capture and the thread's active slot.
type Forbidden = Arc<Mutex<Vec<String>>>;

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Captures every `tracing` event on this thread, down to debug level, until dropped.
/// Dropping it runs `assert_no_secret_logged` unless the test is already panicking, so a
/// test that holds one cannot leak the fake key and pass. Tests with a current-thread
/// runtime (all of them, since `FakeClock` needs one) log on the thread that holds it.
pub struct LogCapture {
    buf: Buffer,
    forbidden: Forbidden,
}

/// The subscriber is global and installed once. A scoped one is only seen by callsites
/// registered after it exists, and a callsite another test thread hits first can be cached as
/// "never", so the capture would miss events now and then (#55).
pub fn capture_logs() -> LogCapture {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(|| Writer)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
    });
    let buf: Buffer = Arc::default();
    let forbidden: Forbidden = Arc::new(Mutex::new(
        std::iter::once(FAKE_KEY).chain(CREDENTIAL_KINDS.iter().map(|k| k.1)).map(str::to_owned).collect(),
    ));
    ACTIVE.with(|a| {
        *a.borrow_mut() = Some(Active {
            buf: buf.clone(),
            forbidden: forbidden.clone(),
        });
    });
    LogCapture { buf, forbidden }
}

impl LogCapture {
    /// Everything logged so far.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()).into_owned()
    }

    /// Adds another key (an account's own) to the ones that must never appear.
    pub fn forbid(&self, secret: &str) {
        self.forbidden.lock().unwrap().push(secret.to_owned());
    }

    fn check(&self) {
        let text = self.text();
        for secret in self.forbidden.lock().unwrap().iter() {
            assert!(
                !text.contains(secret.as_str()),
                "a secret was logged:\n{text}"
            );
        }
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.check();
        }
        // Only clear the slot if it is still ours: a test that makes its own capture after an
        // `xdg_temp` did replaces the slot, and the older one must not clear the newer.
        ACTIVE.with(|a| {
            let mut slot = a.borrow_mut();
            if slot.as_ref().is_some_and(|s| Arc::ptr_eq(&s.buf, &self.buf)) {
                *slot = None;
            }
        });
    }
}

/// `capture_logs()` unless one is already running on this thread; `xdg_temp` uses it so every
/// test that makes a directory also checks its logs for the fake key.
pub(crate) fn capture_unless_live() -> Option<LogCapture> {
    let live = ACTIVE.with(|a| a.borrow().is_some());
    (!live).then(capture_logs)
}

/// Panics if the fake key (or any key added with `LogCapture::forbid`) appears in what
/// `capture_logs` has recorded on this thread.
pub fn assert_no_secret_logged() {
    ACTIVE.with(|a| {
        let a = a.borrow();
        let active = a
            .as_ref()
            .expect("assert_no_secret_logged needs a live capture_logs()");
        let text = String::from_utf8_lossy(&active.buf.lock().unwrap()).into_owned();
        for secret in active.forbidden.lock().unwrap().iter() {
            assert!(
                !text.contains(secret.as_str()),
                "a secret was logged:\n{text}"
            );
        }
    });
}
