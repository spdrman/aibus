//! 10 section 4, bullets 7 and 8: the advisor's `same_error_x2` checkpoint and the router's
//! `sharp` and `split` verdicts. The lead, the advisor and the router each get their own fake.

use std::sync::Arc;

use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::StopReason;
use aibus_testkit::fake_credential;
use serde_json::json;

use crate::rig::{call, script};
use crate::solo::{bypass, Solo};

fn say(text: &str) -> [Step; 2] {
    [Step::Text(text.into()), Step::Stop(StopReason::EndTurn)]
}

fn seat(fake: &Arc<FakeProvider>, account: &str) -> Seat {
    Seat {
        provider: fake.clone(),
        creds: Arc::new(fake_credential()),
        model: ModelId("fake-small".into()),
        account: AccountName(account.into()),
        fallback: None,
    }
}

fn advisor(script: Script) -> (Arc<Assist>, Arc<FakeProvider>) {
    let fake = Arc::new(FakeProvider::new(script));
    let seat = AdvisorSeat {
        seat: seat(&fake, "adv"),
        def: bypass("advisor"),
        enabled: ["before_plan", "same_error_x2", "before_done"]
            .map(String::from)
            .to_vec(),
        packet_max_tokens: 400_000,
    };
    (Assist::new(Some(seat), None), fake)
}

fn router(script: Script) -> (Arc<Assist>, Arc<FakeProvider>) {
    let fake = Arc::new(FakeProvider::new(script));
    let seat = RouterSeat {
        seat: seat(&fake, "rtr"),
        system: "route".into(),
        threshold: 0.85,
        max_per_turn: 10,
        label: "ROUTER".into(),
    };
    (Assist::new(None, Some(seat)), fake)
}

#[tokio::test]
async fn same_error_x2_triggers_the_advisor_once_per_pair_and_the_advice_lands_on_the_next_request()
{
    let s = Solo::new();
    let (assist, advisor) = advisor(script(say("wrong fixture")));
    let bad = |n: u32| call("read", json!({"path": format!("/nope/file{n}.txt")}));
    let lead = script(
        bad(1)
            .into_iter()
            .chain(bad(2))
            .chain(bad(3))
            .chain(say("giving up")),
    );
    let done = s
        .go(lead, bypass("lead"), |_, d| d.assist = Some(assist))
        .await;

    let hits: Vec<_> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::CheckpointHit { checkpoint, .. } => Some(checkpoint.name().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        hits,
        ["same_error_x2"],
        "two failures make a pair, the third starts another"
    );
    assert_eq!(done.count("AdvisorSpoke"), 1);
    assert_eq!(advisor.requests().len(), 1);
    // The advice is a system message on the lead's next request, and not on the ones before.
    assert!(done.notes(1).is_empty());
    assert_eq!(done.notes(2), ["[advisor] wrong fixture"]);
}

fn pick(choice: &str, p: f32) -> Script {
    script([
        Step::RouterAnswer {
            choice: choice.into(),
            p,
        },
        Step::Stop(StopReason::EndTurn),
    ])
}

/// A tool call that fails at once, which is what the router is asked `retry_or_stop` about.
fn empty_bash() -> serde_json::Value {
    json!({"command": " "})
}

#[tokio::test]
async fn router_sharp_applies_in_code_and_split_lands_in_the_leads_request_as_a_note() {
    let s = Solo::new();

    // Sharp: `stop + ask` at 0.9 ends the turn with the tool's error, no note, no second turn.
    let (assist, _) = router(pick("stop + ask", 0.9));
    let lead = script(call("bash", empty_bash()).into_iter().chain(say("never")));
    let done = s
        .go(lead, bypass("lead"), |_, d| d.assist = Some(assist))
        .await;
    let answered: Vec<_> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::RouterAnswered { answer, .. } => Some(answer.verdict),
            _ => None,
        })
        .collect();
    assert_eq!(answered, [Verdict::Sharp]);
    let r = done.finished();
    assert_eq!(r.status, RunStatus::Partial);
    assert_eq!(r.error.as_ref().unwrap().kind, RunErrorKind::Tool);
    assert_eq!(
        done.fake.requests().len(),
        1,
        "applied in code: the lead never took another turn"
    );

    // Split: the same answer at 0.58 is only a guess, and the lead gets it as a note.
    let (assist, _) = router(pick("stop + ask", 0.58));
    let lead = script(call("bash", empty_bash()).into_iter().chain(say("ok")));
    let done = s
        .go(lead, bypass("lead"), |_, d| d.assist = Some(assist))
        .await;
    let verdicts: Vec<_> = done
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::RouterAnswered { answer, .. } => Some(answer.verdict),
            _ => None,
        })
        .collect();
    assert_eq!(verdicts, [Verdict::Split]);
    let notes = done.notes(1);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("[router] retry_or_stop"), "{notes:?}");
    assert!(
        notes[0].contains("stop + ask") && notes[0].contains("split") && notes[0].contains("0.58"),
        "{notes:?}"
    );
    assert_eq!(done.finished().status, RunStatus::Completed);
}
