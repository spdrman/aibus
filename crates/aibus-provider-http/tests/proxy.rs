//! `HTTP_PROXY` is honoured (09 section 8; `HTTPS_PROXY` goes through the same code, and a TLS
//! proxy cannot be faked in process). This file has one test because it sets an environment
//! variable, and an integration test file is its own process.

use aibus_provider_http::Http;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_request_to_another_host_goes_through_the_proxy_from_the_environment() {
    let proxy = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("via proxy"))
        .mount(&proxy)
        .await;
    std::env::set_var("HTTP_PROXY", proxy.uri());
    std::env::set_var("http_proxy", proxy.uri());
    std::env::remove_var("NO_PROXY");
    std::env::remove_var("no_proxy");

    let http = Http::new();
    let resp = http
        .send(&CancellationToken::new(), |c| c.get("http://upstream.example.test/v1/models"))
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "via proxy");
    let seen = &proxy.received_requests().await.unwrap()[0];
    assert_eq!(seen.headers.get("host").unwrap(), "upstream.example.test");
}
