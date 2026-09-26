//! Secrets a caller puts into a config must not reach `Debug` output, compact or pretty.

use std::collections::HashMap;

use crawlberg::{AuthConfig, BrowserConfig, CookieInfo, CrawlConfig, ProxyConfig};

const SECRET: &str = "sk-live-9f8e7d6c5b4a";

fn assert_hidden(what: &str, compact: String, pretty: String) {
    for rendered in [compact, pretty] {
        assert!(!rendered.contains(SECRET), "{what} printed the secret: {rendered}");
    }
}

fn every_auth_config() -> Vec<AuthConfig> {
    vec![
        AuthConfig::Basic {
            username: "user".into(),
            password: SECRET.into(),
        },
        AuthConfig::Bearer { token: SECRET.into() },
        AuthConfig::Header {
            name: "X-Api-Key".into(),
            value: SECRET.into(),
        },
    ]
}

fn secret_proxy() -> ProxyConfig {
    ProxyConfig {
        url: format!("http://user:{SECRET}@proxy.internal:8080"),
        username: Some("user".into()),
        password: Some(SECRET.into()),
    }
}

fn secret_browser_config() -> BrowserConfig {
    BrowserConfig {
        endpoint: Some(format!("wss://chrome.example.com/devtools?token={SECRET}")),
        proxy: Some(secret_proxy()),
        ..BrowserConfig::default()
    }
}

#[test]
fn crawl_config_debug_hides_every_secret_field() {
    for auth in every_auth_config() {
        let config = CrawlConfig {
            custom_headers: HashMap::from([("Authorization".to_owned(), format!("Bearer {SECRET}"))]),
            auth: Some(auth),
            proxy: Some(secret_proxy()),
            browser: secret_browser_config(),
            ..CrawlConfig::default()
        };
        assert_hidden("CrawlConfig", format!("{config:?}"), format!("{config:#?}"));
        let compact = format!("{config:?}");
        assert!(
            compact.contains(r#"custom_headers: {"Authorization": "***"}"#),
            "header name must stay visible: {compact}"
        );
    }
}

#[test]
fn browser_config_debug_hides_the_endpoint_token() {
    let config = secret_browser_config();
    assert_hidden("BrowserConfig", format!("{config:?}"), format!("{config:#?}"));
    let compact = format!("{config:?}");
    assert!(
        compact.contains(r#"endpoint: Some("wss://chrome.example.com/devtools?***")"#),
        "endpoint host must stay visible: {compact}"
    );
}

#[test]
fn cookie_info_debug_hides_the_value() {
    let cookie = CookieInfo {
        name: "session".into(),
        value: SECRET.into(),
        domain: Some("example.com".into()),
        path: None,
    };
    assert_hidden("CookieInfo", format!("{cookie:?}"), format!("{cookie:#?}"));
    assert_eq!(
        format!("{cookie:?}"),
        r#"CookieInfo { name: "session", value: Some("***"), domain: Some("example.com"), path: None }"#
    );
}

#[cfg(feature = "browser-chromiumoxide")]
#[test]
fn browser_pool_config_debug_hides_the_endpoint_token() {
    let config = crawlberg::browser_pool::BrowserPoolConfig {
        browser_endpoint: Some(format!(
            "ws://user:{SECRET}@chrome.internal:9222/devtools?token={SECRET}"
        )),
        ..Default::default()
    };
    assert_hidden("BrowserPoolConfig", format!("{config:?}"), format!("{config:#?}"));
}

#[cfg(feature = "browser")]
#[test]
fn session_key_debug_hides_proxy_credentials() {
    let proxy = format!("http://user:{SECRET}@proxy.internal:8080");
    let key = crawlberg::browser_session_pool::SessionKey::from_url("https://example.com/", Some(&proxy)).unwrap();
    assert_hidden("SessionKey", format!("{key:?}"), format!("{key:#?}"));
}
