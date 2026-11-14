//! 10 section 4, bullets 5, 6, 14 and 15: permissions, the `pre_tool_use` hook, retries and the
//! cost budget.

use std::time::Duration;

use aibus_core::*;
use aibus_provider::fake::Step;
use aibus_provider::{ProviderError, ProviderErrorKind};
use aibus_testkit::FakeClock;
use serde_json::json;

use crate::rig::{call, script};
use crate::solo::{bypass, Solo};

fn answer(h: &RunHandle, m: &Msg, decision: Decision) {
    if let Msg::PermissionNeeded { request } = m {
        h.mailbox
            .try_send(RunMail::Permission {
                id: request.id,
                decision,
            })
            .unwrap();
    }
}

#[tokio::test]
async fn permission_ask_pauses_the_run_decided_resumes_it_and_dont_ask_denies() {
    let _clock = FakeClock::new();
    let s = Solo::new();
    let tty = || Gate {
        tty: true,
        ..Gate::default()
    };
    let mut general = bypass("general");
    general.permission_mode = PermissionMode::Default;

    // `default` mode: the call asks, the run waits for `PermissionDecided`, then the call runs.
    let tools = s.tools(tty());
    let done = s
        .go_with(
            script(
                call("bash", json!({"command": "echo ran"}))
                    .into_iter()
                    .chain([Step::Text("ok".into())]),
            ),
            general.clone(),
            |_, d| d.tools = tools,
            |m, h| answer(h, m, Decision::Allow),
        )
        .await;
    let names: Vec<_> = done
        .msgs
        .iter()
        .map(Msg::name)
        .filter(|n| !matches!(*n, "Stream" | "UsageUpdate"))
        .collect();
    assert_eq!(
        names,
        [
            "RunStarted",
            "ToolCallRequested",
            "PermissionNeeded",
            "PermissionDecided",
            "ToolCallFinished",
            "RunFinished"
        ]
    );
    assert!(
        done.results(1)[0].trim_end().ends_with("ran"),
        "{:?}",
        done.results(1)
    );

    // A refusal comes back as an error result and the run carries on.
    let tools = s.tools(tty());
    let done = s
        .go_with(
            script(
                call("bash", json!({"command": "touch PWN"}))
                    .into_iter()
                    .chain([Step::Text("ok".into())]),
            ),
            general.clone(),
            |_, d| d.tools = tools,
            |m, h| answer(h, m, Decision::Deny),
        )
        .await;
    assert!(
        done.results(1)[0].contains("permission denied"),
        "{:?}",
        done.results(1)
    );
    assert!(!s.dir.join("PWN").exists());
    assert_eq!(done.finished().status, RunStatus::Completed);

    // `dont_ask` never prompts: the call is denied at once.
    let mut dont_ask = general;
    dont_ask.permission_mode = PermissionMode::DontAsk;
    let done = s
        .go(
            script(
                call("bash", json!({"command": "touch PWN"}))
                    .into_iter()
                    .chain([Step::Text("ok".into())]),
            ),
            dont_ask,
            |_, _| {},
        )
        .await;
    assert_eq!(done.count("PermissionNeeded"), 0);
    assert!(
        done.results(1)[0].contains("dont_ask mode does not allow bash"),
        "{:?}",
        done.results(1)
    );
    assert!(!s.dir.join("PWN").exists());
}

#[tokio::test]
async fn a_pre_tool_use_hook_that_exits_two_blocks_the_call_and_the_model_sees_the_stderr() {
    let s = Solo::new();
    let mut def = bypass("general");
    def.hooks
        .entry("pre_tool_use".into())
        .or_default()
        .push(HookSpec {
            matcher: Some("bash".into()),
            command: "echo psql is off limits >&2; exit 2".into(),
            timeout_secs: None,
            kind: HookKind::Command,
        });
    let hooks = s.hooks();
    let done = s
        .go(
            script(
                call("bash", json!({"command": "touch marker"}))
                    .into_iter()
                    .chain([Step::Text("ok".into())]),
            ),
            def,
            |_, d| d.hooks = hooks,
        )
        .await;
    assert!(!s.dir.join("marker").exists(), "the command never ran");
    assert_eq!(done.finished().status, RunStatus::Completed);
    let seen = done.results(1);
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("psql is off limits"), "{}", seen[0]);
    let fired: Vec<_> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::HookFired { event, outcome, .. } => Some((*event, outcome.exit_code)),
            _ => None,
        })
        .collect();
    assert_eq!(fired, [(HookEvent::PreToolUse, 2)]);
}

#[tokio::test]
async fn a_retryable_error_retries_with_backoff_and_a_non_retryable_one_fails_the_run_as_exit_5() {
    let clock = FakeClock::new();
    let s = Solo::new();
    let server = || {
        Step::Error(ProviderError::new(
            ProviderErrorKind::Server,
            "upstream fell over",
        ))
    };

    // Two failures, then success: 1 s and 2 s of virtual time (500 ms base, doubling, jittered
    // only upwards), and the run completes.
    let done = s
        .go(
            script([server(), server(), Step::Text("made it".into())]),
            bypass("explore"),
            |_, _| {},
        )
        .await;
    let r = done.finished();
    assert_eq!(
        (r.status, r.summary.as_str()),
        (RunStatus::Completed, "made it")
    );
    assert_eq!(done.fake.requests().len(), 3);
    assert!(
        clock.elapsed() >= Duration::from_millis(1500),
        "{:?}",
        clock.elapsed()
    );
    assert!(
        clock.elapsed() < Duration::from_secs(10),
        "{:?}",
        clock.elapsed()
    );

    // A request the provider rejects is not retried: the run fails once, as a provider error,
    // and a provider error is what the CLI maps to exit code 5.
    let done = s
        .go(
            script([
                Step::Error(ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    "bad schema",
                )),
                Step::Text("never".into()),
            ]),
            bypass("explore"),
            |_, _| {},
        )
        .await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Failed);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Provider);
    assert_eq!(done.fake.requests().len(), 1);
    assert_eq!(done.count("RunFailed"), 1);
    let exit = AibusError::from(ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        "bad schema",
    ))
    .exit_code();
    assert_eq!(exit, 5);
}

#[tokio::test]
async fn max_cost_usd_stops_the_run_with_a_budget_error() {
    let _clock = FakeClock::new();
    let s = Solo::new();
    let mut def = bypass("explore");
    def.max_cost_usd = Some(0.5);
    // The fake prices input at $1 per million tokens.
    let steps = [
        Step::Text("pricey".into()),
        Step::Usage {
            input: 1_000_000,
            output: 0,
        },
    ]
    .into_iter()
    .chain(call("ls", json!({})))
    .chain([Step::Text("never".into())]);
    let done = s.go(script(steps), def, |_, _| {}).await;
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Partial);
    let e = r.error.as_ref().unwrap();
    assert_eq!(e.kind, RunErrorKind::Budget);
    assert!(e.message.contains("max_cost_usd"), "{}", e.message);
    assert_eq!(done.count("ToolCallRequested"), 0, "the tool did not run");
    assert!((r.usage.cost_usd - 1.0).abs() < 1e-9);
}
