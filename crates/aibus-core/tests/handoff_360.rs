//! #360 (A2.2d): a handoff between two parties is shown to the user before anything moves, moves
//! only with their yes, and leaves a record of the answer that holds only for what was shown, to
//! whom it was shown, in the run it was given in, once. The boundary is who receives and bills the
//! data (provider and account), not the runtime. A plan marks every transfer that could cross it.

use std::collections::BTreeSet;

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::compile;
use aibus_core::workflow::handoff::{
    deliver, item, needs_consent, preview, preview_sha256, record, Artifact, Context, Decision,
    HandoffError, Party, State, Transfer,
};
use aibus_core::workflow::{Runtime, Workflow};
use aibus_core::Redactor;
use serde_json::Value;

const ID: &str = "01937a3e-5b2c-7d4e-8f10-0123456789ab";
const OTHER: &str = "01937a3e-5b2c-7d4e-8f10-0123456789ac";

fn party(step: &str, runtime: Runtime, provider: &str, account: &str) -> Party {
    Party {
        step: step.into(),
        runtime,
        provider: provider.into(),
        account: account.into(),
    }
}

fn codex() -> Party {
    party("draft", Runtime::CodexCli, "openai", "chatgpt-1")
}
fn claude() -> Party {
    party("review", Runtime::ClaudeCli, "anthropic", "claude-1")
}
fn router() -> Party {
    party("review", Runtime::OpenRouter, "openrouter", "or-1")
}

fn ctx(from: Party, to: Party) -> Context {
    Context {
        run_id: "run-1".into(),
        plan_hash: "sha256:plan-1".into(),
        from,
        to,
    }
}

const SUMMARY: &[u8] = b"a short draft";
const NOTES: &[u8] = b"some notes";

fn transfer(to: Party) -> Transfer {
    Transfer {
        context: ctx(codex(), to),
        items: vec![
            item("prev.summary", "step_output", SUMMARY),
            item("notes.md", "artifact", NOTES),
        ],
    }
}

fn contents() -> Vec<(String, Vec<u8>)> {
    vec![
        ("prev.summary".into(), SUMMARY.to_vec()),
        ("notes.md".into(), NOTES.to_vec()),
    ]
}

fn granted(t: &Transfer) -> Artifact {
    let shown = preview_sha256(&preview(t));
    record(
        t.clone(),
        Decision::Granted { by: "rom" },
        &shown,
        ID,
        1_700_000_000,
    )
    .unwrap()
}

fn deliver_once(
    a: &Artifact,
    now: &Context,
    c: Vec<(String, Vec<u8>)>,
) -> Result<Vec<(String, Vec<u8>)>, HandoffError> {
    deliver(a, now, c, &mut BTreeSet::new())
}

#[test]
fn the_boundary_is_who_receives_and_bills_not_the_runtime() {
    assert!(needs_consent(&ctx(codex(), claude())));
    // Two aibus steps on an Anthropic and an OpenRouter account are a cross-provider move.
    let a = party("a", Runtime::Aibus, "anthropic", "claude-1");
    let b = party("b", Runtime::Aibus, "openrouter", "or-1");
    assert!(needs_consent(&ctx(a.clone(), b)));
    // The same provider under another account is another bill.
    assert!(needs_consent(&ctx(
        a.clone(),
        party("b", Runtime::Aibus, "anthropic", "claude-2")
    )));
    // The Claude CLI to an aibus step on the same Anthropic account stays with one receiver.
    assert!(!needs_consent(&ctx(
        claude(),
        party("b", Runtime::Aibus, "anthropic", "claude-1")
    )));
    // Same runtime, same everything: nothing to ask.
    assert!(!needs_consent(&ctx(
        a.clone(),
        party("b", Runtime::Aibus, "anthropic", "claude-1")
    )));
}

#[test]
fn the_preview_names_the_receiver_the_run_the_items_and_holds_no_content() {
    let t = transfer(claude());
    let p = preview(&t);
    assert_eq!(p["context"]["to"]["provider"], "anthropic");
    assert_eq!(p["context"]["run_id"], "run-1");
    assert_eq!(p["context"]["plan_hash"], "sha256:plan-1");
    assert_eq!(p["total_bytes"], (SUMMARY.len() + NOTES.len()) as u64);
    let items = p["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["name"], "prev.summary");
    assert!(items[0]["sha256"].as_str().unwrap().starts_with("sha256:"));
    assert!(
        !p.to_string().contains("a short draft"),
        "content is never in a preview"
    );
    let text = p["text"].as_str().unwrap();
    assert!(text.contains("Anthropic"), "{text}");
    let or = preview(&transfer(router()));
    let text = or["text"].as_str().unwrap();
    assert!(
        text.contains("OpenRouter") && text.contains("separate"),
        "{text}"
    );
    // Every runtime has its own words.
    for rt in [
        Runtime::Aibus,
        Runtime::ClaudeCli,
        Runtime::Attached,
        Runtime::CodexCli,
        Runtime::GeminiCli,
        Runtime::OpenRouter,
    ] {
        let mut t = transfer(claude());
        t.context.to.runtime = rt;
        assert!(!preview(&t)["text"].as_str().unwrap().is_empty(), "{rt:?}");
    }
}

#[test]
fn a_granted_handoff_records_who_when_and_the_preview_and_delivers_exactly_the_bytes_it_checked() {
    let t = transfer(claude());
    let a = granted(&t);
    assert_eq!(a.kind, "aibus.workflow.handoff");
    assert_eq!(a.id, ID);
    assert_eq!(
        (a.consent.state, a.consent.by.as_str(), a.consent.at_unix),
        (State::Granted, "rom", 1_700_000_000)
    );
    assert_eq!(a.preview_sha256, preview_sha256(&preview(&t)));
    assert_eq!(
        deliver_once(&a, &t.context, contents()).unwrap(),
        contents()
    );
}

#[test]
fn a_declined_handoff_is_recorded_and_delivers_nothing() {
    let t = transfer(claude());
    let shown = preview_sha256(&preview(&t));
    let a = record(t.clone(), Decision::Declined { by: "rom" }, &shown, ID, 1).unwrap();
    assert_eq!(a.consent.state, State::Declined);
    assert_eq!(
        deliver_once(&a, &t.context, contents()),
        Err(HandoffError::NotConsented)
    );
}

#[test]
fn consent_covers_exactly_the_items_shown_as_a_multiset() {
    let t = transfer(claude());
    let a = granted(&t);
    let now = &t.context;
    let mut changed = contents();
    changed[1].1 = b"other notes".to_vec();
    assert_eq!(
        deliver_once(&a, now, changed),
        Err(HandoffError::Changed("notes.md".into()))
    );
    let mut extra = contents();
    extra.push(("secrets.txt".into(), b"x".to_vec()));
    assert_eq!(
        deliver_once(&a, now, extra),
        Err(HandoffError::Changed("secrets.txt".into()))
    );
    assert_eq!(
        deliver_once(&a, now, contents()[..1].to_vec()),
        Err(HandoffError::Changed("notes.md".into()))
    );
    // Shown once, delivered twice.
    let mut twice = contents();
    twice.push(twice[0].clone());
    assert_eq!(
        deliver_once(&a, now, twice),
        Err(HandoffError::Changed("prev.summary".into()))
    );
    // Shown twice, delivered once.
    let mut shown_twice = t.clone();
    shown_twice.items.push(shown_twice.items[0].clone());
    let a2 = granted(&shown_twice);
    assert_eq!(
        deliver_once(&a2, now, contents()),
        Err(HandoffError::Changed("prev.summary".into()))
    );
}

#[test]
fn the_record_needs_a_real_uuid_v7_a_name_and_the_hash_of_the_preview_that_was_shown() {
    let t = transfer(claude());
    let shown = preview_sha256(&preview(&t));
    for bad in [
        "",
        "not-a-uuid",
        "01937a3e-5b2c-4d4e-8f10-0123456789ab",
        // version 7 but not the RFC 4122 variant
        "01937a3e-5b2c-7d4e-0f10-0123456789ab",
        // not the canonical lower-case form
        "01937A3E-5B2C-7D4E-8F10-0123456789AB",
    ] {
        assert_eq!(
            record(t.clone(), Decision::Granted { by: "rom" }, &shown, bad, 1).unwrap_err(),
            HandoffError::BadId(bad.into())
        );
    }
    assert_eq!(
        record(t.clone(), Decision::Granted { by: "" }, &shown, ID, 1).unwrap_err(),
        HandoffError::NoOne
    );
    assert_eq!(
        record(
            t,
            Decision::Granted { by: "rom" },
            "sha256:what-they-actually-saw",
            ID,
            1
        )
        .unwrap_err(),
        HandoffError::PreviewShown
    );
}

fn tampered(a: &Artifact, edit: impl FnOnce(&mut Value)) -> Option<Artifact> {
    let mut v = serde_json::to_value(a).unwrap();
    edit(&mut v);
    serde_json::from_value(v).ok()
}

#[test]
fn an_edited_record_does_not_deliver() {
    let t = transfer(claude());
    let a = granted(&t);
    let now = &t.context;
    // A state that is not one of the three does not even read.
    for s in ["pending", "", "GRANTED"] {
        assert!(
            tampered(&a, |v| v["consent"]["state"] = s.into()).is_none(),
            "{s:?}"
        );
    }
    // A declined one relabelled `not_needed` is a cross-provider move, so it still refuses.
    let shown = preview_sha256(&preview(&t));
    let declined = record(t.clone(), Decision::Declined { by: "rom" }, &shown, ID, 1).unwrap();
    let relabelled = tampered(&declined, |v| v["consent"]["state"] = "not_needed".into()).unwrap();
    assert_eq!(
        deliver_once(&relabelled, now, contents()),
        Err(HandoffError::NotConsented)
    );
    // A garbage hash, and a transfer edited to go somewhere else under the old hash.
    let garbage = tampered(&a, |v| v["preview_sha256"] = "sha256:garbage".into()).unwrap();
    assert_eq!(
        deliver_once(&garbage, now, contents()),
        Err(HandoffError::PreviewHash)
    );
    let rerouted = tampered(&a, |v| {
        v["transfer"]["context"]["to"]["provider"] = "openrouter".into()
    })
    .unwrap();
    assert_eq!(
        deliver_once(&rerouted, &rerouted.transfer.context, contents()),
        Err(HandoffError::PreviewHash)
    );
}

#[test]
fn a_consent_for_one_destination_run_or_plan_does_not_carry_to_another() {
    let t = transfer(claude());
    let a = granted(&t);
    // Codex to Claude was agreed, Codex to OpenRouter is asked for.
    let mut elsewhere = t.context.clone();
    elsewhere.to = router();
    assert_eq!(
        deliver_once(&a, &elsewhere, contents()),
        Err(HandoffError::WrongContext)
    );
    let mut later = t.context.clone();
    later.run_id = "run-2".into();
    assert_eq!(
        deliver_once(&a, &later, contents()),
        Err(HandoffError::WrongContext)
    );
    let mut edited = t.context.clone();
    edited.plan_hash = "sha256:plan-2".into();
    assert_eq!(
        deliver_once(&a, &edited, contents()),
        Err(HandoffError::WrongContext)
    );
}

#[test]
fn a_consent_is_used_once() {
    let t = transfer(claude());
    let a = granted(&t);
    let mut spent = BTreeSet::new();
    assert!(deliver(&a, &t.context, contents(), &mut spent).is_ok());
    assert_eq!(
        deliver(&a, &t.context, contents(), &mut spent),
        Err(HandoffError::Spent)
    );
    // A refusal does not use it up.
    let mut spent = BTreeSet::new();
    let mut bad = contents();
    bad[0].1 = b"x".to_vec();
    assert!(deliver(&a, &t.context, bad, &mut spent).is_err());
    assert!(deliver(&a, &t.context, contents(), &mut spent).is_ok());
    // The same transfer recorded again under a new id is the same consent, so it is spent too.
    let shown = preview_sha256(&preview(&t));
    let b = record(t.clone(), Decision::Granted { by: "rom" }, &shown, OTHER, 2).unwrap();
    assert_eq!(
        deliver(&b, &t.context, contents(), &mut spent),
        Err(HandoffError::Spent)
    );
    // And so is a copy of the record with only its id changed.
    let copy = tampered(&a, |v| v["id"] = OTHER.into()).unwrap();
    assert_eq!(
        deliver(&copy, &t.context, contents(), &mut spent),
        Err(HandoffError::Spent)
    );
}

#[test]
fn an_empty_or_placeholder_party_always_needs_consent_and_is_never_not_needed() {
    for account in ["", "unresolved", "resolved when the step runs", "unknown"] {
        let from = party("a", Runtime::Aibus, "anthropic", account);
        let to = party("b", Runtime::Aibus, "anthropic", account);
        let c = ctx(from, to);
        assert!(needs_consent(&c), "account {account:?}");
        let t = Transfer {
            context: c.clone(),
            items: transfer(claude()).items,
        };
        let shown = preview_sha256(&preview(&t));
        let a = record(t.clone(), Decision::Declined { by: "rom" }, &shown, ID, 1).unwrap();
        assert_eq!(a.consent.state, State::Declined, "{account:?}");
        assert_eq!(
            deliver_once(&a, &c, contents()),
            Err(HandoffError::NotConsented),
            "{account:?}"
        );
    }
    let empty = ctx(
        party("a", Runtime::Aibus, "", "x"),
        party("b", Runtime::Aibus, "", "x"),
    );
    assert!(needs_consent(&empty));
}

#[test]
fn the_provider_of_a_cli_or_openrouter_step_comes_from_its_runtime_not_from_the_caller() {
    // Codex is OpenAI's, whatever the caller writes; an OpenRouter step claiming `openai` and the
    // same account is still another receiver.
    let liar = party("b", Runtime::OpenRouter, "openai", "chatgpt-1");
    assert!(needs_consent(&ctx(codex(), liar)));
    let liar_from = party("a", Runtime::CodexCli, "anthropic", "claude-1");
    assert!(needs_consent(&ctx(liar_from, claude())));
}

#[test]
fn a_move_that_stays_with_one_receiver_needs_no_yes_and_cannot_borrow_one() {
    let same = ctx(
        claude(),
        party("b", Runtime::Aibus, "anthropic", "claude-1"),
    );
    let t = Transfer {
        context: same.clone(),
        items: transfer(claude()).items,
    };
    let shown = preview_sha256(&preview(&t));
    let a = record(t.clone(), Decision::Granted { by: "rom" }, &shown, ID, 1).unwrap();
    assert_eq!(a.consent.state, State::NotNeeded);
    assert_eq!(deliver_once(&a, &same, contents()).unwrap(), contents());
}

fn plan_of(text: &str) -> Value {
    let wf = Workflow::parse("t", text).unwrap_or_else(|e| panic!("{e}"));
    let dir = tempfile::tempdir().unwrap();
    let agents = discover(None, dir.path(), None, &[], &[]).unwrap().agents;
    compile(&wf, text, &agents, &Redactor::new()).unwrap_or_else(|p| panic!("{p:?}"))
}

#[test]
fn a_plan_lists_every_data_edge_that_can_cross_the_boundary_as_a_handoff_that_needs_consent() {
    let plan = plan_of(
        "[[step]]\nid = \"draft\"\nagent = \"explore\"\nruntime = \"cli.codex\"\nprompt = \"p {{input}}\"\n\n\
         [[step]]\nid = \"review\"\nagent = \"explore\"\nruntime = \"cli.claude\"\nprompt = \"q {{prev.summary}}\"\n",
    );
    let h = plan["handoffs"].as_array().expect("handoffs");
    assert_eq!(h.len(), 1);
    assert_eq!(
        (h[0]["from"].as_str(), h[0]["to"].as_str()),
        (Some("draft"), Some("review"))
    );
    assert_eq!(
        (h[0]["from_runtime"].as_str(), h[0]["to_runtime"].as_str()),
        (Some("cli.codex"), Some("cli.claude"))
    );
    assert_eq!(h[0]["via"], "prev.summary");
    assert_eq!(h[0]["needs_consent"], true);
    // The accounts are not known until a run, so the plan says so and the run asks again.
    assert_eq!(h[0]["accounts"], "resolved when the step runs");
}

#[test]
fn a_chain_on_one_runtime_has_no_handoffs_and_the_plan_has_no_such_key() {
    let plan = plan_of(
        "[[step]]\nid = \"a\"\nagent = \"explore\"\nprompt = \"p {{input}}\"\n\n\
         [[step]]\nid = \"b\"\nagent = \"explore\"\nprompt = \"q {{prev.summary}}\"\n",
    );
    assert!(plan.get("handoffs").is_none());
}
