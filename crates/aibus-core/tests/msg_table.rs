mod common;

use aibus_core::state::value::{as_str, get};
use aibus_core::*;
use aibus_graph::{Graph, Value};
use common::*;

#[test]
fn names_are_pinned() {
    // Built by hand, not from `name()`, so a rename shows up here.
    let samples: Vec<(Msg, &str)> = vec![
        (
            Msg::Key(KeyEvent {
                code: KeyCode::Tab,
                ctrl: false,
                alt: false,
                shift: false,
            }),
            "Key",
        ),
        (
            Msg::Mouse(MouseEvent {
                kind: MouseKind::Move,
                col: 0,
                row: 0,
            }),
            "Mouse",
        ),
        (Msg::Resize { cols: 1, rows: 1 }, "Resize"),
        (Msg::Tick, "Tick"),
        (
            Msg::SubmitPrompt {
                text: String::new(),
            },
            "SubmitPrompt",
        ),
        (Msg::Command(Command::Clear), "Command"),
        (started(R()), "RunStarted"),
        (text_stream(R(), ""), "Stream"),
        (
            Msg::ToolCallRequested {
                run: R(),
                call: tool_call(false),
            },
            "ToolCallRequested",
        ),
        (
            Msg::PermissionNeeded {
                request: permission_request(),
            },
            "PermissionNeeded",
        ),
        (
            Msg::PermissionDecided {
                id: perm(),
                decision: Decision::Deny,
                reason: None,
            },
            "PermissionDecided",
        ),
        (
            Msg::UsageUpdate {
                run: R(),
                usage: Usage::default(),
            },
            "UsageUpdate",
        ),
        (
            Msg::Compacted {
                run: R(),
                pre_tokens: 0,
            },
            "Compacted",
        ),
        (
            Msg::Shutdown {
                reason: ShutdownReason::UserQuit,
            },
            "Shutdown",
        ),
        (Msg::Persisted, "Persisted"),
    ];
    for (m, want) in samples {
        assert_eq!(m.name(), want);
    }
    // The rest are covered by the case table, which pins all 28 names.
    let from_cases: std::collections::BTreeSet<_> = cases().iter().map(|c| c.msg.name()).collect();
    assert_eq!(from_cases, ALL_NAMES.into_iter().collect());
    assert_eq!(ALL_NAMES.len(), 29);
}

#[test]
fn last_msg_never_carries_text() {
    for case in cases() {
        let mut a = app();
        (case.setup)(&mut a);
        a.step(case.msg.clone()).unwrap();
        let last = a.graph.read_key("model:last_msg").unwrap();
        assert_eq!(as_str(get(last, "msg").unwrap()), Some(case.msg.name()));
        let text = format!("{last:?}");
        assert!(
            !text.contains(SENTINEL),
            "{} leaked text: {text}",
            case.msg.name()
        );
        assert!(get(last, "seq").is_some());
        assert_eq!(a.graph.commit_log().last().unwrap().intent, case.msg.name());
    }
}

#[test]
fn a_key_press_is_summarised_as_nothing() {
    let m = Msg::Key(KeyEvent {
        code: KeyCode::Char('x'),
        ctrl: false,
        alt: false,
        shift: false,
    });
    let Value::Map(s) = m.summary() else { panic!() };
    assert_eq!(s.keys().collect::<Vec<_>>(), ["msg"]);
}

#[test]
fn exit_codes_follow_06_section_3() {
    use aibus_provider::{ConfigError, ProviderError, ProviderErrorKind};
    let cases: Vec<(AibusError, u8)> = vec![
        (std::io::Error::other("x").into(), 1),
        (AibusError::Usage("x".into()), 2),
        (ConfigError::UnknownProvider("x".into()).into(), 3),
        (CredentialError::Missing(AccountName("a".into())).into(), 4),
        (ProviderError::new(ProviderErrorKind::Server, "x").into(), 5),
        (AibusError::Denied("x".into()), 6),
        (AibusError::Interrupted, 7),
        (AibusError::Budget("x".into()), 8),
        (
            RunError {
                kind: RunErrorKind::Internal,
                message: "x".into(),
                retryable: false,
            }
            .into(),
            10,
        ),
    ];
    for (e, code) in cases {
        assert_eq!(e.exit_code(), code, "{e}");
    }
}

#[test]
fn agent_names_are_validated() {
    for ok in ["a", "code-reviewer", "x_1", "9lives", &"a".repeat(64)] {
        assert!(AgentName::new(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "Upper",
        "-lead",
        "has space",
        "a:b",
        "ü",
        &"a".repeat(65),
    ] {
        assert!(AgentName::new(bad).is_err(), "{bad}");
    }
    assert!(serde_json::from_str::<AgentName>("\"A\"").is_err());
    assert_eq!(
        serde_json::from_str::<AgentName>("\"ok\"")
            .unwrap()
            .as_str(),
        "ok"
    );
}

#[test]
fn run_status_round_trips_through_its_node_string() {
    let all = [
        RunStatus::Queued,
        RunStatus::Starting,
        RunStatus::Running,
        RunStatus::WaitingPermission(perm()),
        RunStatus::WaitingUser,
        RunStatus::Compacting,
        RunStatus::Completed,
        RunStatus::Partial,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::Denied,
    ];
    for st in all {
        assert_eq!(RunStatus::parse(&st.as_value()), Some(st));
    }
}

#[test]
fn a_key_ref_that_is_not_a_reference_is_refused_without_echoing_it() {
    let secret = "sk-ant-api03-REALLYSECRET";
    let mut a = app();
    let err = a
        .step(Msg::AccountsReloaded {
            accounts: vec![AccountView {
                name: AccountName("work".into()),
                provider: "anthropic".into(),
                label: "w".into(),
                default_model: None,
                key_ref: secret.into(),
                models: vec![],
            }],
        })
        .unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert!(!err.to_string().contains(secret));
    assert!(!format!("{err:?}").contains(secret));
    assert_eq!(
        a.graph.read_key("model:accounts"),
        Some(&Value::List(vec![].into()))
    );
}
