//! Native-only tests for `http.rs`.
//!
//! ~keep A sibling file reached through `#[path]` rather than an inline `mod tests`, mirroring
//! `engine/wasm_crawl_tests.rs`: keeping the tests inline pushed `http.rs` past the
//! `file-too-long` lint limit, and the tests are the part that keeps growing.

use super::*;
use crate::net::ssrf::SsrfPolicy;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Whether the most recent request `mock` received carried an `Authorization` header.
async fn last_request_carried_authorization(mock: &MockServer) -> bool {
    let requests = mock.received_requests().await.expect("request recording must be on");
    requests
        .last()
        .expect("at least one request must have been recorded")
        .headers
        .contains_key("authorization")
}

fn bearer_auth_config() -> CrawlConfig {
    CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: "test-fixture-bearer-token-not-a-real-secret".to_owned(),
        }),
        ssrf: SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        },
        ..CrawlConfig::default()
    }
}

/// #387: `http_fetch` is the fetch path used directly by robots.txt, sitemap and asset
/// requests, none of which necessarily share a host with the crawl that triggered them.
/// An explicit `origin_host` that does not match the request's own host must withhold
/// configured credentials, exactly as a cross-host redirect hop already does.
#[tokio::test]
async fn http_fetch_withholds_credentials_when_origin_host_does_not_match() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&mock)
        .await;
    let config = bearer_auth_config();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client must build");

    let url = format!("{}/x", mock.uri());
    http_fetch(&url, &config, &HashMap::new(), &client, Some("attacker.test"))
        .await
        .expect("fetch must succeed");

    assert!(
        !last_request_carried_authorization(&mock).await,
        "credentials must be withheld when origin_host names a different host"
    );
}

/// The positive control for the test above: an `origin_host` matching the request's own
/// host must still get the configured credentials, proving the withholding above is a
/// real host comparison and not e.g. `http_fetch` silently dropping auth altogether.
#[tokio::test]
async fn http_fetch_attaches_credentials_when_origin_host_matches() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&mock)
        .await;
    let config = bearer_auth_config();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client must build");

    let url = format!("{}/x", mock.uri());
    let origin_host = url::Url::parse(&url)
        .expect("mock URL must parse")
        .host_str()
        .expect("mock URL must have a host")
        .to_owned();
    http_fetch(&url, &config, &HashMap::new(), &client, Some(&origin_host))
        .await
        .expect("fetch must succeed");

    assert!(
        last_request_carried_authorization(&mock).await,
        "credentials must be attached when origin_host matches the request's own host"
    );
}

#[tokio::test]
async fn http_fetch_enforces_config_timeout_without_client_default() {
    const REQUEST_TIMEOUT: Duration = Duration::from_millis(50);
    const RESPONSE_DELAY: Duration = Duration::from_millis(500);
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/slow"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(RESPONSE_DELAY)
                .set_body_string("late"),
        )
        .mount(&mock)
        .await;
    let config = CrawlConfig {
        request_timeout: REQUEST_TIMEOUT,
        ssrf: SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        },
        ..CrawlConfig::default()
    };
    // ~keep WASM has no client-level timeout; a bare native client reproduces that condition.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client must build");
    for endpoint in ["/slow", "/start"] {
        let result = http_fetch(
            &format!("{}{endpoint}", mock.uri()),
            &config,
            &HashMap::new(),
            &client,
            None,
        )
        .await;
        assert!(
            matches!(result, Err(CrawlError::Timeout { .. })),
            "expected timeout for {endpoint}"
        );
    }
}

/// `http_fetch` must populate `final_url` from the reqwest response URL.
///
/// On native targets (Policy::none) this equals the request URL because
/// redirects are not followed transparently.  The test verifies the field
/// is set to a non-empty value matching the requested URL — confirming
/// the plumbing that the wasm path relies on to capture the post-redirect
/// URL is in place.
///
/// Note: the wasm-specific transparent-redirect behaviour (browser `fetch`
/// following 3xx and returning the final URL via `response.url()`) cannot
/// be exercised under `cargo test` because it requires a wasm32 target and
/// a real browser runtime.  The build-time check (`cargo build --target
/// wasm32-unknown-unknown`) verifies the changed code path compiles
/// correctly for wasm.
#[tokio::test]
async fn http_fetch_populates_final_url() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Hello</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = format!("{}/page", mock.uri());
    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    let client = build_client(&config).expect("client must build");
    let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client, None)
        .await
        .expect("http_fetch must succeed");

    assert!(
        !resp.final_url.is_empty(),
        "final_url must not be empty after a successful fetch"
    );
    assert!(
        resp.final_url.contains("/page"),
        "final_url must contain the requested path, got: {}",
        resp.final_url
    );
}

/// Regression test: `http_fetch`'s internal redirect loop used to enforce
/// `config.ssrf.max_redirects` (a `u8` with no public builder setter, default 5)
/// instead of the builder-settable `config.max_redirects`, so `.max_redirects(N)`
/// had no effect on this loop. This proves the builder value now bounds it.
#[tokio::test]
async fn http_fetch_stops_once_builder_max_redirects_is_exceeded() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/hop0"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/hop1"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/hop1"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/hop2"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/hop2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("done"))
        .mount(&mock)
        .await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    config.max_redirects = 1;
    let client = build_client(&config).expect("client must build");
    let url = format!("{}/hop0", mock.uri());
    let result = http_fetch(&url, &config, &std::collections::HashMap::new(), &client, None).await;

    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("must stop once the second hop exceeds max_redirects(1), got Ok"),
    };
    assert!(
        matches!(err, CrawlError::SsrfPolicyViolation { ref reason, .. } if reason == "too many redirects"),
        "expected a too-many-redirects SsrfPolicyViolation, got {err:?}"
    );
}

/// ~keep Regression: `redact_url_credentials` existed and was unit-tested, but every
/// real `SsrfPolicyViolation` site built the variant with a struct literal carrying
/// the raw URL — so a refused `http://user:pass@host/` leaked the credential into API
/// error bodies, MCP payloads and tracing fields. Testing the helper in isolation is
/// exactly what hid that, so this drives a real `http_fetch` rejection instead.
#[tokio::test]
async fn http_fetch_ssrf_rejection_does_not_leak_url_credentials() {
    let config = CrawlConfig::default();
    let client = build_client(&config).expect("client must build");
    let url = "http://alice:hunter2@169.254.169.254/latest/meta-data/";

    let err = match http_fetch(url, &config, &std::collections::HashMap::new(), &client, None).await {
        Err(e) => e,
        Ok(_) => panic!("the link-local metadata address must be refused by the default policy"),
    };

    let rendered = format!("{err}\n{err:?}");
    assert!(
        !rendered.contains("hunter2"),
        "the refused URL's password must never reach the error, got {rendered}"
    );
    assert!(
        !rendered.contains("alice"),
        "the refused URL's username must never reach the error, got {rendered}"
    );
    assert!(
        rendered.contains("169.254.169.254"),
        "the host must survive redaction so the error stays actionable, got {rendered}"
    );
}

/// Same redirect chain as above, but with a builder value large enough to reach the
/// end — proving `.max_redirects(N)` is actually honored (not just enforced too
/// tightly) by the same loop.
#[tokio::test]
async fn http_fetch_follows_full_chain_when_builder_max_redirects_is_sufficient() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/hop0"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/hop1"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/hop1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("done"))
        .mount(&mock)
        .await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    config.max_redirects = 1;
    let client = build_client(&config).expect("client must build");
    let url = format!("{}/hop0", mock.uri());
    let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client, None)
        .await
        .expect("one redirect hop must be followed when max_redirects == 1");

    assert_eq!(
        resp.body, "done",
        "final response body must be the hop1 body, got {:?}",
        resp.body
    );
    assert_eq!(resp.status, 200, "final status must be 200, got {}", resp.status);
}
/// Characterization for the status dispatch `fetch_one_hop` performs: every terminal
/// status maps to one specific error carrying that status. ~keep
///
/// ~keep 429 and 503 reach this table through `challenge::challenge_status_error`, which
/// reads their body first; the bodyless responses below carry no fingerprint, so they fall
/// through to the same `status_error` mapping as the rest.
#[tokio::test]
async fn http_fetch_maps_each_body_free_terminal_status_to_its_own_error() {
    /// (status, the variant it must raise, a fragment its message must carry).
    type StatusCase = (u16, fn(&CrawlError) -> bool, &'static str);

    let cases: &[StatusCase] = &[
        (401, |e| matches!(e, CrawlError::Unauthorized { .. }), "unauthorized"),
        (404, |e| matches!(e, CrawlError::NotFound { .. }), "not_found"),
        (408, |e| matches!(e, CrawlError::Timeout { .. }), "timeout"),
        (410, |e| matches!(e, CrawlError::Gone { .. }), "gone"),
        (429, |e| matches!(e, CrawlError::RateLimited { .. }), "rate_limited"),
        (500, |e| matches!(e, CrawlError::ServerError { .. }), "server_error"),
        (502, |e| matches!(e, CrawlError::BadGateway { .. }), "bad_gateway"),
        (
            503,
            |e| matches!(e, CrawlError::ServerError { .. }),
            "service unavailable",
        ),
        (504, |e| matches!(e, CrawlError::ServerError { .. }), "gateway timeout"),
    ];

    for (status, is_expected_variant, message_fragment) in cases {
        let error = fetch_status(*status, ResponseTemplate::new(*status)).await;
        assert!(
            is_expected_variant(&error),
            "status {status} produced the wrong error variant: {error:?}"
        );
        assert!(
            error.to_string().contains(message_fragment),
            "status {status} message must contain {message_fragment:?}, got: {error}"
        );
        assert_eq!(status::error_status(&error), Some(*status), "{error:?}");
    }
}

/// A 403 that carries no WAF fingerprint is a plain forbidden, not a WAF block.
#[tokio::test]
async fn http_fetch_reports_a_plain_403_as_forbidden() {
    let error = fetch_status(403, ResponseTemplate::new(403).set_body_string("nope")).await;
    assert!(
        matches!(error, CrawlError::Forbidden { .. }),
        "expected Forbidden, got {error:?}"
    );
}

/// A 403 whose body fingerprints must name the vendor the corpus identifies. ~keep
#[tokio::test]
async fn http_fetch_reports_a_waf_block_when_a_403_body_fingerprints() {
    let error = fetch_status(403, ResponseTemplate::new(403).set_body_string("cf-chl- challenge")).await;
    assert!(
        matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
        "expected a cloudflare WafBlocked, got {error:?}"
    );
    assert!(
        error.to_string().contains("waf/blocked detected: cloudflare"),
        "unexpected message: {error}"
    );
}

/// A 503 stamped by a WAF vendor header is a challenge, not a server fault, so it must
/// escalate rather than be retried blindly (crawlberg#169).
#[tokio::test]
async fn http_fetch_reports_a_waf_block_when_a_503_carries_a_vendor_header() {
    let error = fetch_status(
        503,
        ResponseTemplate::new(503)
            .append_header("x-datadome", "blocked")
            .set_body_string("<html>challenge</html>"),
    )
    .await;
    assert!(
        matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "datadome"),
        "expected a datadome WafBlocked, got {error:?}"
    );
    assert!(
        error.to_string().contains("waf/blocked detected on 503: datadome"),
        "unexpected message: {error}"
    );
}

/// A 503 that only fingerprints once its body is read is still a challenge. This is the
/// Cloudflare interstitial shape from crawlberg#169: `server: cloudflare` alone is not a
/// block signal in the corpus, so the header check is inconclusive and the body decides.
#[tokio::test]
async fn http_fetch_reports_a_waf_block_when_a_503_body_fingerprints() {
    let error = fetch_status(
        503,
        ResponseTemplate::new(503)
            .append_header("server", "cloudflare")
            .set_body_string(
                "<html><head><title>Just a moment...</title></head>\
             <body><script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1\"></script>\
             </body></html>",
            ),
    )
    .await;
    assert!(
        matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
        "expected a cloudflare WafBlocked, got {error:?}"
    );
    assert!(
        error.to_string().contains("waf/blocked detected on 503: cloudflare"),
        "unexpected message: {error}"
    );
}

/// A 429 challenge fingerprints exactly like a 503 one.
#[tokio::test]
async fn http_fetch_reports_a_waf_block_when_a_429_challenge_fingerprints() {
    let error = fetch_status(
        429,
        ResponseTemplate::new(429)
            .append_header("x-px-block", "1")
            .set_body_string("<html>px-captcha</html>"),
    )
    .await;
    assert!(
        matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "perimeterx"),
        "expected a perimeterx WafBlocked, got {error:?}"
    );
    assert!(
        error.to_string().contains("waf/blocked detected on 429: perimeterx"),
        "unexpected message: {error}"
    );
}

/// A 503 or 429 carrying no WAF signal must come out of the new classification step
/// untouched: the same variant, the same message, its status still attached, and still
/// retryable for the `retry_codes` that list it (crawlberg#84).
#[tokio::test]
async fn http_fetch_keeps_a_challenge_status_without_a_waf_signal_retryable() {
    let cases: &[(u16, &str)] = &[(503, "service unavailable"), (429, "rate_limited")];
    for (status, message_fragment) in cases {
        let error = fetch_status(
            *status,
            ResponseTemplate::new(*status)
                .append_header("content-type", "text/html")
                .set_body_string("<html><body><h1>Service Unavailable</h1></body></html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::ServerError { .. } | CrawlError::RateLimited { .. }),
            "status {status} must stay an ordinary retryable error, got {error:?}"
        );
        assert!(
            error.to_string().contains(message_fragment),
            "status {status} message must contain {message_fragment:?}, got: {error}"
        );
        assert_eq!(
            status::error_status(&error),
            Some(*status),
            "status {status} must stay attached to its error: {error:?}"
        );
        assert!(
            should_retry_error(&error, &[*status]),
            "status {status} must still be retryable when retry_codes lists it: {error:?}"
        );
    }
}

/// A 2xx whose headers alone fingerprint is a WAF interstitial, reported before the
/// body is treated as page content. ~keep
///
/// ~keep Also the regression guard for crawlberg#169: this and the body-block test below
/// pin the 2xx wording, which the challenge-status work must not reword. Both pass with
/// and without that change, which is the point of a guard.
#[tokio::test]
async fn http_fetch_reports_a_header_waf_block_on_a_2xx() {
    let error = fetch_status(
        200,
        ResponseTemplate::new(200)
            .append_header("x-datadome", "protected")
            .set_body_string("<html></html>"),
    )
    .await;
    assert!(
        matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "datadome"),
        "expected a datadome WafBlocked, got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("waf/blocked detected on 2xx (header): datadome"),
        "unexpected message: {error}"
    );
}

/// A 2xx that only fingerprints once its body is read is reported as a body block.
#[tokio::test]
async fn http_fetch_reports_a_body_waf_block_on_a_2xx() {
    let error = fetch_status(
        200,
        ResponseTemplate::new(200).set_body_string("<html>cf-chl- x</html>"),
    )
    .await;
    assert!(
        matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
        "expected a cloudflare WafBlocked, got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("waf/blocked detected on 2xx (body): cloudflare"),
        "unexpected message: {error}"
    );
}

/// A 3xx whose `Location` does not resolve to a URL is returned as the response
/// rather than followed or rejected. ~keep
#[tokio::test]
async fn http_fetch_returns_a_3xx_with_an_unresolvable_location_as_the_response() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/here"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "http://")
                .set_body_string("moved"),
        )
        .mount(&mock)
        .await;

    let config = permissive_config();
    let client = build_client(&config).expect("client must build");
    let response = http_fetch(&format!("{}/here", mock.uri()), &config, &HashMap::new(), &client, None)
        .await
        .expect("an unresolvable Location must not fail the fetch");

    assert_eq!(response.status, 302, "the 3xx itself must be returned");
    assert_eq!(response.body, "moved", "its body must be read");
}

fn permissive_config() -> CrawlConfig {
    CrawlConfig {
        ssrf: SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        },
        ..CrawlConfig::default()
    }
}

/// Fetch a single mocked response and return the error it produced.
async fn fetch_status(status: u16, template: ResponseTemplate) -> CrawlError {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/probe"))
        .respond_with(template)
        .mount(&mock)
        .await;

    let config = permissive_config();
    let client = build_client(&config).expect("client must build");
    http_fetch(
        &format!("{}/probe", mock.uri()),
        &config,
        &HashMap::new(),
        &client,
        None,
    )
    .await
    .map(|_| ())
    .expect_err(&format!("status {status} must produce an error"))
}
