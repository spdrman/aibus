//! What the lead's run needs to consult the advisor and the router (01 section 7): their models
//! and accounts, their settings, and the ledger every run of the session writes to.
//!
//! An `Assist` is built at startup and shared by every run's `RunDeps`. Only the lead consults
//! anyone; the other runs just keep the ledger.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use aibus_provider::{ChatRequest, ChatResponse, Provider, ProviderError, ProviderErrorKind, StreamChunk};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::advisor::Ledger;
use crate::agent::AgentDef;
use crate::credentials::CredentialSource;
use crate::ids::{AccountName, ForkId, ModelId};
use crate::run::STREAM_CAPACITY;

/// A model on an account: how the advisor and the router reach theirs. Each has its own, apart
/// from the lead's.
pub struct Seat {
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<dyn CredentialSource>,
    pub model: ModelId,
    pub account: AccountName,
}

pub struct AdvisorSeat {
    pub seat: Seat,
    /// The `advisor` definition: its prompt, tool list and custom checkpoints.
    pub def: AgentDef,
    /// The built-in checkpoints `[advisor] checkpoints` leaves on.
    pub enabled: Vec<String>,
    pub packet_max_tokens: u64,
}

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
}

impl Assist {
    pub fn new(advisor: Option<AdvisorSeat>, router: Option<RouterSeat>) -> Arc<Self> {
        Arc::new(Self {
            advisor,
            router,
            ledger: Ledger::default(),
            forks: AtomicU64::new(0),
        })
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
