//! The seven steps of the selection chain, one table row or more each. Every account is the
//! fake provider (models `fake-small` and `fake-large`), told apart by its alias table: an alias
//! resolves only on the accounts that define it, a full id on all of them.

use std::cell::RefCell;
use std::io;
use std::sync::Once;

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
    install_log_router();
    let mut cfg: Config = toml::from_str(CONFIG).unwrap();
    edit(&mut cfg);
    let providers =
        ProviderRegistry::new(&[Box::new(aibus_provider::fake::Factory)], &BTreeMap::new())
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

thread_local! {
    static CAPTURED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Writes into the calling thread's `CAPTURED`, so parallel tests never see each other's lines.
struct Route;

impl io::Write for Route {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        CAPTURED.with(|c| c.borrow_mut().extend_from_slice(b));
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A scoped subscriber is only seen by callsites that are registered after it exists, and a
/// callsite another test thread hits first can be cached as "never" (#55). One global
/// subscriber, installed before any test fires an event, is alive at every registration.
fn install_log_router() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let sub = tracing_subscriber::fmt()
            .with_writer(|| Route)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(sub).unwrap();
    });
}

fn logged(f: impl FnOnce()) -> String {
    install_log_router();
    CAPTURED.with(|c| c.borrow_mut().clear());
    f();
    String::from_utf8(CAPTURED.with(|c| c.take())).unwrap()
}

/// Issue 55. A callsite's first hit registers it against whatever subscribers are alive at
/// that moment. When another test thread does that while the capture is being installed, the
/// callsite can end up cached as "never" and the capture sees nothing. Each of the 4096
/// callsites below is fresh, so each round is one more chance for the race.
#[test]
fn issue_55_capture_sees_a_fresh_callsite_that_another_thread_hits_first() {
    use std::sync::Barrier;

    // Every expansion of `one` is its own callsite; each level multiplies by four.
    macro_rules! one {
        () => {
            (|| tracing::info!("hammer")) as fn()
        };
    }
    macro_rules! x4 {
        () => {
            vec![one!(), one!(), one!(), one!()]
        };
    }
    macro_rules! x16 {
        () => {
            [x4!(), x4!(), x4!(), x4!()].concat()
        };
    }
    macro_rules! x64 {
        () => {
            [x16!(), x16!(), x16!(), x16!()].concat()
        };
    }
    macro_rules! x256 {
        () => {
            [x64!(), x64!(), x64!(), x64!()].concat()
        };
    }
    macro_rules! x1024 {
        () => {
            [x256!(), x256!(), x256!(), x256!()].concat()
        };
    }
    let sites: Vec<fn()> = [x1024!(), x1024!(), x1024!(), x1024!()].concat();
    // Every real test gets this from `registry`, before it fires anything.
    install_log_router();
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
    assert_eq!(
        missed, 0,
        "{missed} of {total} events never reached the capture"
    );
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
    assert_eq!(
        r.select(&alias("sonnet"), &with("nope")),
        Err(SelectError::Unknown(name("nope")))
    );
    assert_eq!(
        r.select(&alias("dormant"), &with("asleep")),
        Err(SelectError::Disabled(name("asleep")))
    );
    assert_eq!(
        r.select(&alias("sonnet"), &with("kimi")),
        Err(SelectError::CannotServe {
            account: name("kimi"),
            model: "sonnet".into()
        })
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
    assert!(matches!(
        r.select(&alias("sonnet"), &bad),
        Err(SelectError::CannotServe { .. })
    ));
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
    assert_eq!(
        r.select(&alias("sonnet"), &h),
        pair("personal", "fake-small")
    );
}

#[test]
fn inherit_without_a_parent_means_the_configured_default_model() {
    let r = registry(|_| {});
    assert_eq!(
        r.select(&ModelRef::Inherit, &hints()),
        pair("personal", "fake-small")
    );
    let none = registry(|c| c.defaults = config::Defaults::default());
    assert_eq!(
        none.select(&ModelRef::Inherit, &hints()),
        Err(SelectError::NoAccount {
            model: "inherit".into()
        })
    );
}

#[test]
fn step_4_the_default_account_wins_over_config_order() {
    let r = registry(|_| {});
    // work comes first in the file, but defaults.account is personal.
    assert_eq!(
        r.select(&alias("sonnet"), &hints()),
        pair("personal", "fake-small")
    );
}

#[test]
fn step_4_gives_way_when_the_default_account_cannot_serve_the_model() {
    let r = registry(|_| {});
    assert_eq!(
        r.select(&alias("fast"), &hints()),
        pair("kimi", "fake-large")
    );
}

#[test]
fn step_5_an_alias_pinned_to_an_account_picks_it() {
    let r = registry(|_| {});
    assert_eq!(
        r.select(&alias("pinned"), &hints()),
        pair("kimi", "fake-large")
    );
}

#[test]
fn step_5_other_accounts_do_not_serve_a_pinned_alias_even_when_they_define_it() {
    let r = registry(|c| {
        c.models.insert("sonnet".into(), "kimi/fake-large".into());
    });
    assert_eq!(
        r.select(&alias("sonnet"), &hints()),
        pair("kimi", "fake-large")
    );
}

#[test]
fn step_5_a_pin_to_a_disabled_account_serves_nothing() {
    let r = registry(|c| {
        c.models.insert("ghost".into(), "asleep/fake-small".into());
    });
    assert_eq!(
        r.select(&alias("ghost"), &hints()),
        Err(SelectError::NoAccount {
            model: "ghost".into()
        })
    );
}

#[test]
fn step_6_several_accounts_resolve_so_config_order_decides_and_the_others_are_logged() {
    // No default account, so the chain reaches step 6.
    let r = registry(|c| c.defaults = config::Defaults::default());
    let text = logged(|| {
        assert_eq!(
            r.select(&full("fake-large"), &hints()),
            pair("work", "fake-large")
        );
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
    assert_eq!(
        r.select(&full("fake-large"), &tagged("cheap")),
        pair("kimi", "fake-large")
    );
    assert_eq!(
        r.select(&full("fake-large"), &tagged("default")),
        pair("work", "fake-large")
    );
    // A tag nobody has changes nothing.
    assert_eq!(
        r.select(&full("fake-large"), &tagged("eu")),
        pair("work", "fake-large")
    );
}

#[test]
fn step_6_exactly_one_account_is_chosen_without_a_log_line() {
    let r = registry(|c| c.defaults = config::Defaults::default());
    let text = logged(|| {
        assert_eq!(
            r.select(&alias("mine"), &hints()),
            pair("personal", "fake-small")
        );
    });
    assert!(!text.contains("also:"), "{text}");
}

#[test]
fn step_6_a_disabled_account_never_serves() {
    let r = registry(|c| c.defaults = config::Defaults::default());
    assert_eq!(
        r.select(&alias("dormant"), &hints()),
        Err(SelectError::NoAccount {
            model: "dormant".into()
        })
    );
}

#[test]
fn step_7_no_account_is_exit_4_with_a_hint_the_banner_can_show() {
    let r = registry(|_| {});
    let e = r.select(&full("gpt-9"), &hints()).unwrap_err();
    assert_eq!(
        e,
        SelectError::NoAccount {
            model: "gpt-9".into()
        }
    );
    assert!(e.to_string().contains("aibus account add"));
    assert_eq!(AibusError::from(e).exit_code(), 4);
    for e in [
        SelectError::Unknown(name("x")),
        SelectError::Disabled(name("x")),
        SelectError::CannotServe {
            account: name("x"),
            model: "m".into(),
        },
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
        ProviderRegistry::new(&[Box::new(aibus_provider::fake::Factory)], &BTreeMap::new())
            .unwrap();
    let r = AccountRegistry::new(&cfg, Arc::new(providers));
    assert!(
        matches!(r.provider(&name("work")), Err(ConfigError::UnknownProvider(p)) if p == "ghost")
    );
    assert!(r.provider(&name("personal")).is_ok());
    let h = SelectHints {
        explicit: Some(name("work")),
        ..hints()
    };
    assert!(matches!(
        r.select(&alias("sonnet"), &h),
        Err(SelectError::CannotServe { .. })
    ));
}

/// The fake provider, except that its `fake-small` has no logprobs (the model, not the provider).
struct SmallHasNoLogprobs;

impl aibus_provider::ProviderFactory for SmallHasNoLogprobs {
    fn id(&self) -> aibus_provider::ProviderId {
        aibus_provider::ProviderId("fake")
    }
    fn build(&self, _: &aibus_provider::ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        use aibus_provider::fake::{FakeProvider, Script};
        let over = aibus_provider::CapabilityOverrides {
            logprobs: Some(false),
            ..Default::default()
        };
        Ok(Arc::new(
            FakeProvider::new(Script::new()).with_model_capabilities("fake-small", over),
        ))
    }
    fn auth_methods(&self) -> &'static [aibus_provider::AuthMethodKind] {
        &[aibus_provider::AuthMethodKind::None]
    }
}

#[test]
fn a_model_without_logprobs_on_a_logprob_capable_provider_is_not_preferred_by_the_router() {
    install_log_router();
    let cfg: Config = toml::from_str(
        r#"
[[account]]
name = "first"
provider = "fake"
auth = { none = true }
aliases = { judge = "fake-small" }

[[account]]
name = "second"
provider = "fake"
auth = { none = true }
aliases = { judge = "fake-large" }
"#,
    )
    .unwrap();
    let providers =
        ProviderRegistry::new(&[Box::new(SmallHasNoLogprobs)], &BTreeMap::new()).unwrap();
    let r = AccountRegistry::new(&cfg, Arc::new(providers));

    // Without the hint the first account in the file wins.
    assert_eq!(
        r.select(&alias("judge"), &hints()),
        pair("first", "fake-small")
    );
    // With it, the account whose model reports logprobs does, though both share one provider.
    let h = SelectHints {
        logprobs: true,
        ..hints()
    };
    assert_eq!(r.select(&alias("judge"), &h), pair("second", "fake-large"));
}

fn quota(resets_in: Option<Duration>) -> aibus_provider::ProviderError {
    let mut e =
        aibus_provider::ProviderError::new(aibus_provider::ProviderErrorKind::Quota, "spent");
    e.rate_limit = Some(Box::new(aibus_provider::RateLimitInfo {
        resets_in,
        ..Default::default()
    }));
    e
}

#[tokio::test(start_paused = true)]
async fn a_cooled_down_account_is_skipped_by_select_and_comes_back_after_the_cooldown() {
    let r = registry(|_| {});
    // `sonnet` is on `work` and `personal`; the default account is `personal`.
    assert_eq!(
        r.select(&alias("sonnet"), &hints()),
        pair("personal", "fake-small")
    );
    r.cool(
        &name("personal"),
        FallbackKind::Quota,
        &quota(Some(Duration::from_secs(90))),
    );
    assert!(r.cooled(&name("personal")));
    assert_eq!(
        r.select(&alias("sonnet"), &hints()),
        pair("work", "fake-small")
    );

    tokio::time::advance(Duration::from_secs(89)).await;
    assert_eq!(
        r.select(&alias("sonnet"), &hints()),
        pair("work", "fake-small")
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(!r.cooled(&name("personal")));
    assert_eq!(
        r.select(&alias("sonnet"), &hints()),
        pair("personal", "fake-small")
    );
}

#[tokio::test(start_paused = true)]
async fn the_cooldown_length_follows_the_kind() {
    let r = registry(|_| {});
    let (a, b, c) = (name("work"), name("personal"), name("kimi"));
    r.cool(&a, FallbackKind::Quota, &quota(None));
    r.cool(
        &b,
        FallbackKind::Overloaded,
        &aibus_provider::ProviderError::new(aibus_provider::ProviderErrorKind::Overloaded, "busy"),
    );
    r.cool(
        &c,
        FallbackKind::Auth,
        &aibus_provider::ProviderError::new(aibus_provider::ProviderErrorKind::Auth, "bad key"),
    );
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(r.cooled(&a) && !r.cooled(&b) && r.cooled(&c));
    tokio::time::advance(Duration::from_secs(30 * 60)).await;
    // A quota with no stated reset waits 30 minutes; an auth failure waits for the test.
    assert!(!r.cooled(&a) && r.cooled(&c));
    r.tested(&c);
    assert!(!r.cooled(&c));
}

#[tokio::test(start_paused = true)]
async fn an_explicit_account_is_a_demand_and_ignores_the_cooldown_and_so_does_the_last_resort() {
    let r = registry(|_| {});
    r.cool(&name("kimi"), FallbackKind::Quota, &quota(None));
    let h = SelectHints {
        explicit: Some(name("kimi")),
        ..hints()
    };
    assert_eq!(r.select(&alias("fast"), &h), pair("kimi", "fake-large"));
    // `fast` is only on `kimi`: with nothing else it serves though it is cooling.
    assert_eq!(
        r.select(&alias("fast"), &hints()),
        pair("kimi", "fake-large")
    );
}

#[test]
fn fallback_names_take_the_definition_over_the_account_and_leave_out_unknown_and_disabled() {
    let r = registry(|c| {
        c.accounts[0].fallback = vec!["personal".into(), "asleep".into(), "nobody".into()];
    });
    assert_eq!(r.fallback_names(&name("work"), None), [name("personal")]);
    assert_eq!(
        r.fallback_names(&name("work"), Some(&[name("kimi")])),
        [name("kimi")]
    );
    assert!(r.fallback_names(&name("work"), Some(&[])).is_empty());
    assert!(r.fallback_names(&name("kimi"), None).is_empty());
}
