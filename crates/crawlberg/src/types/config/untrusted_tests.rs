use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::{UNTRUSTED_CALLER_FORBIDDEN_FIELDS, reject_untrusted_fields};
use crate::{
    AuthConfig, BrowserConfig, CrawlConfig, DispatchProfile, HostMatcher, ProxyConfig, SsrfPolicy, StaticProxyProvider,
};

const EXPECTED_FORBIDDEN_FIELDS: [&str; 14] = [
    "/ssrf",
    "/ssrf_deny_private_explicit",
    "/max_redirects",
    "/proxy",
    "/browser/proxy",
    "/browser/endpoint",
    "/browser/chrome_path",
    "/browser/chrome_args",
    "/browser/eval_script",
    "/browser/session_affinity",
    "/browser_profile",
    "/save_browser_profile",
    "/document_output_dir",
    "/warc_output",
];

fn value_with_pointer(pointer: &str, leaf: Value) -> Value {
    pointer
        .split('/')
        .filter(|segment| !segment.is_empty())
        .rev()
        .fold(leaf, |value, segment| json!({ segment: value }))
}

#[test]
fn should_reject_every_non_null_operator_owned_field() {
    assert_eq!(UNTRUSTED_CALLER_FORBIDDEN_FIELDS, EXPECTED_FORBIDDEN_FIELDS);

    for pointer in EXPECTED_FORBIDDEN_FIELDS {
        let secret = format!("secret-for-{pointer}");
        for non_null in [
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
            json!(secret.clone()),
        ] {
            let error = reject_untrusted_fields(&value_with_pointer(pointer, non_null))
                .expect_err("every non-null operator-owned field must be rejected");
            let displayed = error.to_string();
            let debugged = format!("{error:?}");
            assert_eq!(
                displayed,
                format!("invalid_config: untrusted caller may not set {pointer}")
            );
            assert!(!displayed.contains(&secret), "Display must not expose the field value");
            assert!(!debugged.contains(&secret), "Debug must not expose the field value");
        }
    }
}

#[test]
fn should_allow_null_for_every_operator_owned_field() {
    for pointer in EXPECTED_FORBIDDEN_FIELDS {
        reject_untrusted_fields(&value_with_pointer(pointer, Value::Null))
            .expect("null does not override the operator-owned field");
    }
}

#[test]
fn should_ignore_lookalike_keys_inside_caller_owned_values() {
    let value = json!({
        "auth": {
            "type": "header",
            "name": "/browser/endpoint",
            "value": "proxy"
        },
        "custom_headers": {
            "/ssrf": "caller-secret",
            "proxy": "caller-secret",
            "browser": "caller-secret"
        }
    });

    reject_untrusted_fields(&value).expect("caller-owned values may contain lookalike strings");
}

#[test]
fn should_adopt_every_serializable_operator_owned_field() {
    let mut caller = CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: "caller-token".to_owned(),
        }),
        ssrf: SsrfPolicy {
            deny_private: false,
            allowlist: vec![HostMatcher::suffix(".caller.internal")],
            denylist: vec![HostMatcher::cidr("203.0.113.0/24").expect("literal CIDR is valid")],
            max_redirects: 99,
            scheme_allowlist: vec!["http".to_owned()],
        },
        ssrf_deny_private_explicit: Some(false),
        max_redirects: 88,
        proxy: Some(proxy("http://caller-proxy.invalid:8000")),
        browser: BrowserConfig {
            proxy: Some(proxy("http://caller-browser-proxy.invalid:8001")),
            endpoint: Some("ws://caller-browser.invalid/devtools/browser/secret".to_owned()),
            chrome_path: Some(PathBuf::from("/caller/chrome")),
            chrome_args: vec!["--proxy-server=http://caller-proxy.invalid".to_owned()],
            eval_script: Some("callerScript()".to_owned()),
            session_affinity: false,
            ..BrowserConfig::default()
        },
        browser_profile: Some("caller-profile".to_owned()),
        save_browser_profile: true,
        document_output_dir: Some(PathBuf::from("/caller/documents")),
        warc_output: Some(PathBuf::from("/caller/archive.warc")),
        ..CrawlConfig::default()
    };
    caller
        .custom_headers
        .insert("x-caller-key".to_owned(), "caller-secret".to_owned());

    let mut operator = CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: "operator-token".to_owned(),
        }),
        ssrf: SsrfPolicy {
            deny_private: true,
            allowlist: vec![HostMatcher::suffix(".operator.internal")],
            denylist: vec![HostMatcher::cidr("198.51.100.0/24").expect("literal CIDR is valid")],
            max_redirects: 3,
            scheme_allowlist: vec!["https".to_owned()],
        },
        ssrf_deny_private_explicit: None,
        max_redirects: 4,
        proxy: Some(proxy("http://operator-proxy.invalid:9000")),
        browser: BrowserConfig {
            proxy: Some(proxy("http://operator-browser-proxy.invalid:9001")),
            endpoint: Some("wss://operator-browser.invalid/devtools/browser/capability".to_owned()),
            chrome_path: Some(PathBuf::from("/operator/chrome")),
            chrome_args: vec!["--lang=de".to_owned()],
            eval_script: Some("operatorScript()".to_owned()),
            session_affinity: true,
            ..BrowserConfig::default()
        },
        browser_profile: Some("operator-profile".to_owned()),
        save_browser_profile: false,
        document_output_dir: Some(PathBuf::from("/operator/documents")),
        warc_output: Some(PathBuf::from("/operator/archive.warc")),
        ..CrawlConfig::default()
    };
    operator
        .custom_headers
        .insert("x-operator-key".to_owned(), "operator-secret".to_owned());

    caller.adopt_operator_egress(&operator);

    assert!(caller.ssrf.deny_private);
    assert_eq!(caller.ssrf.allowlist, operator.ssrf.allowlist);
    assert_eq!(caller.ssrf.denylist, operator.ssrf.denylist);
    assert_eq!(caller.ssrf.max_redirects, 3);
    assert_eq!(caller.ssrf.scheme_allowlist, vec!["https".to_owned()]);
    assert_eq!(caller.ssrf_deny_private_explicit, None);
    assert_eq!(caller.max_redirects, 4);
    assert_proxy_eq(caller.proxy.as_ref(), operator.proxy.as_ref());
    assert_proxy_eq(caller.browser.proxy.as_ref(), operator.browser.proxy.as_ref());
    assert_eq!(caller.browser.endpoint, operator.browser.endpoint);
    assert_eq!(caller.browser.chrome_path, operator.browser.chrome_path);
    assert_eq!(caller.browser.chrome_args, operator.browser.chrome_args);
    assert_eq!(caller.browser.eval_script, operator.browser.eval_script);
    assert_eq!(caller.browser.session_affinity, operator.browser.session_affinity);
    assert_eq!(caller.browser_profile, operator.browser_profile);
    assert_eq!(caller.save_browser_profile, operator.save_browser_profile);
    assert_eq!(caller.document_output_dir, operator.document_output_dir);
    assert_eq!(caller.warc_output, operator.warc_output);
    assert_eq!(
        caller.custom_headers,
        [("x-caller-key".to_owned(), "caller-secret".to_owned())].into()
    );
    match caller.auth {
        Some(AuthConfig::Bearer { token }) => assert_eq!(token, "caller-token"),
        other => panic!("caller authentication must be retained, got {other:?}"),
    }
}

#[test]
fn should_adopt_every_runtime_operator_owned_field() {
    let operator_dispatch = DispatchProfile {
        max_total_attempts: 27,
        ..DispatchProfile::default()
    };
    let operator_provider = Arc::new(StaticProxyProvider::empty());
    let operator = CrawlConfig {
        dispatch: Some(operator_dispatch),
        proxy_provider: Some(operator_provider.clone()),
        #[cfg(feature = "browser")]
        browser_pool: Some(crate::BrowserPool::new(crate::BrowserPoolConfig::default())),
        #[cfg(feature = "browser")]
        browser_session_pool: Some(Arc::new(crate::BrowserSessionPool::new())),
        ..CrawlConfig::default()
    };

    let mut caller = CrawlConfig::default();
    caller.adopt_operator_egress(&operator);

    assert_eq!(
        caller.dispatch.as_ref().map(|profile| profile.max_total_attempts),
        Some(27)
    );
    assert!(Arc::ptr_eq(
        caller.proxy_provider.as_ref().expect("operator proxy provider copied"),
        operator
            .proxy_provider
            .as_ref()
            .expect("operator proxy provider configured")
    ));
    #[cfg(feature = "browser")]
    {
        assert!(Arc::ptr_eq(
            caller.browser_pool.as_ref().expect("operator browser pool copied"),
            operator
                .browser_pool
                .as_ref()
                .expect("operator browser pool configured")
        ));
        assert!(Arc::ptr_eq(
            caller
                .browser_session_pool
                .as_ref()
                .expect("operator browser session pool copied"),
            operator
                .browser_session_pool
                .as_ref()
                .expect("operator browser session pool configured")
        ));
    }
}

fn proxy(url: &str) -> ProxyConfig {
    ProxyConfig {
        url: url.to_owned(),
        username: Some("user".to_owned()),
        password: Some("secret".to_owned()),
    }
}

fn assert_proxy_eq(actual: Option<&ProxyConfig>, expected: Option<&ProxyConfig>) {
    let actual = actual.expect("actual proxy configured");
    let expected = expected.expect("expected proxy configured");
    assert_eq!(actual.url, expected.url);
    assert_eq!(actual.username, expected.username);
    assert_eq!(actual.password, expected.password);
}

#[test]
fn adding_a_config_field_requires_an_explicit_trust_classification() {
    let CrawlConfig {
        max_depth: _,
        max_pages: _,
        max_links_per_page: _,
        max_concurrent: _,
        crawl_strategy: _,
        content_filter: _,
        bm25_query: _,
        bm25_threshold: _,
        respect_robots_txt: _,
        soft_http_errors: _,
        user_agent: _,
        stay_on_domain: _,
        allow_subdomains: _,
        include_paths: _,
        exclude_paths: _,
        path_patterns_match_query: _,
        path_patterns_match_url: _,
        dedup_include_query: _,
        strip_tracking_params: _,
        tracking_params: _,
        custom_headers: _,
        request_timeout: _,
        rate_limit_ms: _,
        max_redirects: _,
        retry_count: _,
        retry_codes: _,
        retry_initial_delay_ms: _,
        retry_max_delay_ms: _,
        rate_limit_jitter_ratio: _,
        cookies_enabled: _,
        auth: _,
        max_body_size: _,
        remove_tags: _,
        content: _,
        map_limit: _,
        map_search: _,
        download_assets: _,
        asset_types: _,
        max_asset_size: _,
        browser,
        proxy: _,
        user_agents: _,
        capture_screenshot: _,
        follow_document_urls: _,
        document_url_depth: _,
        download_documents: _,
        document_max_size: _,
        document_mime_types: _,
        document_output_dir: _,
        document_content_encoding: _,
        warc_output: _,
        browser_profile: _,
        save_browser_profile: _,
        ssrf: _,
        ssrf_deny_private_explicit: _,
        dispatch: _,
        credential_scope: _,
        #[cfg(feature = "browser")]
            browser_pool: _,
        proxy_provider: _,
        #[cfg(feature = "browser")]
            browser_session_pool: _,
    } = CrawlConfig::default();

    let BrowserConfig {
        mode: _,
        backend: _,
        endpoint: _,
        timeout: _,
        overall_timeout: _,
        shutdown_timeout: _,
        wait: _,
        wait_selector: _,
        extra_wait: _,
        proxy: _,
        block_url_patterns: _,
        eval_script: _,
        robots_user_agent: _,
        capture_network_events: _,
        session_affinity: _,
        chrome_path: _,
        chrome_args: _,
    } = browser;
}
