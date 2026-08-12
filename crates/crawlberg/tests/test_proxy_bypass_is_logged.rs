//! A `ProxyProvider` that hands back a URL reqwest cannot parse makes the request go
//! DIRECT — reqwest's `Proxy::custom` closure has no error channel, so `None` is the only
//! answer available and `None` means "no proxy". That silently defeats whatever egress
//! control the proxy was there to enforce.
//!
//! The behaviour cannot be made fail-closed from inside the closure, so the requirement is
//! that it is at least never silent. This test asserts the bypass is reported at `ERROR`
//! naming the target host, and that the unparseable proxy URL is left out of the report
//! entirely — it cannot be redacted, so it cannot be logged.

use std::sync::{Arc, Mutex};

use crawlberg::{CrawlConfig, CrawlEngine, ProxyConfig, ProxyProvider};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROXY_PASSWORD: &str = "s3cr3t-proxy-pw";

/// A proxy URL that `Url::parse` rejects, carrying userinfo so a leak of it into a log
/// field would be detectable.
const UNPARSEABLE_PROXY_URL: &str = "://operator:s3cr3t-proxy-pw@proxy.invalid:8080";

/// Records every field of every captured event, formatted the way tracing dispatches
/// `%value`, `?value`, and plain `Display`/`Debug` fields.
struct FieldVisitor<'a>(&'a mut Vec<(String, String)>);

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
}

/// Captured `ERROR`-level events only, so the assertions cannot be satisfied by an
/// incidental `DEBUG`/`WARN` line mentioning the same host.
struct ErrorEventSubscriber {
    sink: Arc<Mutex<Vec<(String, String)>>>,
}

impl tracing::Subscriber for ErrorEventSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() == Level::ERROR
    }

    fn new_span(&self, _attrs: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if *event.metadata().level() != Level::ERROR {
            return;
        }
        let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
        event.record(&mut FieldVisitor(&mut fields));
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

/// Hands out a proxy URL that cannot be parsed, for every host.
#[derive(Debug)]
struct BrokenProxyProvider;

impl ProxyProvider for BrokenProxyProvider {
    fn next_proxy(&self, _host: &str) -> Option<ProxyConfig> {
        Some(ProxyConfig {
            url: UNPARSEABLE_PROXY_URL.to_owned(),
            username: Some("operator".to_owned()),
            password: Some(PROXY_PASSWORD.to_owned()),
        })
    }
}

/// Opts into the SSRF policy's private-network allowance so wiremock's `127.0.0.1` server
/// is reachable. Mirrors `test_crawl_span_credential_redaction.rs::allow_private_network`.
fn allow_private_network() {
    static ALLOW_PRIVATE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

#[tokio::test]
async fn an_unparseable_provider_proxy_url_is_reported_before_the_request_goes_direct() {
    // ~keep #[tokio::test] defaults to a current-thread runtime, so the whole crawl stays
    // on the thread the subscriber guard was set on.
    allow_private_network();

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>reached directly</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let engine = CrawlEngine::builder()
        .config(CrawlConfig::default())
        .with_proxy_provider(Arc::new(BrokenProxyProvider))
        .build()
        .expect("engine must build");

    let sink: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(ErrorEventSubscriber { sink: sink.clone() });

    let seed_url = format!("{}/", mock.uri());
    let result = engine.crawl(&seed_url).await.expect("crawl must succeed");
    assert_eq!(
        result.pages.len(),
        1,
        "the request must still reach the origin directly — that is the bypass being reported"
    );

    let recorded = sink.lock().expect("sink mutex must not be poisoned");
    let messages: Vec<&String> = recorded
        .iter()
        .filter(|(name, _)| name == "message")
        .map(|(_, value)| value)
        .collect();

    assert!(
        messages.iter().any(|message| message.contains("bypassing the proxy")),
        "expected an ERROR reporting the proxy bypass, got {messages:?}"
    );

    assert!(
        recorded
            .iter()
            .any(|(name, value)| name == "target_host" && value.contains("127.0.0.1")),
        "the report must name the host whose request went direct, got {recorded:?}"
    );

    // ~keep `redact_url_credentials` returns its input unchanged when the input does not
    // parse — and an unparseable URL is precisely this branch's premise. Logging the URL
    // here would therefore print any embedded `user:pass@` verbatim, so it must not be
    // logged at all. This asserts the omission, not the redaction.
    assert!(
        recorded.iter().all(|(_, value)| !value.contains(PROXY_PASSWORD)),
        "the proxy password must never reach a log field, got {recorded:?}"
    );
    assert!(
        recorded.iter().all(|(name, _)| name != "proxy_url"),
        "an unparseable proxy URL cannot be redacted, so it must not be recorded at all, got {recorded:?}"
    );
}
