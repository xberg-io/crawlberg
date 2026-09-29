//! A proxy password never reaches a log line, an error or a `Debug` print (#238, #285, #385).
//!
//! The native backend renders a page that imports a module, through a proxy whose password
//! sits in the separate config fields or in the URL userinfo. Every span and event field is
//! recorded, at every level, and must not hold the password; the proxy must still receive it.

#![cfg(feature = "browser-native")]

use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlEngine, ProxyConfig};
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Every span and event field recorded since the last [`take`].
#[derive(Default)]
struct Observed(Mutex<Vec<String>>);

struct FieldVisitor<'a>(&'a mut Vec<String>);

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }
}

struct Capture(Arc<Observed>);

impl tracing::Subscriber for Capture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        attrs.record(&mut FieldVisitor(&mut self.0.0.lock().expect("capture lock")));
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        values.record(&mut FieldVisitor(&mut self.0.0.lock().expect("capture lock")));
    }

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut FieldVisitor(&mut self.0.0.lock().expect("capture lock")));
    }

    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// The process-wide recorder. The native backend logs from its own threads, so a
/// thread-local default subscriber would miss those lines.
fn observed() -> Arc<Observed> {
    static OBSERVED: OnceLock<Arc<Observed>> = OnceLock::new();
    OBSERVED
        .get_or_init(|| {
            let observed = Arc::new(Observed::default());
            tracing::subscriber::set_global_default(Capture(Arc::clone(&observed)))
                .expect("no other global subscriber in this test binary");
            observed
        })
        .clone()
}

fn take() -> Vec<String> {
    std::mem::take(&mut *observed().0.lock().expect("capture lock"))
}

/// A page that imports a module. The mock answers the absolute-form requests a proxy gets,
/// so it serves as the proxy and the site at once.
async fn site() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(r#"<html><body><script type="module" src="/m.js"></script></body></html>"#),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/m.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/javascript")
                .set_body_string("globalThis.loaded = 1;"),
        )
        .mount(&mock)
        .await;
    mock
}

fn native_config(proxy: ProxyConfig) -> CrawlConfig {
    CrawlConfig {
        proxy: Some(proxy),
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: std::time::Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// Render the page through `proxy` and check the three things a proxy password needs: the
/// render ran through the proxy, the proxy got the password, and no recorded field shows it.
async fn assert_render_hides(mock: &MockServer, proxy: ProxyConfig, user: &str, password: &str, hidden: &[&str]) {
    let _ = observed();
    let engine = CrawlEngine::builder()
        .config(native_config(proxy))
        .build()
        .expect("a native proxy with credentials is a valid config");
    take();
    let result = engine.scrape(&mock.uri()).await;
    let lines = take();
    let error = result.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(result.is_ok(), "the render through the proxy must succeed: {error}");

    let module_line = lines.iter().find(|line| line.contains("Loading ES module"));
    assert!(
        module_line.is_some(),
        "positive twin: the module-load line must be recorded, got {} lines",
        lines.len()
    );
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    );
    let requests = mock.received_requests().await.expect("the mock records requests");
    assert!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/m.js")
            .any(|request| {
                request
                    .headers
                    .get("proxy-authorization")
                    .is_some_and(|value| value.to_str().ok() == Some(expected.as_str()))
            }),
        "the module fetch must reach the proxy with its credentials"
    );
    let leaks: Vec<&String> = lines
        .iter()
        .filter(|line| hidden.iter().any(|secret| line.contains(secret)))
        .collect();
    assert!(leaks.is_empty(), "a recorded field shows the proxy password: {leaks:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_native_render_never_logs_a_proxy_password_from_the_config_fields() {
    let mock = site().await;
    let password = "IMPL385-FIELD-PW-4q7";
    let proxy = ProxyConfig {
        url: mock.uri(),
        username: Some("operator".into()),
        password: Some(password.into()),
    };
    assert_render_hides(&mock, proxy, "operator", password, &[password]).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_native_render_never_logs_a_proxy_password_from_the_url() {
    let mock = site().await;
    let password = "IMPL385-URL-PW-8k2";
    let authority = mock.uri().trim_start_matches("http://").to_owned();
    let proxy = ProxyConfig {
        url: format!("http://operator:{password}@{authority}"),
        username: None,
        password: None,
    };
    assert_render_hides(&mock, proxy, "operator", password, &[password]).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_percent_encoded_proxy_password_reaches_the_proxy_decoded() {
    let mock = site().await;
    let authority = mock.uri().trim_start_matches("http://").to_owned();
    let proxy = ProxyConfig {
        url: format!("http://operator:IMPL385%23ENC%2FPW%3F@{authority}"),
        username: None,
        password: None,
    };
    assert_render_hides(
        &mock,
        proxy,
        "operator",
        "IMPL385#ENC/PW?",
        &["IMPL385#ENC", "IMPL385%23ENC"],
    )
    .await;
}

/// A plain HTTP fetch, with no render, through `proxy`: the proxy must get the credentials.
async fn assert_http_fetch_authenticates(mock: &MockServer, proxy: ProxyConfig, user: &str, password: &str) {
    let mut config = native_config(proxy);
    config.browser.mode = BrowserMode::Never;
    let engine = CrawlEngine::builder()
        .config(config)
        .build()
        .expect("a proxy with credentials is a valid config");
    let result = engine.scrape(&mock.uri()).await;
    let error = result.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(result.is_ok(), "the fetch through the proxy must succeed: {error}");
    assert!(
        password.is_empty() || !error.contains(password),
        "an error shows the proxy password: {error}"
    );
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    );
    let requests = mock.received_requests().await.expect("the mock records requests");
    assert!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/")
            .any(|request| {
                request
                    .headers
                    .get("proxy-authorization")
                    .is_some_and(|value| value.to_str().ok() == Some(expected.as_str()))
            }),
        "the page fetch must reach the proxy with its credentials"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn an_http_fetch_sends_the_proxy_credentials_from_the_config_fields() {
    let mock = site().await;
    let proxy = ProxyConfig {
        url: mock.uri(),
        username: Some("operator".into()),
        password: Some("IMPL385-HTTP-PW-3v9".into()),
    };
    assert_http_fetch_authenticates(&mock, proxy, "operator", "IMPL385-HTTP-PW-3v9").await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn an_http_fetch_sends_a_percent_encoded_proxy_password_decoded() {
    let mock = site().await;
    let authority = mock.uri().trim_start_matches("http://").to_owned();
    let proxy = ProxyConfig {
        url: format!("http://operator:IMPL385%23HTTP%2FPW%3F@{authority}"),
        username: None,
        password: None,
    };
    assert_http_fetch_authenticates(&mock, proxy, "operator", "IMPL385#HTTP/PW?").await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn an_http_fetch_with_only_a_proxy_user_name_sends_it_with_an_empty_password() {
    let mock = site().await;
    let proxy = ProxyConfig {
        url: mock.uri(),
        username: Some("FIX385D-USER-ONLY".into()),
        password: None,
    };
    assert_http_fetch_authenticates(&mock, proxy, "FIX385D-USER-ONLY", "").await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_native_interact_goes_through_the_proxy_with_its_credentials() {
    let mock = site().await;
    let proxy = ProxyConfig {
        url: mock.uri(),
        username: Some("operator".into()),
        password: Some("FIX385D-INTERACT-PW".into()),
    };
    let engine = CrawlEngine::builder()
        .config(native_config(proxy))
        .build()
        .expect("a native proxy with credentials is a valid config");
    let actions = [crawlberg::PageAction::ExecuteJs {
        script: "1".to_string(),
    }];
    let result = engine.interact(&mock.uri(), &actions).await;
    let error = result.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(result.is_ok(), "the interact through the proxy must succeed: {error}");
    assert!(
        !error.contains("FIX385D-INTERACT-PW"),
        "an error shows the proxy password: {error}"
    );
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("operator:FIX385D-INTERACT-PW")
    );
    assert!(
        proxy_authorizations(&mock).await.contains(&expected),
        "the interact page load must reach the proxy with its credentials"
    );
}

/// A provider that hands out one proxy for every host.
#[derive(Debug)]
struct OneProxy(ProxyConfig);

impl crawlberg::ProxyProvider for OneProxy {
    fn next_proxy(&self, _host: &str) -> Option<ProxyConfig> {
        Some(self.0.clone())
    }
}

/// A plain HTTP fetch, with no render, through a provider that returns `proxy`.
async fn fetch_through_provider(mock: &MockServer, proxy: ProxyConfig) -> Result<(), String> {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.browser.mode = BrowserMode::Never;
    let engine = CrawlEngine::builder()
        .config(config)
        .with_proxy_provider(Arc::new(OneProxy(proxy)))
        .build()
        .expect("a proxy provider is a valid config");
    engine.scrape(&mock.uri()).await.map(|_| ()).map_err(|e| e.to_string())
}

async fn proxy_authorizations(mock: &MockServer) -> Vec<String> {
    let requests = mock.received_requests().await.expect("the mock records requests");
    requests
        .iter()
        .filter(|request| request.url.path() == "/")
        .filter_map(|request| request.headers.get("proxy-authorization"))
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_provider_proxy_with_credentials_gets_them_as_proxy_authorization_and_logs_no_password() {
    let in_fields = |authority: &str| ProxyConfig {
        url: format!("http://{authority}"),
        username: Some("operator".into()),
        password: Some("FIX385D-PROV-PW-7c1".into()),
    };
    let in_url = |authority: &str| ProxyConfig {
        url: format!("http://operator:FIX385D%23PROV%2FPW@{authority}"),
        username: None,
        password: None,
    };
    for (make, password) in [
        (&in_fields as &dyn Fn(&str) -> ProxyConfig, "FIX385D-PROV-PW-7c1"),
        (&in_url, "FIX385D#PROV/PW"),
    ] {
        let mock = site().await;
        let proxy = make(mock.uri().trim_start_matches("http://"));
        drop(take());
        let result = fetch_through_provider(&mock, proxy).await;
        let recorded = take();
        let error = result.as_ref().err().cloned().unwrap_or_default();
        assert!(
            result.is_ok(),
            "the fetch through the provider's proxy must succeed: {error}"
        );
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("operator:{password}"))
        );
        assert!(
            proxy_authorizations(&mock).await.contains(&expected),
            "the page fetch must reach the provider's proxy with its credentials"
        );
        assert!(
            recorded.iter().all(|field| !field.contains("FIX385D")),
            "a recorded field shows the proxy password: {recorded:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_provider_proxy_with_an_unencoded_password_is_refused_with_an_error_that_hides_it() {
    for template in [
        "http://operator:4242#FIX385D-TAIL@AUTHORITY",
        "http://operator:pa@127.0.0.1:9#FIX385D-TAIL@AUTHORITY",
    ] {
        let mock = site().await;
        let url = template.replace("AUTHORITY", mock.uri().trim_start_matches("http://"));
        drop(take());
        let proxy = ProxyConfig {
            url: url.clone(),
            username: None,
            password: None,
        };
        let result = fetch_through_provider(&mock, proxy).await;
        let recorded = take();
        assert!(
            recorded.iter().any(|field| field.contains("bypassing the proxy")),
            "{url}: the refused proxy must be reported: {recorded:?}"
        );
        assert!(
            recorded.iter().any(|field| field.contains("percent-encode")),
            "{url}: the report must name the fix: {recorded:?}"
        );
        let shown = format!("{recorded:?} {result:?}");
        for part in ["FIX385D-TAIL", "pa@127"] {
            assert!(!shown.contains(part), "{url}: '{part}' is shown: {shown}");
        }
        assert!(
            proxy_authorizations(&mock).await.is_empty(),
            "{url}: a refused proxy must get no credentials"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial(proxy_credentials)]
async fn a_native_render_through_a_scheme_no_client_speaks_names_the_scheme_and_hides_the_credential() {
    let mock = site().await;
    let authority = mock.uri().trim_start_matches("http://").to_owned();
    for scheme in ["socks5", "socks5h", "ftp"] {
        let proxy = ProxyConfig {
            url: format!("{scheme}://operator:FIX385D-SCHEME-PW@{authority}"),
            username: None,
            password: None,
        };
        let error = match CrawlEngine::builder().config(native_config(proxy)).build() {
            Err(error) => error.to_string(),
            Ok(engine) => engine
                .scrape(&mock.uri())
                .await
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| panic!("{scheme}: a render through a {scheme} proxy must fail")),
        };
        assert!(
            error.contains(&format!("'{scheme}'")),
            "{scheme}: the error must name the scheme: {error}"
        );
        assert!(
            !error.contains("FIX385D-SCHEME-PW"),
            "{scheme}: the error shows the credential: {error}"
        );
        assert!(
            !error.contains("operator"),
            "{scheme}: the error shows the user name: {error}"
        );
    }
    let requests = mock.received_requests().await.expect("the mock records requests");
    assert!(
        requests.is_empty(),
        "a refused proxy must not reach the site: {requests:?}"
    );
}
