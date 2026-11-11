//! What the lead's run needs to consult the advisor and the router (01 section 7): their models
//! and accounts, their settings, and the ledger every run of the session writes to.
//!
//! An `Assist` is built at startup and shared by every run's `RunDeps`. Only the lead consults
//! anyone; the other runs just keep the ledger.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use aibus_provider::{ChatRequest, ChatResponse, Provider, ProviderError, ProviderErrorKind, StreamChunk};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::advisor::Ledger;
use crate::agent::AgentDef;
use crate::credentials::CredentialSource;
use crate::ids::{AccountName, ForkId, ModelId};
use crate::msg::ModelTarget;
use crate::run::STREAM_CAPACITY;

/// A model on an account: how the advisor and the router reach theirs. Each has its own, apart
/// from the lead's.
#[derive(Clone)]
pub struct Seat {
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<dyn CredentialSource>,
    pub model: ModelId,
    pub account: AccountName,
}

#[derive(Clone)]
pub struct AdvisorSeat {
    pub seat: Seat,
    /// The `advisor` definition: its prompt, tool list and custom checkpoints.
    pub def: AgentDef,
    /// The built-in checkpoints `[advisor] checkpoints` leaves on.
    pub enabled: Vec<String>,
    pub packet_max_tokens: u64,
}

#[derive(Clone)]
pub struct RouterSeat {
    pub seat: Seat,
    /// The `router` definition's prompt.
    pub system: String,
    pub threshold: f32,
    pub max_per_turn: u32,
    /// `router.label`: what the notes the router leaves in the conversation call it.
    pub label: String,
}

pub struct Assist {
    pub advisor: Option<AdvisorSeat>,
    pub router: Option<RouterSeat>,
    pub ledger: Ledger,
    forks: AtomicU64,
    /// What `/advisor` and `/router` chose, over the seats above: `None` is no choice yet,
    /// `Some(None)` is the seat turned off.
    chosen: Mutex<(Option<Option<Seat>>, Option<Option<Seat>>)>,
}

impl Assist {
    pub fn new(advisor: Option<AdvisorSeat>, router: Option<RouterSeat>) -> Arc<Self> {
        Arc::new(Self {
            advisor,
            router,
            ledger: Ledger::default(),
            forks: AtomicU64::new(0),
            chosen: Mutex::default(),
        })
    }

    /// `/advisor` or `/router`: from the next consultation on, the seat uses `seat`, or is off
    /// for `None`. A seat the session started without cannot be switched on this way, since it
    /// has no definition to speak from.
    pub fn switch(&self, target: ModelTarget, seat: Option<Seat>) {
        let mut g = self.chosen.lock().unwrap_or_else(PoisonError::into_inner);
        match target {
            ModelTarget::Advisor => g.0 = Some(seat),
            ModelTarget::Router => g.1 = Some(seat),
            ModelTarget::Lead | ModelTarget::Run(_) => {}
        }
    }

    /// The advisor as it is now: its definition with the model it was last switched to, or
    /// `None` when it was never configured or has been turned off.
    pub fn advisor_now(&self) -> Option<AdvisorSeat> {
        let base = self.advisor.as_ref()?;
        match &self.chosen.lock().unwrap_or_else(PoisonError::into_inner).0 {
            None => Some(base.clone()),
            Some(None) => None,
            Some(Some(seat)) => Some(AdvisorSeat { seat: seat.clone(), ..base.clone() }),
        }
    }

    pub fn router_now(&self) -> Option<RouterSeat> {
        let base = self.router.as_ref()?;
        match &self.chosen.lock().unwrap_or_else(PoisonError::into_inner).1 {
            None => Some(base.clone()),
            Some(None) => None,
            Some(Some(seat)) => Some(RouterSeat { seat: seat.clone(), ..base.clone() }),
        }
    }

    /// The next fork number: the session's lifetime count of router questions.
    pub fn next_fork(&self) -> ForkId {
        ForkId(self.forks.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// One completion on `seat`. Returns the response and the logprobs the provider streamed.
pub async fn complete(
    seat: &Seat,
    req: ChatRequest,
    cancel: &CancellationToken,
) -> Result<(ChatResponse, Vec<(String, f32)>), ProviderError> {
    let creds = seat
        .creds
        .credential()
        .map_err(|e| ProviderError::new(ProviderErrorKind::Auth, e.to_string()))?;
    let (out, mut chunks) = mpsc::channel(STREAM_CAPACITY);
    let call = seat.provider.stream(&creds, req, out, cancel.clone());
    tokio::pin!(call);
    let mut logprobs = Vec::new();
    let mut keep = |chunk| {
        if let StreamChunk::Logprob { token, logprob } = chunk {
            logprobs.push((token, logprob));
        }
    };
    let resp = loop {
        tokio::select! {
            r = &mut call => break r,
            Some(chunk) = chunks.recv() => keep(chunk),
        }
    };
    while let Ok(chunk) = chunks.try_recv() {
        keep(chunk);
    }
    resp.map(|r| (r, logprobs))
}
