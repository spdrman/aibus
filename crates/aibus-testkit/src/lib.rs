//! Fixtures, fake clock, TestBackend helpers and snapshot macros.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibus_provider::fake::FAKE_KEY;
use aibus_provider::{AccountName, AuthMethodKind, Credential, Secret};
use tracing::subscriber::DefaultGuard;

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

struct Writer(Buffer);

impl io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Active {
    buf: Buffer,
    forbidden: Vec<String>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Captures every `tracing` event on this thread, down to debug level, until dropped.
/// Dropping it runs `assert_no_secret_logged` unless the test is already panicking, so a
/// test that holds one cannot leak the fake key and pass. Tests with a current-thread
/// runtime (all of them, since `FakeClock` needs one) log on the thread that holds it.
pub struct LogCapture {
    buf: Buffer,
    _guard: DefaultGuard,
}

pub fn capture_logs() -> LogCapture {
    let buf: Buffer = Arc::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || Writer(writer.clone()))
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    ACTIVE.with(|a| {
        *a.borrow_mut() = Some(Active {
            buf: buf.clone(),
            forbidden: vec![FAKE_KEY.to_owned()],
        });
    });
    LogCapture { buf, _guard: guard }
}

impl LogCapture {
    /// Everything logged so far.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()).into_owned()
    }

    /// Adds another key (an account's own) to the ones that must never appear.
    pub fn forbid(&self, secret: &str) {
        ACTIVE.with(|a| {
            a.borrow_mut()
                .as_mut()
                .expect("capture is active")
                .forbidden
                .push(secret.to_owned())
        });
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        let check = !std::thread::panicking();
        if check {
            assert_no_secret_logged();
        }
        ACTIVE.with(|a| *a.borrow_mut() = None);
    }
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
        for secret in &active.forbidden {
            assert!(
                !text.contains(secret.as_str()),
                "a secret was logged:\n{text}"
            );
        }
    });
}
