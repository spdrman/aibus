//! Per-account rate limits (04 section 4.2): a token bucket that gates `stream`.
//!
//! Time is tokio's clock, so a test that pauses it drives the whole thing without sleeping.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;

use crate::error::{ProviderError, ProviderErrorKind, RateLimitInfo};
use crate::provider::*;
use crate::secret::Credential;
use crate::types::*;

/// The window the per-minute limits count over, and how long a 429 keeps the bucket tight.
const MINUTE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimits {
    pub max_concurrent: u32,
    pub requests_per_minute: Option<u32>,
    pub tokens_per_minute: Option<u32>,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            requests_per_minute: None,
            tokens_per_minute: None,
        }
    }
}

#[derive(Default)]
struct State {
    active: u32,
    /// When each request in the last minute started.
    starts: VecDeque<Instant>,
    /// Tokens spent in the last minute, by when.
    spent: VecDeque<(Instant, u64)>,
    /// No request starts before this (a 429's `retry_after`).
    blocked_until: Option<Instant>,
    /// Until then every limit is halved.
    tight_until: Option<Instant>,
}

/// One account's bucket. Share it as an `Arc`.
pub struct Limiter {
    account: AccountName,
    limits: RateLimits,
    state: Mutex<State>,
    changed: Notify,
}

/// A slot. Dropping it frees the slot and wakes a waiter.
pub struct Permit(Arc<Limiter>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.lock().active -= 1;
        self.0.changed.notify_waiters();
    }
}

impl Limiter {
    pub fn new(account: AccountName, limits: RateLimits) -> Arc<Self> {
        Arc::new(Self {
            account,
            limits,
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes a slot now, or says when to look again (`None`: when a slot is freed).
    fn try_take(&self) -> Result<(), Option<Instant>> {
        let now = Instant::now();
        let mut st = self.lock();
        while st.starts.front().is_some_and(|t| now >= *t + MINUTE) {
            st.starts.pop_front();
        }
        while st.spent.front().is_some_and(|(t, _)| now >= *t + MINUTE) {
            st.spent.pop_front();
        }
        if let Some(t) = st.blocked_until.filter(|t| *t > now) {
            return Err(Some(t));
        }
        // A tight bucket allows half of everything, and never less than one.
        let tight = st.tight_until.is_some_and(|t| t > now);
        let scale = |n: u32| if tight { (n / 2).max(1) } else { n };
        if st.active >= scale(self.limits.max_concurrent.max(1)) {
            return Err(None);
        }
        if let Some(rpm) = self.limits.requests_per_minute.map(scale) {
            if st.starts.len() >= rpm as usize {
                return Err(Some(st.starts[st.starts.len() - rpm as usize] + MINUTE));
            }
        }
        if let Some(tpm) = self.limits.tokens_per_minute.map(scale) {
            let mut total: u64 = st.spent.iter().map(|(_, n)| n).sum();
            if total >= u64::from(tpm) {
                // Wait until enough of the oldest spending has aged out.
                for (t, n) in &st.spent {
                    total -= n;
                    if total < u64::from(tpm) {
                        return Err(Some(*t + MINUTE));
                    }
                }
            }
        }
        st.active += 1;
        st.starts.push_back(now);
        Ok(())
    }

    /// Waits for a slot. `on_wait` runs once, with the status text, if it has to wait.
    pub async fn acquire(self: &Arc<Self>, on_wait: impl FnOnce(&str)) -> Permit {
        let mut on_wait = Some(on_wait);
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            // Registered before the check, so a slot freed in between is not missed.
            notified.as_mut().enable();
            let wake = match self.try_take() {
                Ok(()) => return Permit(self.clone()),
                Err(wake) => wake,
            };
            if let Some(f) = on_wait.take() {
                f(&format!("waiting for {} slot", self.account));
            }
            match wake {
                Some(t) => tokio::select! {
                    () = &mut notified => {}
                    () = sleep_until(t) => {}
                },
                None => notified.await,
            }
        }
    }

    /// A 429: nothing starts for `retry_after`, and for the next minute the limits are halved.
    pub fn tighten(&self, retry_after: Duration) {
        let now = Instant::now();
        let mut st = self.lock();
        st.blocked_until = Some(now + retry_after);
        st.tight_until = Some(now + MINUTE);
        drop(st);
        self.changed.notify_waiters();
    }

    /// What a response said about the windows: zero requests or tokens remaining blocks until
    /// the reset, the same way a 429 does, but without halving the limits (nothing went wrong).
    /// A response that says anything else teaches the bucket nothing.
    pub fn observe(&self, info: &RateLimitInfo) {
        let exhausted = info.requests_remaining == Some(0) || info.tokens_remaining == Some(0);
        let Some(reset) = info.resets_in.filter(|_| exhausted) else {
            return;
        };
        let until = Instant::now() + reset;
        let mut st = self.lock();
        st.blocked_until = Some(st.blocked_until.map_or(until, |t| t.max(until)));
        drop(st);
        self.changed.notify_waiters();
    }

    /// Counts tokens against `tokens_per_minute` once a response says what it cost.
    pub fn spend(&self, tokens: u64) {
        self.lock().spent.push_back((Instant::now(), tokens));
    }
}

/// An account's view of a provider: `stream` waits for a slot first, tells the run when it is
/// waiting, and tightens the bucket on a 429. Everything else passes straight through.
pub struct Gated {
    inner: Arc<dyn Provider>,
    limiter: Arc<Limiter>,
}

impl Gated {
    pub fn new(inner: Arc<dyn Provider>, limiter: Arc<Limiter>) -> Self {
        Self { inner, limiter }
    }
}

#[async_trait]
impl Provider for Gated {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn capabilities_for(&self, model: &ModelId) -> Capabilities {
        self.inner.capabilities_for(model)
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.inner.models()
    }

    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        self.inner.refresh_models(creds).await
    }

    fn resolve_model(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId> {
        self.inner.resolve_model(r, aliases)
    }

    async fn stream(
        &self,
        creds: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError> {
        let waited = AtomicBool::new(false);
        let say = |text: &str| {
            // A full channel only loses a status line.
            let _ = out.try_send(StreamChunk::Waiting(text.to_owned()));
        };
        let permit = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Err(ProviderError::new(ProviderErrorKind::Cancelled, "cancelled while waiting for a slot"));
            }
            p = self.limiter.acquire(|text| {
                waited.store(true, Ordering::Relaxed);
                say(text);
            }) => p,
        };
        if waited.load(Ordering::Relaxed) {
            say("");
        }
        let result = self.inner.stream(creds, req, out.clone(), cancel).await;
        match &result {
            Err(e) => {
                if let Some(info) = &e.rate_limit {
                    self.limiter.observe(info);
                }
                if e.kind == ProviderErrorKind::RateLimit {
                    if let Some(d) = e.retry_after {
                        self.limiter.tighten(d);
                    }
                }
            }
            Ok(r) => {
                if let Some(info) = &r.rate_limit {
                    self.limiter.observe(info);
                }
                self.limiter.spend(r.usage.input_tokens + r.usage.output_tokens);
            }
        }
        drop(permit);
        result
    }

    async fn count_tokens(
        &self,
        creds: &Credential,
        req: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>> {
        self.inner.count_tokens(creds, req).await
    }

    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError> {
        self.inner.verify(creds).await
    }

    fn price(&self, model: &ModelId) -> Option<Price> {
        self.inner.price(model)
    }

    fn estimate_tokens(&self, req: &ChatRequest) -> u64 {
        self.inner.estimate_tokens(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeProvider, Script, Step, FAKE_KEY};
    use crate::secret::{AuthMethodKind, Secret};

    fn acct(n: &str) -> AccountName {
        AccountName(n.into())
    }

    fn limits(max_concurrent: u32) -> RateLimits {
        RateLimits {
            max_concurrent,
            ..RateLimits::default()
        }
    }

    /// Polls `fut` once on the paused clock: `Some` when it is already done.
    async fn ready<T>(fut: impl std::future::Future<Output = T>) -> Option<T> {
        tokio::time::timeout(Duration::from_millis(1), fut).await.ok()
    }

    #[tokio::test(start_paused = true)]
    async fn the_fifth_concurrent_request_waits_for_a_slot_and_says_so() {
        let lim = Limiter::new(acct("work"), limits(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(ready(lim.acquire(|_| panic!("the first four do not wait"))).await.unwrap());
        }
        let said = Mutex::new(Vec::new());
        assert!(ready(lim.acquire(|t| said.lock().unwrap().push(t.to_owned()))).await.is_none());
        assert_eq!(*said.lock().unwrap(), ["waiting for work slot"]);

        let fifth = tokio::spawn({
            let lim = lim.clone();
            async move { lim.acquire(|_| {}).await }
        });
        tokio::task::yield_now().await;
        assert!(!fifth.is_finished());
        held.pop();
        let _permit = fifth.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_429_blocks_for_retry_after_then_halves_the_limits_for_a_minute() {
        let lim = Limiter::new(acct("work"), limits(4));
        let t0 = Instant::now();
        lim.tighten(Duration::from_secs(10));

        let _p = lim.acquire(|_| {}).await;
        assert_eq!(Instant::now() - t0, Duration::from_secs(10));

        // 4 / 2 = 2 slots while tight; one is taken already.
        let _q = ready(lim.acquire(|_| {})).await.expect("second slot");
        assert!(ready(lim.acquire(|_| {})).await.is_none(), "third is over the halved limit");

        tokio::time::sleep_until(t0 + MINUTE).await;
        let _r = ready(lim.acquire(|_| {})).await.expect("tightening is over");
        let _s = ready(lim.acquire(|_| {})).await.expect("full limit again");
    }

    #[tokio::test(start_paused = true)]
    async fn requests_per_minute_counts_over_a_sliding_minute() {
        let lim = Limiter::new(
            acct("work"),
            RateLimits {
                requests_per_minute: Some(2),
                ..RateLimits::default()
            },
        );
        let t0 = Instant::now();
        drop(lim.acquire(|_| {}).await);
        tokio::time::sleep(Duration::from_secs(20)).await;
        drop(lim.acquire(|_| {}).await);
        drop(lim.acquire(|_| {}).await);
        assert_eq!(Instant::now() - t0, MINUTE, "the third waits for the first to age out");
    }

    #[tokio::test(start_paused = true)]
    async fn tokens_per_minute_counts_what_responses_cost() {
        let lim = Limiter::new(
            acct("work"),
            RateLimits {
                tokens_per_minute: Some(1000),
                ..RateLimits::default()
            },
        );
        let t0 = Instant::now();
        drop(lim.acquire(|_| {}).await);
        lim.spend(1500);
        drop(lim.acquire(|_| {}).await);
        assert_eq!(Instant::now() - t0, MINUTE);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_requests_remaining_blocks_until_the_reset_without_tightening() {
        let lim = Limiter::new(acct("work"), limits(4));
        let t0 = Instant::now();
        lim.observe(&RateLimitInfo {
            requests_remaining: Some(0),
            resets_in: Some(Duration::from_secs(12)),
            ..RateLimitInfo::default()
        });
        drop(lim.acquire(|_| {}).await);
        assert_eq!(Instant::now() - t0, Duration::from_secs(12));
        // Not a 429: all four slots are there again straight away.
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(ready(lim.acquire(|_| {})).await.expect("full limit, not halved"));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn zero_tokens_remaining_blocks_too_and_a_later_reset_wins() {
        let lim = Limiter::new(acct("work"), limits(4));
        let t0 = Instant::now();
        lim.observe(&RateLimitInfo {
            tokens_remaining: Some(0),
            resets_in: Some(Duration::from_secs(30)),
            ..RateLimitInfo::default()
        });
        lim.observe(&RateLimitInfo {
            requests_remaining: Some(0),
            resets_in: Some(Duration::from_secs(5)),
            ..RateLimitInfo::default()
        });
        drop(lim.acquire(|_| {}).await);
        assert_eq!(Instant::now() - t0, Duration::from_secs(30), "the shorter reset does not shorten the block");
    }

    #[tokio::test(start_paused = true)]
    async fn headers_with_room_left_or_no_reset_block_nothing() {
        let lim = Limiter::new(acct("work"), limits(1));
        lim.observe(&RateLimitInfo {
            requests_remaining: Some(7),
            tokens_remaining: Some(900),
            resets_in: Some(Duration::from_secs(60)),
            ..RateLimitInfo::default()
        });
        lim.observe(&RateLimitInfo {
            requests_remaining: Some(0),
            ..RateLimitInfo::default()
        });
        ready(lim.acquire(|_| {})).await.expect("nothing blocks");
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_blocks_on_the_window_headers_of_an_error() {
        let mut err = ProviderError::new(ProviderErrorKind::Server, "boom");
        err.rate_limit = Some(RateLimitInfo {
            requests_remaining: Some(0),
            resets_in: Some(Duration::from_secs(20)),
            ..RateLimitInfo::default()
        });
        let limiter = Limiter::new(acct("work"), limits(4));
        let gated = Gated::new(Arc::new(FakeProvider::new(Script::new().then(Step::Error(err)))), limiter.clone());
        let (tx, _rx) = mpsc::channel(8);
        let t0 = Instant::now();
        gated.stream(&creds(), request(), tx, CancellationToken::new()).await.unwrap_err();
        drop(limiter.acquire(|_| {}).await);
        assert_eq!(Instant::now() - t0, Duration::from_secs(20));
    }

    fn creds() -> Credential {
        Credential {
            account: acct("work"),
            secret: Secret::new(FAKE_KEY.to_owned()),
            kind: AuthMethodKind::ApiKey,
            resolved_at: std::time::Instant::now(),
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: ModelId("fake-small".into()),
            system: String::new(),
            messages: vec![Message::text(MessageRole::User, "hi")],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 100,
            temperature: None,
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_tells_the_second_run_it_is_waiting_and_clears_it_after() {
        let script = Script::new()
            .then(Step::Delay(Duration::from_secs(5)))
            .then(Step::Text("one".into()))
            .then(Step::Stop(StopReason::EndTurn))
            .then(Step::Text("two".into()));
        let gated = Arc::new(Gated::new(
            Arc::new(FakeProvider::new(script)),
            Limiter::new(acct("work"), limits(1)),
        ));
        let go = |g: Arc<Gated>| async move {
            let (tx, mut rx) = mpsc::channel(64);
            let r = g.stream(&creds(), request(), tx, CancellationToken::new()).await;
            let mut chunks = Vec::new();
            while let Ok(c) = rx.try_recv() {
                chunks.push(c);
            }
            (r, chunks)
        };
        let first = tokio::spawn(go(gated.clone()));
        tokio::task::yield_now().await;
        let second = tokio::spawn(go(gated.clone()));
        let (_, a) = first.await.unwrap();
        let (r, b) = second.await.unwrap();
        r.unwrap();
        assert!(!a.iter().any(|c| matches!(c, StreamChunk::Waiting(_))));
        let waits: Vec<_> = b
            .iter()
            .filter_map(|c| match c {
                StreamChunk::Waiting(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(waits, ["waiting for work slot", ""]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_tightens_on_a_429_that_carries_retry_after() {
        let mut err = ProviderError::new(ProviderErrorKind::RateLimit, "slow down");
        err.retry_after = Some(Duration::from_secs(30));
        let script = Script::new().then(Step::Error(err));
        let limiter = Limiter::new(acct("work"), limits(4));
        let gated = Gated::new(Arc::new(FakeProvider::new(script)), limiter.clone());
        let (tx, _rx) = mpsc::channel(8);
        let t0 = Instant::now();
        let e = gated
            .stream(&creds(), request(), tx, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::RateLimit);
        drop(limiter.acquire(|_| {}).await);
        assert_eq!(Instant::now() - t0, Duration::from_secs(30));
    }
}
