use std::sync::Arc;

use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{Capabilities, SystemRole};
use aibus_testkit::fake_credential;

use super::*;
use crate::assist::Seat;
use crate::msg::RouterKind;

fn seat(fake: Arc<FakeProvider>) -> RouterSeat {
    RouterSeat {
        seat: Seat {
            provider: fake,
            creds: Arc::new(fake_credential()),
            model: aibus_provider::ModelId("fake-small".into()),
            account: crate::ids::AccountName("fake".into()),
            fallback: None,
        },
        system: "route".into(),
        threshold: 0.85,
        max_per_turn: 10,
        label: "ROUTER".into(),
    }
}

fn question() -> RouterQuestion {
    RouterQuestion {
        kind: RouterKind::RetryOrStop,
        context: "it failed".into(),
        choices: RETRY_CHOICES.map(String::from).to_vec(),
    }
}

fn no_logprobs() -> Capabilities {
    Capabilities {
        tools: true,
        parallel_tool_calls: true,
        streaming: true,
        thinking: false,
        prompt_cache: false,
        logprobs: false,
        effort: false,
        images: false,
        system_role: SystemRole::Separate,
        max_tools: None,
    }
}

#[tokio::test]
async fn a_logprob_capable_provider_gives_p_from_the_logprobs_at_16_tokens() {
    let fake = Arc::new(FakeProvider::new(Script::new().then(Step::RouterAnswer {
        choice: "retry once".into(),
        p: 0.94,
    })));
    let a = ask(&seat(fake.clone()), &question(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(a.choice, "retry once");
    assert!((a.p - 0.94).abs() < 1e-3);
    assert_eq!(a.source, PSource::Logprob);
    assert_eq!(a.verdict, Verdict::Sharp);
    let req = &fake.requests()[0].request;
    assert_eq!(req.max_output_tokens, 16);
    assert!(req.logprobs && req.tools.is_empty());
}

#[tokio::test]
async fn without_logprobs_p_is_self_reported_and_the_threshold_decides_the_verdict() {
    let fake = Arc::new(
        FakeProvider::new(
            Script::new()
                .then(Step::RouterAnswer {
                    choice: "stop + ask".into(),
                    p: 0.58,
                })
                .then(Step::Stop(aibus_provider::StopReason::EndTurn))
                .then(Step::RouterAnswer {
                    choice: "stop + ask".into(),
                    p: 0.85,
                }),
        )
        .with_capabilities(no_logprobs()),
    );
    let s = seat(fake.clone());
    let a = ask(&s, &question(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((a.source, a.verdict), (PSource::SelfReport, Verdict::Split));
    assert!(!fake.requests()[0].request.logprobs);
    let b = ask(&s, &question(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(b.verdict, Verdict::Sharp, "the threshold itself is sharp");
}

#[tokio::test]
async fn an_answer_outside_the_choices_is_no_answer() {
    let fake = Arc::new(FakeProvider::new(Script::new().then(Step::RouterAnswer {
        choice: "give up".into(),
        p: 0.99,
    })));
    assert!(ask(&seat(fake), &question(), &CancellationToken::new())
        .await
        .is_err());
}

#[test]
fn parse_takes_a_choice_case_insensitively_or_by_number() {
    let choices: Vec<String> = RETRY_CHOICES.map(String::from).to_vec();
    let (c, _, p) = parse(r#"sure {"choice":"Retry Once","p":0.7}"#, &choices).unwrap();
    assert_eq!((c.as_str(), p), ("retry once", Some(0.7)));
    assert_eq!(parse(r#"{"choice":3}"#, &choices).unwrap().0, "stop + ask");
    assert!(parse(r#"{"choice":9}"#, &choices).is_none());
    assert!(parse("no json", &choices).is_none());
    assert_eq!(parse(r#"{"choice":"x","p":7}"#, &[]).unwrap().2, Some(1.0));
}

#[test]
fn joint_p_multiplies_the_tokens_that_spell_the_choice() {
    let lp = |t: &str, p: f32| (t.to_owned(), p.ln());
    let tokens = [
        lp("{\"choice\":\"", 0.99),
        lp("retry", 0.9),
        lp(" once", 0.5),
        lp("\",\"p\":0.9}", 0.1),
    ];
    let p = joint_p("retry once", &tokens).unwrap();
    assert!((p - 0.45).abs() < 1e-4, "{p}");
    assert!(joint_p("retry once", &[]).is_none());
    assert!(joint_p("stop", &tokens).is_none());
}

#[test]
fn an_intention_is_a_last_line_that_says_what_comes_next() {
    assert!(ends_with_intention("Found it.\nNow I'll run the tests."));
    assert!(ends_with_intention("Let me check the config"));
    assert!(ends_with_intention("The next steps are:"));
    assert!(!ends_with_intention("All done, the tests pass."));
    assert!(!ends_with_intention(""));
}

#[test]
fn candidates_are_the_files_that_share_a_symbol() {
    let d = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/a.rs"), "fn parse_config() {}").unwrap();
    std::fs::write(d.path().join("src/b.rs"), "use x::parse_config;").unwrap();
    std::fs::write(d.path().join("src/c.rs"), "fn other() {}").unwrap();
    let (sym, files) = which_file_candidates(d.path(), "please fix parse_config soon").unwrap();
    assert_eq!(sym, "parse_config");
    assert_eq!(files, ["src/a.rs", "src/b.rs"]);
    // One file only, or no symbol at all, is not ambiguous.
    assert!(which_file_candidates(d.path(), "fix `other`").is_none());
    assert!(which_file_candidates(d.path(), "please fix it").is_none());
    assert_eq!(read_head(d.path(), "src/a.rs", 5).unwrap(), "fn pa");
}
