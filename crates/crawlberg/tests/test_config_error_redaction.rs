//! Secrets a caller puts into a config must not reach a validation error's `Display`.
//!
//! `CrawlError` Display text reaches logs and API error bodies, so it is as exposed as
//! `Debug`. Neither error covered here prints the rejected value, not even redacted: the
//! `browser.endpoint` error names only the field, because the endpoint is a capability and
//! the field name is enough to find it; the `proxy.url` error comes from the `crate::proxy`
//! parser (#401), which never interpolates the value at all.

use crawlberg::{BrowserConfig, CrawlConfig, ProxyConfig};

const SECRET: &str = "sk-live-9f8e7d6c5b4a";

#[test]
fn an_unparseable_proxy_url_error_hides_the_password() {
    let config = CrawlConfig {
        proxy: Some(ProxyConfig {
            // ~keep A space in the host makes this unparseable, so the error below is the
            // ~keep `Url::parse` failure branch. #401 made that branch omit the value.
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
        !message.contains("svc-account") && !message.contains("proxy internal"),
        "no part of the rejected URL may reach the error, got: {message}"
    );
    assert_eq!(
        message, "invalid_config: invalid proxy URL: invalid international domain name",
        "the error must name the field and the parse failure, and carry no value: {message}"
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
