//! SIGINT, SIGTERM and SIGHUP as one stream of "stop now" requests, and SIGWINCH as "the window
//! changed size" (06 section 5). They are registered at startup so one that arrives early is
//! queued, not fatal.
//!
//! `signal-hook` rather than tokio's `signal` feature: that feature turns on `mio` for every
//! crate in the workspace, and a test keeps `mio` out of `aibus-provider`'s tree.

use std::io;

use tokio::sync::mpsc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    /// SIGINT, SIGTERM or SIGHUP.
    Stop,
    /// SIGWINCH.
    Resize,
}

pub struct Signals {
    rx: mpsc::UnboundedReceiver<Signal>,
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
}

impl Signals {
    #[cfg(unix)]
    pub fn new() -> io::Result<Self> {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
        let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP, SIGWINCH])?;
        let handle = signals.handle();
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            // `forever` ends when the handle is closed.
            for sig in signals.forever() {
                let kind = if sig == SIGWINCH { Signal::Resize } else { Signal::Stop };
                if tx.send(kind).is_err() {
                    break;
                }
            }
        });
        Ok(Self { rx, handle })
    }

    /// No handlers: the platform's defaults apply.
    #[cfg(not(unix))]
    pub fn new() -> io::Result<Self> {
        let (_tx, rx) = mpsc::unbounded_channel();
        Ok(Self { rx })
    }

    /// Resolves when any of them arrives.
    pub async fn recv(&mut self) -> Signal {
        match self.rx.recv().await {
            Some(s) => s,
            None => std::future::pending().await,
        }
    }
}

#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        self.handle.close();
    }
}
