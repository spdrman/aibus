//! The seven steps of the selection chain, one table row or more each. Every account is the
//! fake provider (models `fake-small` and `fake-large`), told apart by its alias table: an alias
//! resolves only on the accounts that define it, a full id on all of them.

use std::io;
use std::sync::Mutex;

use tracing_subscriber::fmt::MakeWriter;

use super::*;

const CONFIG: &str = r#"
[defaults]
account = "personal"
model = "sonnet"

[models]
pinned = "kimi/fake-large"
big = "fake-large"

[[account]]
name = "work"
provider = "fake"
auth = { none = true }
tags = ["default"]
aliases = { sonnet = "fake-small" }

[[account]]
name = "personal"
provider = "fake"
auth = { none = true }
aliases = { sonnet = "fake-small", mine = "fake-small" }

[[account]]
name = "kimi"
provider = "fake"
auth = { none = true }
tags = ["cheap"]
aliases = { fast = "fake-large" }

[[account]]
name = "asleep"
provider = "fake"
auth = { none = true }
enabled = false
aliases = { dormant = "fake-small" }
"#;

fn registry(edit: impl FnOnce(&mut Config)) -> AccountRegistry {
    let mut cfg: Config = toml::from_str(CONFIG).unwrap();
    edit(&mut cfg);
    let providers = ProviderRegistry::new(
        &[Box::new(aibus_provider::fake::Factory)],
        &BTreeMap::new(),
    )
    .unwrap();
    AccountRegistry::new(&cfg, Arc::new(providers))
}

fn name(n: &str) -> AccountName {
    AccountName(n.into())
}

fn alias(a: &str) -> ModelRef {
    ModelRef::Alias(a.into())
}

fn full(id: &str) -> ModelRef {
    ModelRef::Full(ModelId(id.into()))
}

fn pair(account: &str, model: &str) -> Result<(AccountName, ModelId), SelectError> {
    Ok((name(account), ModelId(model.into())))
}

fn hints() -> SelectHints {
    SelectHints::default()
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl io::Write for Log {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Log {
    type Writer = Log;
    fn make_writer(&'a self) -> Log {
        self.clone()
    }
}

fn logged(f: impl FnOnce()) -> String {
    let log = Log::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(sub, f);
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    text
}

/// Issue 55. A callsite's first hit registers it against whatever subscribers are alive at
/// that moment. When another test thread does that while the capture is being installed, the
/// callsite can end up cached as "never" and the capture sees nothing. Each of the 4096
/// callsites below is fresh, so each round is one more chance for the race.
#[test]
fn issue_55_capture_sees_a_fresh_callsite_that_another_thread_hits_first() {
    use std::sync::{Arc, Barrier};

    // Every expansion of `one` is its own callsite; each level multiplies by four.
    macro_rules! one { () => { (|| tracing::info!("hammer")) as fn() }; }
    macro_rules! x4 { () => { vec![one!(), one!(), one!(), one!()] }; }
    macro_rules! x16 { () => { [x4!(), x4!(), x4!(), x4!()].concat() }; }
    macro_rules! x64 { () => { [x16!(), x16!(), x16!(), x16!()].concat() }; }
    macro_rules! x256 { () => { [x64!(), x64!(), x64!(), x64!()].concat() }; }
    macro_rules! x1024 { () => { [x256!(), x256!(), x256!(), x256!()].concat() }; }
    let sites: Vec<fn()> = [x1024!(), x1024!(), x1024!(), x1024!()].concat();
    let gate = Arc::new(Barrier::new(2));
    let rival = {
        let gate = gate.clone();
        let sites = sites.clone();
        std::thread::spawn(move || {
            for fire in sites {
                gate.wait();
                fire();
            }
        })
    };
    let total = sites.len();
    let mut missed = 0;
    for fire in sites {
        gate.wait();
        if !logged(fire).contains("hammer") {
            missed += 1;
        }
    }
    rival.join().unwrap();
    assert_eq!(missed, 0, "{missed} of {total} events never reached the capture");
}

#[test]
fn step_1_the_explicit_account_beats_the_agent_the_parent_and_the_default() {
    let r = registry(|_| {});
    let h = SelectHints {
        explicit: Some(name("work")),
        agent: Some(name("kimi")),
        inherit: Some((name("kimi"), ModelId("fake-large".into()))),
        ..hints()
    };
    assert_eq!(r.select(&alias("sonnet"), &h), pair("work", "fake-small"));
}

#[test]
fn step_1_is_a_demand_not_a_hint() {
    let r = registry(|_| {});
    let with = |a: &str| SelectHints {
        explicit: Some(name(a)),
        ..hints()
    };
    assert_eq!(r.select(&alias("sonnet"), &with("nope")), Err(SelectError::Unknown(name("nope"))));
    assert_eq!(r.select(&alias("dormant"), &with("asleep")), Err(SelectError::Disabled(name("asleep"))));
    assert_eq!(
        r.select(&alias("sonnet"), &with("kimi")),
        Err(SelectError::CannotServe { account: name("kimi"), model: "sonnet".into() })
    );
}

#[test]
fn step_2_the_definitions_account_comes_next() {
    let r = registry(|_| {});
    let h = SelectHints {
        agent: Some(name("work")),
        inherit: Some((name("personal"), ModelId("fake-small".into()))),
        ..hints()
    };
    assert_eq!(r.select(&alias("sonnet"), &h), pair("work", "fake-small"));
    let bad = SelectHints {
        agent: Some(name("kimi")),
        ..hints()
    };
    assert!(matches!(r.select(&alias("sonnet"), &bad), Err(SelectError::CannotServe { .. })));
}

#[test]
fn step_3_inherit_takes_the_parents_account_and_model() {
    let r = registry(|_| {});
    let h = SelectHints {
        inherit: Some((name("kimi"), ModelId("fake-large".into()))),
        ..hints()
    };
    assert_eq!(r.select(&ModelRef::Inherit, &h), pair("kimi", "fake-large"));
}

#[test]
fn step_3_a_model_that_resolves_on_the_parents_account_stays_there() {
    let r = registry(|_| {});
    let h = SelectHints {
        inherit: Some((name("work"), ModelId("fake-large".into()))),
        ..hints()
    };
    // The default account is personal, but sonnet resolves on the parent's work.
    assert_eq!(r.select(&alias("sonnet"), &h), pair("work", "fake-small"));
}

#[test]
fn step_3_gives_way_when_the_model_does_not_resolve_on_the_parents_account() {
    let r = registry(|_| {});
    let h = SelectHints {
        inherit: Some((name("kimi"), ModelId("fake-large".into()))),
        ..hints()
    };
    assert_eq!(r.select(&alias("sonnet"), &h), pair("personal", "fake-small"));
}

#[test]
fn inherit_without_a_parent_means_the_configured_default_model() {
    let r = registry(|_| {});
    assert_eq!(r.select(&ModelRef::Inherit, &hints()), pair("personal", "fake-small"));
    let none = registry(|c| c.defaults = config::Defaults::default());
    assert_eq!(
        none.select(&ModelRef::Inherit, &hints()),
        Err(SelectError::NoAccount { model: "inherit".into() })
    );
}

#[test]
fn step_4_the_default_account_wins_over_config_order() {
    let r = registry(|_| {});
    // work comes first in the file, but defaults.account is personal.
    assert_eq!(r.select(&alias("sonnet"), &hints()), pair("personal", "fake-small"));
}

#[test]
fn step_4_gives_way_when_the_default_account_cannot_serve_the_model() {
    let r = registry(|_| {});
    assert_eq!(r.select(&alias("fast"), &hints()), pair("kimi", "fake-large"));
}

#[test]
fn step_5_an_alias_pinned_to_an_account_picks_it() {
    let r = registry(|_| {});
    assert_eq!(r.select(&alias("pinned"), &hints()), pair("kimi", "fake-large"));
}

#[test]
fn step_5_other_accounts_do_not_serve_a_pinned_alias_even_when_they_define_it() {
    let r = registry(|c| {
        c.models.insert("sonnet".into(), "kimi/fake-large".into());
    });
    assert_eq!(r.select(&alias("sonnet"), &hints()), pair("kimi", "fake-large"));
}

#[test]
fn step_5_a_pin_to_a_disabled_account_serves_nothing() {
    let r = registry(|c| {
        c.models.insert("ghost".into(), "asleep/fake-small".into());
    });
    assert_eq!(
        r.select(&alias("ghost"), &hints()),
        Err(SelectError::NoAccount { model: "ghost".into() })
    );
}

#[test]
fn step_6_several_accounts_resolve_so_config_order_decides_and_the_others_are_logged() {
    // No default account, so the chain reaches step 6.
    let r = registry(|c| c.defaults = config::Defaults::default());
    let text = logged(|| {
        assert_eq!(r.select(&full("fake-large"), &hints()), pair("work", "fake-large"));
    });
    assert!(
        text.contains("model fake-large served by work (also: personal, kimi)"),
        "{text}"
    );
}

#[test]
fn step_6_prefers_an_account_with_a_wanted_tag() {
    let r = registry(|c| c.defaults = config::Defaults::default());
    let tagged = |tag: &str| SelectHints {
        prefer_tags: vec![tag.into()],
        ..hints()
    };
    assert_eq!(r.select(&full("fake-large"), &tagged("cheap")), pair("kimi", "fake-large"));
    assert_eq!(r.select(&full("fake-large"), &tagged("default")), pair("work", "fake-large"));
    // A tag nobody has changes nothing.
    assert_eq!(r.select(&full("fake-large"), &tagged("eu")), pair("work", "fake-large"));
}

#[test]
fn step_6_exactly_one_account_is_chosen_without_a_log_line() {
    let r = registry(|c| c.defaults = config::Defaults::default());
    let text = logged(|| {
        assert_eq!(r.select(&alias("mine"), &hints()), pair("personal", "fake-small"));
    });
    assert!(!text.contains("also:"), "{text}");
}

#[test]
fn step_6_a_disabled_account_never_serves() {
    let r = registry(|c| c.defaults = config::Defaults::default());
    assert_eq!(
        r.select(&alias("dormant"), &hints()),
        Err(SelectError::NoAccount { model: "dormant".into() })
    );
}

#[test]
fn step_7_no_account_is_exit_4_with_a_hint_the_banner_can_show() {
    let r = registry(|_| {});
    let e = r.select(&full("gpt-9"), &hints()).unwrap_err();
    assert_eq!(e, SelectError::NoAccount { model: "gpt-9".into() });
    assert!(e.to_string().contains("aibus account add"));
    assert_eq!(AibusError::from(e).exit_code(), 4);
    for e in [
        SelectError::Unknown(name("x")),
        SelectError::Disabled(name("x")),
        SelectError::CannotServe { account: name("x"), model: "m".into() },
    ] {
        assert_eq!(AibusError::from(e).exit_code(), 4);
    }
}

#[test]
fn an_alias_resolves_through_the_global_table_before_the_accounts_own() {
    let r = registry(|c| {
        c.models.insert("sonnet".into(), "fake-large".into());
    });
    let h = SelectHints {
        explicit: Some(name("work")),
        ..hints()
    };
    assert_eq!(r.select(&alias("sonnet"), &h), pair("work", "fake-large"));
}

#[test]
fn model_ref_tells_aliases_from_full_ids() {
    let r = registry(|_| {});
    assert_eq!(r.model_ref("big"), alias("big"));
    assert_eq!(r.model_ref("fast"), alias("fast"));
    assert_eq!(r.model_ref("fake-small"), full("fake-small"));
    assert_eq!(r.model_ref("inherit"), ModelRef::Inherit);
}

#[test]
fn an_account_whose_provider_is_not_registered_serves_nothing_and_has_no_provider() {
    let mut cfg: Config = toml::from_str(CONFIG).unwrap();
    cfg.accounts[0].provider = "ghost".into();
    let providers =
        ProviderRegistry::new(&[Box::new(aibus_provider::fake::Factory)], &BTreeMap::new()).unwrap();
    let r = AccountRegistry::new(&cfg, Arc::new(providers));
    assert!(matches!(r.provider(&name("work")), Err(ConfigError::UnknownProvider(p)) if p == "ghost"));
    assert!(r.provider(&name("personal")).is_ok());
    let h = SelectHints {
        explicit: Some(name("work")),
        ..hints()
    };
    assert!(matches!(r.select(&alias("sonnet"), &h), Err(SelectError::CannotServe { .. })));
}
