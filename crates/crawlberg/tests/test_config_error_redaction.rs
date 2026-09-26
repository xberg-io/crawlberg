//! Secrets a caller puts into a config must not reach a validation error's `Display`.
//!
//! `CrawlError` Display text reaches logs and API error bodies, so it is as exposed as
//! `Debug`. Both errors covered here fire *precisely* when the value cannot be parsed as a
//! URL, which is also when the parsing redaction helpers return their input unchanged — so
//! redaction that relies on parsing is a no-op exactly here.

use crawlberg::{BrowserConfig, CrawlConfig, ProxyConfig};

const SECRET: &str = "sk-live-9f8e7d6c5b4a";

#[test]
fn an_unparseable_proxy_url_error_hides_the_password() {
    let config = CrawlConfig {
        proxy: Some(ProxyConfig {
            // ~keep A space in the host makes this unparseable, so the error below is the
            // ~keep `Url::parse` failure branch — the one where parsing-based redaction is
            // ~keep a no-op.
            url: format!("http://svc-account:{SECRET}@proxy internal:8080"),
            username: Some("svc-account".into()),
            password: Some(SECRET.into()),
        }),
        ..CrawlConfig::default()
    };

    let error = config
        .validate()
        .expect_err("a proxy URL with a space in its host must be rejected");
    let message = error.to_string();

    assert!(!message.contains(SECRET), "the password printed: {message}");
    assert!(
        message.contains("invalid proxy URL 'http://***@proxy internal:8080'"),
        "the host must stay visible and the userinfo must be replaced, got: {message}"
    );
}

#[test]
fn a_non_websocket_browser_endpoint_error_does_not_echo_the_endpoint() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            endpoint: Some(format!("https://chrome.example.com/devtools?token={SECRET}")),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::default()
    };

    let error = config
        .validate()
        .expect_err("an endpoint that is not ws:// or wss:// must be rejected");
    let message = error.to_string();

    assert!(!message.contains(SECRET), "the endpoint token printed: {message}");
    assert_eq!(
        message, "invalid_config: browser.endpoint must start with ws:// or wss://",
        "the message must name the field and nothing else"
    );
}
