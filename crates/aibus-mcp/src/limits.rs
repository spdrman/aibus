//! What one client may ask of the server: how many calls a minute, and how big a call.

use std::time::Instant;

/// Tool calls per minute, per process (the epic's security model).
pub const CALLS_PER_MINUTE: u32 = 60;

/// The most bytes of arguments one call may carry.
pub const MAX_ARGS_BYTES: usize = 256 * 1024;

/// Writes per minute, per process (`workflow_create` and `workflow_update` share the bucket).
pub const WRITES_PER_MINUTE: u32 = 10;

/// Run starts per minute, per process.
pub const RUNS_PER_MINUTE: u32 = 5;

/// A token bucket: `per_minute` tokens, refilled evenly. The caller says what time it is, so a
/// test can move the clock without sleeping.
pub struct RateLimiter {
    per_minute: u32,
    tokens: f64,
    at: Instant,
}

impl RateLimiter {
    pub fn new(per_minute: u32, now: Instant) -> Self {
        Self { per_minute, tokens: f64::from(per_minute), at: now }
    }

    /// Takes a token, or says in how many whole seconds there will be one.
    pub fn take(&mut self, now: Instant) -> Result<(), u64> {
        let per_second = f64::from(self.per_minute) / 60.0;
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = self.at.max(now);
        self.tokens = (self.tokens + elapsed * per_second).min(f64::from(self.per_minute));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        Err(((1.0 - self.tokens) / per_second).ceil().max(1.0) as u64)
    }
}
