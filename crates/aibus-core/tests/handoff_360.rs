//! #360 (A2.2d): a handoff between two runtimes is shown to the user before anything moves, moves
//! only with their yes, and leaves a record of the answer. A plan marks every step-to-step
//! transfer that crosses runtimes.

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::compile;
use aibus_core::workflow::handoff::{
    check_delivery, needs_consent, preview, record, Decision, HandoffError, Item, Transfer,
};
use aibus_core::workflow::Workflow;
use aibus_core::Redactor;
use serde_json::Value;

const ID: &str = "01937a3e-5b2c-7d4e-8f10-0123456789ab";

fn item(name: &str, sha: &str, bytes: u64) -> Item {
    Item {
        name: name.into(),
        kind: "step_output".into(),
        sha256: format!("sha256:{sha}"),
        bytes,
    }
}

fn transfer(to: &str) -> Transfer {
    Transfer {
        from_step: "draft".into(),
        from_runtime: "cli.codex".into(),
        to_step: "review".into(),
        to_runtime: to.into(),
        items: vec![item("prev.summary", "aa", 120), item("notes.md", "bb", 30)],
    }
}

#[test]
fn only_a_transfer_that_changes_runtime_needs_consent() {
    assert!(needs_consent(&transfer("cli.claude")));
    assert!(needs_consent(&transfer("openrouter")));
    assert!(!needs_consent(&transfer("cli.codex")));
}

#[test]
fn the_preview_names_where_it_goes_what_goes_and_whose_billing_and_holds_no_content() {
    let p = preview(&transfer("cli.claude"));
    assert_eq!(p["from"]["runtime"], "cli.codex");
    assert_eq!(p["to"]["runtime"], "cli.claude");
    assert_eq!(p["total_bytes"], 150);
    let items = p["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["name"], "prev.summary");
    assert_eq!(items[0]["sha256"], "sha256:aa");
    assert_eq!(items[0]["bytes"], 120);
    assert!(items[0].get("content").is_none() && items[0].get("text").is_none());
    let text = p["text"].as_str().unwrap();
    assert!(
        text.contains("cli.claude") && text.contains("Anthropic"),
        "{text}"
    );
    // OpenRouter is a different kind of destination and says so.
    let or = preview(&transfer("openrouter"));
    let text = or["text"].as_str().unwrap();
    assert!(
        text.contains("OpenRouter") && text.contains("separate"),
        "{text}"
    );
}

#[test]
fn a_granted_handoff_records_who_said_yes_and_which_preview_they_saw() {
    let t = transfer("cli.claude");
    let a = record(
        t.clone(),
        Decision::Granted { by: "rom" },
        ID,
        1_700_000_000,
    )
    .unwrap();
    assert_eq!(a.kind, "aibus.workflow.handoff");
    assert_eq!(a.id, ID);
    assert_eq!(a.consent.state, "granted");
    assert_eq!(a.consent.by, "rom");
    assert_eq!(a.consent.at_unix, 1_700_000_000);
    let shown = aibus_core::workflow::compile::content_hash(
        &aibus_core::workflow::compile::canonical_json(&preview(&t)),
    );
    assert_eq!(a.preview_sha256, shown);
    check_delivery(&a, &t.items).unwrap();
}

#[test]
fn a_declined_handoff_is_recorded_and_delivers_nothing() {
    let t = transfer("cli.claude");
    let a = record(t.clone(), Decision::Declined { by: "rom" }, ID, 1).unwrap();
    assert_eq!(a.consent.state, "declined");
    assert_eq!(
        check_delivery(&a, &t.items),
        Err(HandoffError::NotConsented)
    );
}

#[test]
fn consent_covers_exactly_what_was_shown() {
    let t = transfer("cli.claude");
    let a = record(t.clone(), Decision::Granted { by: "rom" }, ID, 1).unwrap();
    let mut changed = t.items.clone();
    changed[1] = item("notes.md", "cc", 30);
    assert_eq!(
        check_delivery(&a, &changed),
        Err(HandoffError::Changed("notes.md".into()))
    );
    let mut extra = t.items.clone();
    extra.push(item("secrets.txt", "dd", 5));
    assert_eq!(
        check_delivery(&a, &extra),
        Err(HandoffError::Changed("secrets.txt".into()))
    );
    assert_eq!(
        check_delivery(&a, &t.items[..1]),
        Err(HandoffError::Changed("notes.md".into())),
        "an item that was shown and is not there is not what was shown either"
    );
}

#[test]
fn the_record_needs_a_uuid_v7_id_and_a_same_runtime_transfer_needs_no_consent() {
    let t = transfer("cli.claude");
    for bad in ["", "not-a-uuid", "01937a3e-5b2c-4d4e-8f10-0123456789ab"] {
        assert_eq!(
            record(t.clone(), Decision::Granted { by: "rom" }, bad, 1).unwrap_err(),
            HandoffError::BadId(bad.into())
        );
    }
    let same = record(
        transfer("cli.codex"),
        Decision::Granted { by: "rom" },
        ID,
        1,
    )
    .unwrap();
    assert_eq!(same.consent.state, "not_needed");
    check_delivery(&same, &same.transfer.items).unwrap();
}

fn plan_of(text: &str) -> Value {
    let wf = Workflow::parse("t", text).unwrap_or_else(|e| panic!("{e}"));
    let dir = tempfile::tempdir().unwrap();
    let agents = discover(None, dir.path(), None, &[], &[]).unwrap().agents;
    compile(&wf, text, &agents, &Redactor::new()).unwrap_or_else(|p| panic!("{p:?}"))
}

#[test]
fn a_plan_lists_every_data_edge_that_crosses_runtimes_as_a_handoff_that_needs_consent() {
    let plan = plan_of(
        "[[step]]\nid = \"draft\"\nagent = \"explore\"\nruntime = \"cli.codex\"\nprompt = \"p {{input}}\"\n\n\
         [[step]]\nid = \"review\"\nagent = \"explore\"\nruntime = \"cli.claude\"\nprompt = \"q {{prev.summary}}\"\n",
    );
    let h = plan["handoffs"].as_array().expect("handoffs");
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["from"], "draft");
    assert_eq!(h[0]["to"], "review");
    assert_eq!(h[0]["from_runtime"], "cli.codex");
    assert_eq!(h[0]["to_runtime"], "cli.claude");
    assert_eq!(h[0]["via"], "prev.summary");
    assert_eq!(h[0]["needs_consent"], true);
}

#[test]
fn a_chain_on_one_runtime_has_no_handoffs_and_the_plan_has_no_such_key() {
    let plan = plan_of(
        "[[step]]\nid = \"a\"\nagent = \"explore\"\nprompt = \"p {{input}}\"\n\n\
         [[step]]\nid = \"b\"\nagent = \"explore\"\nprompt = \"q {{prev.summary}}\"\n",
    );
    assert!(plan.get("handoffs").is_none());
}
