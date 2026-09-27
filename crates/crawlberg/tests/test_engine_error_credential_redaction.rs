//! Engine errors must never carry the password of a URL that holds `user:password@`.
//!
//! Each test drives a public entry point with a credentialed URL into one error the engine
//! builds from that URL, then asserts the password is absent and the rest of the message is
//! present, so an empty or unrelated error cannot pass. The engine takes the userinfo off the
//! URL when it admits it, so each error names the URL without any userinfo.

use std::sync::Arc;

use async_trait::async_trait;
use crawlberg::http::HttpResponse;
use crawlberg::traits::{Frontier, FrontierEntry};
use crawlberg::{
    AntibotError, AntibotStrategy, AttemptOutcome, BrowserMode, CrawlConfig, CrawlEngine, CrawlError, Decision,
    DispatchProfile, DynAntibotStrategy, DynRetryPolicy, EscalationReason, EscalationStrategy, InMemoryFrontier,
    RetryDirective, RetryPolicy, WafSignal, create_engine,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PASSWORD: &str = "s3cret";
/// How an admitted `credentialed_url` starts: no userinfo, redacted or otherwise.
const REDACTED_HOST: &str = "http://127.0.0.1";

/// `mock`'s address for `path`, with `user:s3cret@` userinfo.
fn credentialed_url(mock: &MockServer, path: &str) -> String {
    let uri = mock.uri();
    let host = uri.strip_prefix("http://").expect("wiremock serves plain http");
    format!("http://user:{PASSWORD}@{host}{path}")
}

/// Asserts `text` holds `expected` (so the error is the one under test) and not the password.
fn assert_redacted(text: &str, expected: &str) {
    assert!(
        text.contains(expected),
        "the error must be the one under test and contain '{expected}', got '{text}'"
    );
    assert!(
        !text.contains(PASSWORD),
        "the URL's password must never reach the error, got '{text}'"
    );
}

/// `Display` and `Debug` together: a caller can print either.
fn rendered(err: &CrawlError) -> String {
    format!("{err}\n{err:?}")
}

// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read in this binary's other tests.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

async fn serve(mock: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .mount(mock)
        .await;
}

fn html_page() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_string("<html><body>page</body></html>")
        .append_header("content-type", "text/html")
}

#[derive(Debug)]
struct EscalatingPolicy(EscalationReason);

#[async_trait]
impl RetryPolicy for EscalatingPolicy {
    async fn decide(&self, _outcome: &AttemptOutcome) -> RetryDirective {
        RetryDirective::Escalate { reason: self.0.clone() }
    }

    fn name(&self) -> &'static str {
        "escalating"
    }
}

#[derive(Debug)]
struct EscalateBrowserStrategy;

#[async_trait]
impl AntibotStrategy for EscalateBrowserStrategy {
    async fn pre_request(&self, _url: &str) -> Result<(), AntibotError> {
        Ok(())
    }

    async fn post_response(&self, _response: &HttpResponse, _waf_signal: Option<&WafSignal>) -> Decision {
        Decision::EscalateBrowser
    }
}

/// Every escalation reason that ends a fetch with no tier left reports the URL redacted.
#[tokio::test]
async fn an_escalation_with_no_tier_left_does_not_print_the_password() {
    let mock = MockServer::start().await;
    serve(&mock, "/page", html_page()).await;
    let url = credentialed_url(&mock, "/page");

    let retry = |reason: EscalationReason| -> (Option<DynRetryPolicy>, Option<DynAntibotStrategy>) {
        (Some(Arc::new(EscalatingPolicy(reason))), None)
    };
    let cases = [
        (
            "waf/blocked: acme",
            retry(EscalationReason::WafBlocked { vendor: "acme".into() }),
        ),
        ("soft_block", retry(EscalationReason::SoftBlock)),
        ("js_render_needed", retry(EscalationReason::RenderNeeded)),
        ("origin_unreliable", retry(EscalationReason::OriginUnreliable)),
        (
            "antibot strategy forced browser escalation",
            (None, Some(Arc::new(EscalateBrowserStrategy) as DynAntibotStrategy)),
        ),
    ];

    // ~keep Every case runs before anything is asserted, so one leaking arm cannot hide another.
    let mut failures = Vec::new();
    for (expected, (retry_policy, antibot_strategy)) in cases {
        let handle = create_engine(Some(CrawlConfig {
            dispatch: Some(DispatchProfile {
                retry_policy,
                antibot_strategy,
                strategy: EscalationStrategy::None,
                ..DispatchProfile::default()
            }),
            browser: crawlberg::BrowserConfig {
                mode: BrowserMode::Never,
                ..Default::default()
            },
            ..allow_private_config()
        }))
        .expect("engine build must not fail");

        let err = crawlberg::scrape(&handle, &url)
            .await
            .expect_err("an escalation with no tier left must fail the scrape");
        let text = rendered(&err);
        if !text.contains(expected) || !text.contains(REDACTED_HOST) || text.contains(PASSWORD) {
            failures.push(format!("[{expected}] {text}"));
        }
    }
    assert!(
        failures.is_empty(),
        "each escalation error must name its reason and host and never the password, got:\n{}",
        failures.join("\n")
    );
}

/// A 404 reports the URL it was raised for, redacted.
#[tokio::test]
async fn a_not_found_page_does_not_print_the_password() {
    let mock = MockServer::start().await;
    serve(&mock, "/missing", ResponseTemplate::new(404)).await;
    let handle = create_engine(Some(allow_private_config())).expect("engine build must not fail");

    let err = crawlberg::scrape(&handle, &credentialed_url(&mock, "/missing"))
        .await
        .expect_err("a 404 must fail the scrape");
    let text = rendered(&err);
    assert_redacted(&text, "not_found");
    assert_redacted(&text, "/missing");
    assert_redacted(&text, REDACTED_HOST);
}

/// A redirect to an address that does not parse is not followed, and the address is never printed.
#[tokio::test]
async fn a_refused_unparseable_redirect_target_does_not_print_the_password() {
    let mock = MockServer::start().await;
    let unparseable = format!("http://user:{PASSWORD}@ex ample.com/next");
    serve(
        &mock,
        "/start",
        ResponseTemplate::new(302).insert_header("location", unparseable.as_str()),
    )
    .await;
    let handle = create_engine(Some(allow_private_config())).expect("engine build must not fail");

    let result = crawlberg::crawl(&handle, &credentialed_url(&mock, "/start"))
        .await
        .expect("an unfollowed redirect ends the crawl, not an Err");
    // ~keep The redirect response itself is the seed page, as `http_fetch` already treats a
    // ~keep `Location` it cannot resolve.
    assert_eq!(
        result.pages.first().map(|page| page.status_code),
        Some(302),
        "the unparseable target must not be followed"
    );
    let text = serde_json::to_string(&result).expect("result serializes");
    assert_redacted(&text, REDACTED_HOST);
}

/// A frontier whose `push` fails, or panics, for every entry.
#[derive(Debug)]
struct BrokenFrontier {
    panic_on_push: bool,
    inner: InMemoryFrontier,
}

#[async_trait]
impl Frontier for BrokenFrontier {
    async fn push(&self, _entry: FrontierEntry) -> Result<(), CrawlError> {
        assert!(!self.panic_on_push, "frontier backend crashed");
        Err(CrawlError::other("frontier backend unavailable"))
    }

    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }

    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }

    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        self.inner.is_seen(url).await
    }

    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        self.inner.mark_seen(url).await
    }
}

fn engine_with_broken_frontier(panic_on_push: bool) -> CrawlEngine {
    CrawlEngine::builder()
        .config(allow_private_config())
        .frontier(BrokenFrontier {
            panic_on_push,
            inner: InMemoryFrontier::default(),
        })
        .build()
        .expect("engine build must not fail")
}

/// A frontier backend failure reports the URL it could not push, redacted.
#[tokio::test]
async fn a_frontier_push_failure_does_not_print_the_password() {
    let mock = MockServer::start().await;
    serve(&mock, "/", html_page()).await;
    let engine = engine_with_broken_frontier(false);

    let err = engine
        .crawl(&credentialed_url(&mock, "/"))
        .await
        .expect_err("a failed seed push must fail the crawl");
    let text = rendered(&err);
    assert_redacted(&text, "onto the crawl frontier failed");
    assert_redacted(&text, REDACTED_HOST);
}

/// A batch task that panics reports the URL it was processing, redacted.
#[tokio::test]
async fn a_panicked_batch_task_does_not_print_the_password() {
    let mock = MockServer::start().await;
    serve(&mock, "/", html_page()).await;
    let engine = engine_with_broken_frontier(true);
    let url = credentialed_url(&mock, "/");

    let results = engine.batch_crawl(&[url.as_str()]).await;
    let [(_, outcome)] = results.as_slice() else {
        panic!("one seed must give one result, got {}", results.len());
    };
    let err = outcome.as_ref().expect_err("a panicked task must report an error");
    let text = rendered(err);
    assert_redacted(&text, "task panicked while processing");
    assert_redacted(&text, REDACTED_HOST);
}
