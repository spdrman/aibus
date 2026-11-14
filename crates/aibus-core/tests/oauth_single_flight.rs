//! #115: two requests that both find an expired token do one refresh. The flow is async and
//! takes a cancel token. Red until `OAuthFlow` is async and the store single-flights.

use std::sync::Arc;
use std::time::Duration;

use aibus_core::config;
use aibus_core::{CredentialStore, FakeOAuth, MemoryKeychain, OAuthFlow, Redactor};
use tokio_util::sync::CancellationToken;

fn account() -> config::Account {
    config::Account {
        name: "work".into(),
        provider: "fake".into(),
        auth: config::Auth {
            oauth: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn expired_store() -> (CredentialStore, Arc<FakeOAuth>, config::Account) {
    let mut flow = FakeOAuth::new(1_000, 3_600);
    flow.refresh_delay = Duration::from_millis(80);
    let first = flow
        .authorize(&|_| {}, &CancellationToken::new())
        .await
        .unwrap();
    // The first token ran out at 4,600. A refresh is issued from `now`, so move it on: a token
    // from the exchange has to outlive the test clock, or the next caller finds it expired too
    // and rightly refreshes again.
    flow.now = 10_000;
    let flow = Arc::new(flow);
    // The clock is past the first token's expiry, so every caller finds it expired.
    let store = CredentialStore::new(Arc::new(MemoryKeychain::default()), Redactor::new())
        .with_oauth(flow.clone())
        .with_wall_clock(|| 1_000 + 3_600 + 10);
    let a = account();
    store.store_oauth(&a.account_name(), &first).unwrap();
    (store, flow, a)
}

#[tokio::test]
async fn two_async_resolves_on_an_expired_token_do_one_refresh() {
    let (store, flow, a) = expired_store().await;
    let cancel = CancellationToken::new();
    let (x, y) = tokio::join!(
        store.resolve_async(&a, &cancel),
        store.resolve_async(&a, &cancel)
    );
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!(
        flow.refreshes.lock().unwrap().len(),
        1,
        "one exchange for two callers"
    );
    assert_eq!(x.secret.expose(), y.secret.expose());
    assert_eq!(x.secret.expose(), "oauth-access-2-abcdef");
}

#[test]
fn threads_on_the_sync_resolve_share_one_refresh() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let (store, flow, a) = rt.block_on(expired_store());
    std::thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| {
                assert_eq!(
                    store.resolve(&a).unwrap().secret.expose(),
                    "oauth-access-2-abcdef"
                )
            });
        }
    });
    assert_eq!(flow.refreshes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_cancelled_refresh_is_an_auth_error_and_keeps_the_stored_tokens() {
    let (store, flow, a) = expired_store().await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let e = store
        .resolve_async(&a, &cancel)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("cancelled"), "{e}");
    assert_eq!(flow.refreshes.lock().unwrap().len(), 1);
    let again = store
        .resolve_async(&a, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(again.secret.expose(), "oauth-access-2-abcdef");
}
