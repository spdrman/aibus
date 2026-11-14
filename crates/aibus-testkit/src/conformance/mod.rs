//! The suite every provider adapter runs (issue #109, 04 section 2).
//!
//! An adapter's test builds its provider against a [`FakeServer`], lists its [`Case`]s (a
//! name, a [`Fixture`] to play and what the provider must do with it) and calls [`run`].
//! `run` insists on the full set of case names below, so a missing fixture is a red test and
//! not a quiet gap; a case an adapter genuinely cannot have goes in `Fixtures::skipped` with
//! the reason. No network, no keychain, no real provider.

pub mod server;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibus_core::Redactor;
use aibus_provider::fake::FAKE_KEY;
use aibus_provider::{
    AccountName, AuthMethodKind, CacheHint, ChatRequest, ContentBlock, Credential, Message,
    MessageRole, ModelId, Provider, ProviderErrorKind, RequestMetadata, Secret, StopReason,
    StreamChunk, ToolChoice, ToolSpec,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use server::{Body, End, FakeServer, Fixture, Seen};

use crate::{assert_no_secret_logged, capture_logs};

/// What a provider must do with a case's fixture.
pub enum Expect {
    /// The stream completes. `text`, when given, is the concatenated text deltas.
    Stream {
        text: Option<&'static str>,
        stop: StopReason,
    },
    /// The stream completes with these tool calls, in order, each arriving in at least
    /// `min_fragments` non-empty input fragments that reassemble to the recorded `input`.
    Tools {
        calls: Vec<(&'static str, Value)>,
        min_fragments: usize,
    },
    /// The call fails like this. `delivered` is text that must still have reached the caller
    /// before the failure.
    Fail {
        kind: ProviderErrorKind,
        retryable: bool,
        retry_after: Option<Duration>,
        delivered: Option<&'static str>,
    },
    /// `refresh_models` returns at least this many models.
    Models { at_least: usize },
    /// `verify` succeeds and lists this many models.
    Verify { models: usize },
    /// `count_tokens` is `None` (the fixture is never asked for) or this count.
    Tokens(Option<u64>),
    /// The server sends the fixture's frames and then holds the connection open (the fixture
    /// must end in `End::Hang`); a cancel then ends the call as `Cancelled` within 100 ms.
    Cancel,
}

pub struct Case {
    pub name: &'static str,
    pub fixture: Fixture,
    pub expect: Expect,
}

pub struct Fixtures {
    /// The model id the requests ask for.
    pub model: &'static str,
    /// Which credential kind the provider is run with.
    pub kind: AuthMethodKind,
    /// The header that carries the key and the text in front of it (`("x-api-key", "")`,
    /// `("authorization", "Bearer ")`), or `None` when the provider sends no key at all.
    pub auth: Option<(&'static str, &'static str)>,
    pub cases: Vec<Case>,
    /// Required case names this adapter cannot have, each with the reason.
    pub skipped: Vec<(&'static str, &'static str)>,
}

fn credential(kind: AuthMethodKind) -> Credential {
    Credential {
        account: AccountName("conformance".into()),
        secret: Secret::new(FAKE_KEY.to_owned()),
        kind,
        resolved_at: Instant::now(),
    }
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: ModelId(model.into()),
        system: "Be terse.".into(),
        messages: vec![Message::text(MessageRole::User, "hello")],
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            schema: json!({"type": "object"}),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 1024,
        temperature: None,
        effort: None,
        thinking: None,
        cache_hint: CacheHint::None,
        logprobs: false,
        metadata: RequestMetadata::default(),
    }
}

/// Runs every case against `p`, which must have been built with `server.uri()` as its base
/// URL. Panics on the first thing that breaks the contract, naming the case.
pub async fn run(p: Arc<dyn Provider>, server: &FakeServer, f: &Fixtures) {
    let required = [
        "text",
        "tool_use",
        "parallel_tools",
        "max_tokens",
        "refusal",
        "http_400_invalid",
        "http_400_context",
        "http_401",
        "http_403",
        "http_404",
        "http_413",
        "http_429_retry_after",
        "http_429",
        "http_500",
        "http_503",
        "http_529",
        "stream_error",
        "malformed_sse",
        "truncated",
        "cancel",
        "models",
        "verify",
        "count_tokens",
    ];
    let have: BTreeSet<&str> = f
        .cases
        .iter()
        .map(|c| c.name)
        .chain(f.skipped.iter().map(|(n, _)| *n))
        .collect();
    let missing: Vec<_> = required.iter().filter(|n| !have.contains(*n)).collect();
    assert!(missing.is_empty(), "the fixture set has no case for {missing:?} (add it, or list it in `skipped` with a reason)");
    assert!(
        f.skipped.iter().all(|(_, why)| !why.is_empty()),
        "a skipped case needs a reason"
    );

    let logs = capture_logs();

    // The redactor must leave every recorded response as it is: a pattern that eats a request
    // id would be a bug as much as one that misses a key.
    let redactor = Redactor::new();
    for c in &f.cases {
        let mut text: String = c
            .fixture
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\n"))
            .collect();
        text.push_str(&c.fixture.body_text());
        let out = redactor.redact(&text);
        assert!(out == text, "{}: the redactor changed the fixture", c.name);
    }

    for case in &f.cases {
        check_case(&p, server, f, case).await;
    }
    let text = logs.text();
    assert!(!text.contains(FAKE_KEY), "the key was logged:\n{text}");
}

/// One case, with the wire and log checks that go with every case. Needs a live
/// `capture_logs()` on the calling thread.
pub async fn check_case(p: &Arc<dyn Provider>, server: &FakeServer, f: &Fixtures, case: &Case) {
    let name = case.name;
    server.clear_script();
    let first = server.requests().len();
    if !matches!(case.expect, Expect::Tokens(None)) {
        server.script(case.fixture.clone());
    }
    let cred = credential(f.kind);

    match &case.expect {
        Expect::Stream { text, stop } => {
            let (result, chunks) = collect(p, &cred, request(f.model)).await;
            let resp = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            check_order(name, &chunks);
            assert_eq!(&resp.stop, stop, "{name}: stop reason");
            assert_eq!(
                chunks.last(),
                Some(&StreamChunk::Done(stop.clone())),
                "{name}: Done is the last chunk"
            );
            if let Some(text) = text {
                assert_eq!(texts(&chunks), *text, "{name}: text deltas");
            }
        }
        Expect::Tools {
            calls,
            min_fragments,
        } => {
            let (result, chunks) = collect(p, &cred, request(f.model)).await;
            let resp = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            check_order(name, &chunks);
            assert_eq!(resp.stop, StopReason::ToolUse, "{name}: stop reason");
            let starts: Vec<_> = chunks
                .iter()
                .filter_map(|c| {
                    if let StreamChunk::ToolUseStart { id, name } = c {
                        Some((*id, name.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(starts.len(), calls.len(), "{name}: tool calls started");
            let ids: BTreeSet<_> = starts.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids.len(),
                starts.len(),
                "{name}: two parallel calls share an id"
            );
            let blocks: Vec<_> = resp
                .message
                .blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse {
                        id, name, input, ..
                    } = b
                    {
                        Some((*id, name.clone(), input.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            for (i, ((id, started), (want_name, want_input))) in
                starts.iter().zip(calls).enumerate()
            {
                assert_eq!(started.as_str(), *want_name, "{name}: call {i} name");
                let fragments: Vec<&str> = chunks
                    .iter()
                    .filter_map(|c| match c {
                        StreamChunk::ToolUseInputDelta {
                            id: x,
                            json_fragment,
                        } if x == id && !json_fragment.is_empty() => Some(json_fragment.as_str()),
                        _ => None,
                    })
                    .collect();
                assert!(
                    fragments.len() >= *min_fragments,
                    "{name}: call {i} arrived in {} fragments, wanted {min_fragments}",
                    fragments.len()
                );
                let joined = fragments.concat();
                let streamed: Value = if joined.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&joined).unwrap_or_else(|e| {
                        panic!("{name}: call {i} fragments are not JSON ({e}): {joined}")
                    })
                };
                assert_eq!(&streamed, want_input, "{name}: call {i} streamed input");
                assert_eq!(
                    blocks.get(i),
                    Some(&(*id, started.clone(), want_input.clone())),
                    "{name}: call {i} in the response message"
                );
            }
        }
        Expect::Fail {
            kind,
            retryable,
            retry_after,
            delivered,
        } => {
            let (result, chunks) = collect(p, &cred, request(f.model)).await;
            let e = match result {
                Ok(r) => panic!("{name}: expected {kind:?}, got a response ({:?})", r.stop),
                Err(e) => e,
            };
            assert_eq!((e.kind, e.retryable), (*kind, *retryable), "{name}: {e}");
            assert_eq!(e.retry_after, *retry_after, "{name}: retry_after");
            assert!(
                !e.message.is_empty() && !e.message.contains(FAKE_KEY),
                "{name}: message {:?}",
                e.message
            );
            if let Some(d) = delivered {
                assert!(
                    chunks.contains(&StreamChunk::TextDelta((*d).into())),
                    "{name}: what arrived before the failure was still delivered"
                );
            }
        }
        Expect::Models { at_least } => {
            let listed = p
                .refresh_models(&cred)
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(listed.len() >= *at_least, "{name}: {} models", listed.len());
        }
        Expect::Verify { models } => {
            let info = p
                .verify(&cred)
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(info.models.len(), *models, "{name}: models in AccountInfo");
        }
        Expect::Tokens(want) => match (p.count_tokens(&cred, &request(f.model)).await, want) {
            (None, None) => {}
            (Some(Ok(n)), Some(w)) => assert_eq!(n, *w, "{name}: token count"),
            (got, want) => panic!("{name}: count_tokens gave {got:?}, wanted {want:?}"),
        },
        Expect::Cancel => {
            let hangs = server.hangs();
            let cancel = CancellationToken::new();
            let (tx, _rx) = mpsc::channel(256);
            let task = tokio::spawn({
                let (p, cancel, req) = (p.clone(), cancel.clone(), request(f.model));
                async move { p.stream(&cred, req, tx, cancel).await }
            });
            server.wait_hanging(hangs).await;
            let at = Instant::now();
            cancel.cancel();
            let done = tokio::time::timeout(Duration::from_secs(2), task).await;
            let waited = at.elapsed();
            let result = done
                .unwrap_or_else(|_| panic!("{name}: the call ignored the cancel"))
                .unwrap();
            let e = result
                .err()
                .unwrap_or_else(|| panic!("{name}: the call succeeded after a cancel"));
            assert_eq!(e.kind, ProviderErrorKind::Cancelled, "{name}: {e}");
            assert!(
                waited <= Duration::from_millis(100),
                "{name}: cancel took {waited:?}"
            );
        }
    }

    check_wire(server, first, f, name);
    assert_no_secret_logged();
}

async fn collect(
    p: &Arc<dyn Provider>,
    cred: &Credential,
    req: ChatRequest,
) -> (
    Result<aibus_provider::ChatResponse, aibus_provider::ProviderError>,
    Vec<StreamChunk>,
) {
    let (tx, mut rx) = mpsc::channel(256);
    let result = p.stream(cred, req, tx, CancellationToken::new()).await;
    let mut chunks = Vec::new();
    while let Ok(c) = rx.try_recv() {
        chunks.push(c);
    }
    (result, chunks)
}

fn texts(chunks: &[StreamChunk]) -> String {
    chunks
        .iter()
        .filter_map(|c| {
            if let StreamChunk::TextDelta(t) = c {
                Some(t.as_str())
            } else {
                None
            }
        })
        .collect()
}

/// The order 04 section 2 promises: every tool call starts, gets its fragments, then ends;
/// usage comes before Done; Done is there once, at the end.
fn check_order(name: &str, chunks: &[StreamChunk]) {
    let mut open = BTreeSet::new();
    for c in chunks {
        match c {
            StreamChunk::ToolUseStart { id, .. } => {
                assert!(open.insert(*id), "{name}: tool call started twice")
            }
            StreamChunk::ToolUseInputDelta { id, .. } => assert!(
                open.contains(id),
                "{name}: input fragment outside its tool call"
            ),
            StreamChunk::ToolUseEnd { id } => {
                assert!(open.remove(id), "{name}: ToolUseEnd without a start")
            }
            _ => {}
        }
    }
    assert!(
        open.is_empty(),
        "{name}: a tool call never got its ToolUseEnd"
    );
    let done: Vec<_> = chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, StreamChunk::Done(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        done,
        [chunks.len().saturating_sub(1)],
        "{name}: Done exactly once, last"
    );
    if let Some(u) = chunks
        .iter()
        .position(|c| matches!(c, StreamChunk::Usage(_)))
    {
        assert!(u + 1 < chunks.len(), "{name}: usage after Done");
    }
}

/// What went over the wire since `first`: the key only in the provider's one auth header,
/// never in a URL, a body or any other header.
fn check_wire(server: &FakeServer, first: usize, f: &Fixtures, name: &str) {
    for r in &server.requests()[first..] {
        assert!(
            !r.target.contains(FAKE_KEY),
            "{name}: the key is in the URL"
        );
        assert!(
            !String::from_utf8_lossy(&r.body).contains(FAKE_KEY),
            "{name}: the key is in the body"
        );
        let carriers: Vec<&str> = r
            .headers
            .iter()
            .filter(|(_, v)| v.contains(FAKE_KEY))
            .map(|(k, _)| k.as_str())
            .collect();
        match f.auth {
            Some((header, prefix)) => {
                assert_eq!(
                    carriers,
                    [header],
                    "{name}: the key travels in `{header}` only"
                );
                assert_eq!(
                    r.header(header),
                    Some(format!("{prefix}{FAKE_KEY}").as_str()),
                    "{name}: {header} value"
                );
            }
            None => assert!(
                carriers.is_empty(),
                "{name}: this provider sends no key, but {carriers:?} carries it"
            ),
        }
    }
}
