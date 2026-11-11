//! The fake provider, its clock, and the no-secret-in-logs guard (10 section 3, 04 section 7).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aibus_provider::fake::{Factory, FakeProvider, Script, Step, FAKE_KEY};
use aibus_provider::*;
use aibus_testkit::{
    assert_no_secret_logged, capture_logs, credential, fake_credential, FakeClock,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn request(system: &str, user: &str) -> ChatRequest {
    ChatRequest {
        model: ModelId("fake-small".into()),
        system: system.to_owned(),
        messages: vec![Message::text(MessageRole::User, user)],
        tools: vec![],
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

async fn run(
    fake: &FakeProvider,
    creds: &Credential,
    req: ChatRequest,
) -> (Result<ChatResponse, ProviderError>, Vec<StreamChunk>) {
    let (tx, mut rx) = mpsc::channel(1024);
    let result = fake.stream(creds, req, tx, CancellationToken::new()).await;
    let mut chunks = vec![];
    while let Ok(c) = rx.try_recv() {
        chunks.push(c);
    }
    (result, chunks)
}

fn fixture(name: &str) -> Script {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/scripts")
        .join(name);
    Script::load(&path).unwrap()
}

#[tokio::test]
async fn text_streams_in_8_byte_chunks_without_splitting_characters() {
    let _logs = capture_logs();
    let fake =
        FakeProvider::new(Script::new().then(Step::Text("hello wörld, twenty-two b".into())));
    let (res, chunks) = run(&fake, &fake_credential(), request("", "hi")).await;

    let deltas: Vec<&str> = chunks
        .iter()
        .filter_map(|c| match c {
            StreamChunk::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(deltas.iter().all(|d| d.len() <= 8), "{deltas:?}");
    assert_eq!(deltas[0], "hello w"); // the 2-byte "ö" would straddle byte 8, so it moves on
    assert_eq!(deltas.concat(), "hello wörld, twenty-two b");
    assert_eq!(chunks.last(), Some(&StreamChunk::Done(StopReason::EndTurn)));

    let res = res.unwrap();
    assert_eq!(res.stop, StopReason::EndTurn);
    assert_eq!(res.message.plain_text(), "hello wörld, twenty-two b");
}

#[tokio::test]
async fn tool_call_streams_start_input_end_and_stops_with_tool_use() {
    let script = Script::new()
        .then(Step::Text("checking".into()))
        .then(Step::ToolCall {
            name: "read".into(),
            input: serde_json::json!({"path": "a.rs"}),
        })
        .then(Step::Usage {
            input: 10,
            output: 4,
        })
        .then(Step::Usage {
            input: 5,
            output: 1,
        });
    let fake = FakeProvider::new(script);
    let (res, chunks) = run(&fake, &fake_credential(), request("", "go")).await;
    let res = res.unwrap();

    assert_eq!(res.stop, StopReason::ToolUse);
    assert_eq!((res.usage.input_tokens, res.usage.output_tokens), (15, 5));
    let ContentBlock::ToolUse { id, name, input, .. } = &res.message.blocks[1] else {
        panic!("{:?}", res.message)
    };
    assert_eq!(name, "read");
    assert_eq!(input["path"], "a.rs");
    assert!(chunks.contains(&StreamChunk::ToolUseStart {
        id: *id,
        name: "read".into()
    }));
    assert!(chunks.contains(&StreamChunk::ToolUseInputDelta {
        id: *id,
        json_fragment: r#"{"path":"a.rs"}"#.into()
    }));
    assert!(chunks.contains(&StreamChunk::ToolUseEnd { id: *id }));
    assert!(matches!(res.message.blocks[0], ContentBlock::Text(_)));
}

#[tokio::test]
async fn stop_overrides_the_default_reason() {
    let fake = FakeProvider::new(
        Script::new()
            .then(Step::Text("cut".into()))
            .then(Step::Stop(StopReason::MaxTokens)),
    );
    let (res, chunks) = run(&fake, &fake_credential(), request("", "x")).await;
    assert_eq!(res.unwrap().stop, StopReason::MaxTokens);
    assert_eq!(
        chunks.last(),
        Some(&StreamChunk::Done(StopReason::MaxTokens))
    );
}

#[tokio::test]
async fn error_step_fails_the_turn_with_the_scripted_error() {
    let mut err = ProviderError::new(ProviderErrorKind::RateLimit, "slow down");
    err.retry_after = Some(Duration::from_secs(30));
    let fake = FakeProvider::new(Script::new().then(Step::Error(err)));
    let (res, _) = run(&fake, &fake_credential(), request("", "x")).await;
    let e = res.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::RateLimit);
    assert!(e.retryable);
    assert_eq!(e.retry_after, Some(Duration::from_secs(30)));
}

#[tokio::test]
async fn an_empty_script_is_an_error_not_a_silent_empty_reply() {
    let fake = FakeProvider::new(Script::new());
    let (res, _) = run(&fake, &fake_credential(), request("", "x")).await;
    assert!(res.unwrap_err().message.contains("exhausted"));
}

#[tokio::test]
async fn match_prompt_branches_on_system_and_messages() {
    let script = || {
        Script::new()
            .then(Step::MatchPrompt {
                contains: "needle".into(),
                then: vec![
                    Step::Text("found".into()),
                    Step::Usage {
                        input: 1,
                        output: 1,
                    },
                ],
            })
            .then(Step::Text("default".into()))
    };
    let fake = FakeProvider::new(script());
    let (res, _) = run(&fake, &fake_credential(), request("", "a needle here")).await;
    assert_eq!(res.unwrap().message.plain_text(), "founddefault");

    let fake = FakeProvider::new(script());
    let (res, _) = run(
        &fake,
        &fake_credential(),
        request("system needle", "nothing"),
    )
    .await;
    assert_eq!(res.unwrap().message.plain_text(), "founddefault");

    let fake = FakeProvider::new(script());
    let (res, _) = run(&fake, &fake_credential(), request("", "nothing")).await;
    assert_eq!(res.unwrap().message.plain_text(), "default");
}

#[tokio::test]
async fn router_answer_is_json_with_a_logprob_only_when_asked() {
    let fake = FakeProvider::new(Script::new().then(Step::RouterAnswer {
        choice: "src/auth.ts".into(),
        p: 0.5,
    }));
    let mut req = request("", "which file?");
    req.logprobs = true;
    let (res, chunks) = run(&fake, &fake_credential(), req).await;
    let v: serde_json::Value = serde_json::from_str(&res.unwrap().message.plain_text()).unwrap();
    assert_eq!(v["choice"], "src/auth.ts");
    assert_eq!(v["p"], 0.5);
    let lp = chunks.iter().find_map(|c| match c {
        StreamChunk::Logprob { token, logprob } => Some((token.clone(), *logprob)),
        _ => None,
    });
    assert_eq!(lp, Some(("src/auth.ts".to_owned(), 0.5f32.ln())));

    let fake = FakeProvider::new(Script::new().then(Step::RouterAnswer {
        choice: "x".into(),
        p: 0.9,
    }));
    let (_, chunks) = run(&fake, &fake_credential(), request("", "q")).await;
    assert!(!chunks
        .iter()
        .any(|c| matches!(c, StreamChunk::Logprob { .. })));
}

#[tokio::test]
async fn delay_runs_on_the_fake_clock() {
    let clock = FakeClock::new();
    let fake = FakeProvider::new(
        Script::new()
            .then(Step::Delay(Duration::from_secs(3600)))
            .then(Step::Text("late".into())),
    );
    let wall = Instant::now();
    let (res, _) = run(&fake, &fake_credential(), request("", "x")).await;
    assert_eq!(res.unwrap().message.plain_text(), "late");
    assert!(clock.elapsed() >= Duration::from_secs(3600));
    assert!(
        wall.elapsed() < Duration::from_millis(100),
        "took {:?}",
        wall.elapsed()
    );
}

#[tokio::test]
async fn five_30s_backoffs_finish_under_100ms_wall_time() {
    let clock = FakeClock::new();
    let mut script = Script::new();
    for _ in 0..5 {
        script = script.then(Step::Error(ProviderError::new(
            ProviderErrorKind::Overloaded,
            "529",
        )));
    }
    let fake = FakeProvider::new(script.then(Step::Text("ok".into())));
    let creds = fake_credential();

    let wall = Instant::now();
    let mut attempts = 0;
    let reply = loop {
        attempts += 1;
        match run(&fake, &creds, request("", "x")).await.0 {
            Ok(r) => break r,
            Err(e) => {
                assert!(e.retryable);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    assert_eq!(attempts, 6);
    assert_eq!(reply.message.plain_text(), "ok");
    assert!(clock.elapsed() >= Duration::from_secs(150));
    assert!(
        wall.elapsed() < Duration::from_millis(100),
        "took {:?}",
        wall.elapsed()
    );
}

#[tokio::test]
async fn ron_scripts_load_and_run() {
    let fake = FakeProvider::new(fixture("tool_then_answer.ron"));
    let creds = fake_credential();

    let (first, _) = run(&fake, &creds, request("", "look at the repo")).await;
    let first = first.unwrap();
    assert_eq!(first.stop, StopReason::ToolUse);
    let ContentBlock::ToolUse { input, .. } = &first.message.blocks[1] else {
        panic!()
    };
    assert_eq!(input["lines"][1], 20);

    let (second, _) = run(&fake, &creds, request("", "tool said: fn main() {}")).await;
    assert_eq!(
        second.unwrap().message.plain_text(),
        "It prints the version."
    );
    assert_eq!(fake.script().lock().unwrap().remaining(), 0);

    let clock = FakeClock::new();
    let fake = FakeProvider::new(fixture("rate_limited.ron"));
    let e = run(&fake, &creds, request("", "x")).await.0.unwrap_err();
    assert_eq!(
        (e.kind, e.retryable, e.retry_after),
        (
            ProviderErrorKind::RateLimit,
            true,
            Some(Duration::from_secs(30))
        )
    );
    let ok = run(&fake, &creds, request("", "x")).await.0.unwrap();
    assert_eq!(ok.message.plain_text(), "recovered");
    assert!(clock.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn bad_ron_names_the_file() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let err = Script::load(&dir).unwrap_err();
    assert!(err.contains("Cargo.toml"), "{err}");
}

#[tokio::test]
async fn several_accounts_keep_distinct_keys_and_requests_are_recorded_redacted() {
    let logs = capture_logs();
    logs.forbid("key-work-1111");
    logs.forbid("key-personal-2222");
    let fake = FakeProvider::new(
        Script::new()
            .then(Step::Text("a".into()))
            .then(Step::Stop(StopReason::EndTurn))
            .then(Step::Text("b".into()))
            .then(Step::Stop(StopReason::EndTurn)),
    )
    .with_account("work", "key-work-1111")
    .with_account("personal", "key-personal-2222");

    // A user pasted a key into the prompt: the recording must not keep it.
    let leaky = format!("my key is {FAKE_KEY} and key-work-1111");
    let (a, _) = run(
        &fake,
        &credential("work", "key-work-1111"),
        request("sys", &leaky),
    )
    .await;
    a.unwrap();
    let (b, _) = run(
        &fake,
        &credential("personal", "key-personal-2222"),
        request("sys", "hello"),
    )
    .await;
    b.unwrap();

    let wrong = run(
        &fake,
        &credential("personal", "key-work-1111"),
        request("", "x"),
    )
    .await
    .0
    .unwrap_err();
    assert_eq!(wrong.kind, ProviderErrorKind::Auth);
    assert!(!wrong.retryable);
    let unknown = run(&fake, &credential("nobody", FAKE_KEY), request("", "x"))
        .await
        .0
        .unwrap_err();
    assert_eq!(unknown.kind, ProviderErrorKind::Auth);

    let seen = fake.requests();
    assert_eq!(seen.len(), 2, "rejected calls are not recorded");
    assert_eq!(seen[0].account.0, "work");
    assert_eq!(seen[1].account.0, "personal");
    assert_eq!(
        seen[0].request.messages[0].plain_text(),
        "my key is *** and ***"
    );
    assert_eq!(seen[1].request.system, "sys");

    let all = format!("{seen:?}");
    for k in [FAKE_KEY, "key-work-1111", "key-personal-2222"] {
        assert!(!all.contains(k));
    }
}

#[tokio::test]
async fn cancel_stops_a_delay_within_100ms() {
    let clock = FakeClock::new();
    let fake = std::sync::Arc::new(FakeProvider::new(
        Script::new()
            .then(Step::Delay(Duration::from_secs(10)))
            .then(Step::Text("never".into())),
    ));
    let cancel = CancellationToken::new();
    let (tx, _rx) = mpsc::channel(8);
    let task = {
        let (fake, cancel) = (fake.clone(), cancel.clone());
        tokio::spawn(async move {
            fake.stream(&fake_credential(), request("", "x"), tx, cancel)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let at = clock.elapsed();
    cancel.cancel();
    let e = task.await.unwrap().unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Cancelled);
    assert!(!e.retryable);
    assert!(clock.elapsed() - at < Duration::from_millis(100));
}

#[tokio::test]
async fn cancel_stops_a_send_blocked_on_a_full_channel() {
    let fake = std::sync::Arc::new(FakeProvider::new(
        Script::new().then(Step::Text("x".repeat(64))),
    ));
    let cancel = CancellationToken::new();
    let (tx, _rx) = mpsc::channel(1); // nobody reads, so the second chunk blocks
    let task = {
        let (fake, cancel) = (fake.clone(), cancel.clone());
        tokio::spawn(async move {
            fake.stream(&fake_credential(), request("", "x"), tx, cancel)
                .await
        })
    };
    tokio::task::yield_now().await;
    cancel.cancel();
    let e = tokio::time::timeout(Duration::from_millis(100), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Cancelled);
}

fn caps(f: impl FnOnce(&mut Capabilities)) -> Capabilities {
    let mut c = FakeProvider::new(Script::new()).capabilities();
    f(&mut c);
    c
}

#[tokio::test]
async fn effort_is_dropped_with_a_debug_log_when_unsupported() {
    let logs = capture_logs();
    let fake = FakeProvider::new(Script::new().then(Step::Text("ok".into())))
        .with_capabilities(caps(|c| c.effort = false));
    let mut req = request("", "think hard");
    req.effort = Some(Effort::Max);
    run(&fake, &fake_credential(), req).await.0.unwrap();
    assert_eq!(fake.requests()[0].request.effort, None);
    assert!(logs.text().contains("DEBUG"));
    assert!(
        logs.text().contains("effort not supported"),
        "{}",
        logs.text()
    );

    // Supported: kept, and nothing logged about it.
    let fake = FakeProvider::new(Script::new().then(Step::Text("ok".into())));
    let mut req = request("", "think hard");
    req.effort = Some(Effort::Max);
    run(&fake, &fake_credential(), req).await.0.unwrap();
    assert_eq!(fake.requests()[0].request.effort, Some(Effort::Max));
}

#[tokio::test]
async fn first_message_system_role_folds_the_system_prompt() {
    let fake = FakeProvider::new(Script::new().then(Step::Text("ok".into())))
        .with_capabilities(caps(|c| c.system_role = SystemRole::FirstMessage));
    run(
        &fake,
        &fake_credential(),
        request("You are a worker.", "do it"),
    )
    .await
    .0
    .unwrap();
    let seen = &fake.requests()[0].request;
    assert_eq!(seen.system, "");
    assert_eq!(seen.messages.len(), 1);
    assert_eq!(
        seen.messages[0].plain_text(),
        "You are a worker.\n\n\ndo it"
    );
}

#[tokio::test]
async fn folding_before_an_assistant_first_conversation_adds_a_user_message() {
    let c = caps(|c| c.system_role = SystemRole::FirstMessage);
    let mut req = request("rules", "x");
    req.messages[0].role = MessageRole::Assistant;
    let out = c.degrade(req);
    assert_eq!(out.messages.len(), 2);
    assert_eq!(
        (out.messages[0].role, out.messages[0].plain_text().as_str()),
        (MessageRole::User, "rules")
    );
}

#[test]
fn other_unsupported_features_are_dropped_too() {
    let c = caps(|c| {
        c.tools = false;
        c.thinking = false;
        c.logprobs = false;
        c.prompt_cache = false;
    });
    let mut req = request("", "x");
    req.tools = vec![ToolSpec {
        name: "read".into(),
        description: String::new(),
        schema: serde_json::json!({}),
    }];
    req.tool_choice = ToolChoice::Required;
    req.thinking = Some(ThinkingBudget { tokens: 1000 });
    req.logprobs = true;
    req.cache_hint = CacheHint::AfterSystem;
    let out = c.degrade(req);
    assert!(out.tools.is_empty() && out.thinking.is_none() && !out.logprobs);
    assert_eq!(
        (out.tool_choice, out.cache_hint),
        (ToolChoice::Auto, CacheHint::None)
    );

    let c = caps(|c| c.max_tools = Some(1));
    let mut req = request("", "x");
    let tool = |n: &str| ToolSpec {
        name: n.into(),
        description: String::new(),
        schema: serde_json::json!({}),
    };
    req.tools = vec![tool("a"), tool("b")];
    assert_eq!(c.degrade(req).tools.len(), 1);
}

#[tokio::test]
async fn the_fake_key_never_reaches_the_logs() {
    let logs = capture_logs();
    let fake = FakeProvider::new(Script::new().then(Step::Text("ok".into())));
    let creds = fake_credential();
    run(&fake, &creds, request("sys", "hi")).await.0.unwrap();
    fake.verify(&creds).await.unwrap();
    fake.refresh_models(&creds).await.unwrap();

    // The credential's own Debug is safe to log, which is the point of Secret.
    tracing::debug!(?creds, secret = ?creds.secret, "resolved");
    assert!(logs.text().contains("resolved"));
    assert!(logs.text().contains("***"));
    assert_no_secret_logged();
}

#[test]
#[should_panic(expected = "a secret was logged")]
fn the_guard_trips_when_the_key_is_logged() {
    let _logs = capture_logs();
    tracing::info!(key = FAKE_KEY, "oops");
    assert_no_secret_logged();
}

#[test]
#[should_panic(expected = "a secret was logged")]
fn dropping_the_capture_checks_too() {
    let logs = capture_logs();
    tracing::warn!("{}", fake_credential().secret.expose());
    drop(logs);
}

#[test]
fn secrets_debug_as_stars() {
    let creds = fake_credential();
    assert_eq!(format!("{:?}", creds.secret), "***");
    assert!(!format!("{creds:?}").contains(FAKE_KEY));
    assert!(!format!("{:#?}", creds).contains("7f3a9c"));
}

#[tokio::test]
async fn model_resolution_and_metadata() {
    let fake = FakeProvider::new(Script::new());
    let mut aliases = AliasTable::new();
    aliases.insert("fast".into(), "fake-small".into());
    assert_eq!(fake.id().0, "fake");
    assert_eq!(
        fake.resolve_model(&ModelRef::Alias("fast".into()), &aliases),
        Some(ModelId("fake-small".into()))
    );
    assert_eq!(
        fake.resolve_model(&ModelRef::Alias("nope".into()), &aliases),
        None
    );
    assert_eq!(
        fake.resolve_model(&ModelRef::Full(ModelId("fake-large".into())), &aliases),
        Some(ModelId("fake-large".into()))
    );
    assert_eq!(
        fake.resolve_model(&ModelRef::Full(ModelId("gpt".into())), &aliases),
        None
    );
    assert_eq!(fake.resolve_model(&ModelRef::Inherit, &aliases), None);
    assert!(fake.price(&ModelId("fake-small".into())).is_some());
    assert!(fake.price(&ModelId("other".into())).is_none());
    assert!(fake
        .count_tokens(&fake_credential(), &request("", "x"))
        .await
        .is_none());
}

#[test]
fn registry_builds_from_a_factory_list_and_rejects_duplicates() {
    let one: Vec<Box<dyn ProviderFactory>> = vec![Box::new(Factory)];
    let reg = ProviderRegistry::new(&one, &BTreeMap::new()).unwrap();
    assert_eq!(reg.ids(), ["fake"]);
    assert_eq!(reg.get("fake").unwrap().id().0, "fake");
    assert_eq!(
        reg.get("nope").err(),
        Some(ConfigError::UnknownProvider("nope".into()))
    );

    let two: Vec<Box<dyn ProviderFactory>> = vec![Box::new(Factory), Box::new(Factory)];
    assert_eq!(
        ProviderRegistry::new(&two, &BTreeMap::new()).err(),
        Some(ConfigError::DuplicateProvider("fake".into()))
    );
}
