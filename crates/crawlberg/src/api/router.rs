//! API router setup and configuration.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::Request,
    http::{HeaderValue, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    cors::{AllowOrigin, Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveHeadersLayer,
    trace::TraceLayer,
};

use crate::engine::CrawlEngine;

use utoipa::OpenApi;

use super::{
    error::ApiError,
    handlers,
    openapi::ApiDoc,
    state::{ApiSecurityConfig, ApiState, CORS_ORIGINS_ENV},
};

/// Maximum request body size (10 MB).
const MAX_REQUEST_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Maximum time a request handler may run (5 minutes).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Path exempt from bearer-token authentication: liveness/readiness probes must
/// stay reachable without a credential (standard operational convention).
const AUTH_EXEMPT_PATH: &str = "/health";

/// Create the API router with an explicit [`ApiSecurityConfig`].
///
/// This is crate-internal (the `api` module is `pub(crate)`); `startup::serve`
/// resolves configuration from the environment once (see
/// [`ApiSecurityConfig::from_env`]) and is the only externally reachable entry
/// point. Tests call this directly with an explicit config instead, so they
/// can exercise auth and resource-limit behavior deterministically instead of
/// mutating process environment variables.
///
/// # Arguments
///
/// * `engine` - A shared [`CrawlEngine`] that powers scrape, crawl, and map operations.
pub(crate) fn create_router_with_security(engine: Arc<CrawlEngine>, security: ApiSecurityConfig) -> Router {
    // ~keep Capture config before moving the engine so MCP sessions use the same crawl settings.
    #[cfg(feature = "mcp")]
    let mcp_config = engine.config.clone();

    let auth_token = security.auth_token.clone();
    let state = Arc::new(ApiState::with_security(engine, security));

    let cors_layer = build_cors_layer();

    let router = Router::new()
        .route("/v1/scrape", post(handlers::scrape_handler))
        .route("/v1/crawl", post(handlers::crawl_handler))
        .route(
            "/v1/crawl/{id}",
            get(handlers::crawl_status_handler).delete(handlers::crawl_cancel_handler),
        )
        .route("/v1/map", post(handlers::map_handler))
        .route("/v1/batch/scrape", post(handlers::batch_scrape_handler))
        .route("/v1/batch/scrape/{id}", get(handlers::batch_status_handler))
        .route("/v1/download", post(handlers::download_handler))
        .route("/health", get(handlers::health_handler))
        .route("/version", get(handlers::version_handler))
        .route("/openapi.json", get(openapi_handler))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(SetSensitiveHeadersLayer::new([AUTHORIZATION]))
        .layer(middleware::from_fn(request_timeout))
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BODY_BYTES))
        .layer(cors_layer)
        .layer(CompressionLayer::new())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // ~keep Keep MCP outside timeout/compression middleware because long-lived SSE sessions break under them.
    #[cfg(feature = "mcp")]
    let router = router.nest_service("/mcp", crate::mcp::streamable_http_service(mcp_config));

    // ~keep Auth wraps everything, including /mcp: it's applied last (outermost),
    // ~keep after the nest_service above, unlike the timeout/compression layers.
    router.layer(middleware::from_fn(move |req: Request, next: Next| {
        let token = auth_token.clone();
        enforce_bearer_token(token, req, next)
    }))
}

/// Build the CORS layer from `CRAWLBERG_API_CORS_ORIGINS` (see
/// [`super::state::CORS_ORIGINS_ENV`]).
///
/// Unset or empty: no `Access-Control-Allow-Origin` header is ever added, so
/// browsers enforce same-origin by default (closed). `*`: explicit opt-in to
/// any origin. Otherwise a comma-separated allowlist of exact origins.
fn build_cors_layer() -> CorsLayer {
    let raw = std::env::var(CORS_ORIGINS_ENV).unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return CorsLayer::new();
    }
    if raw == "*" {
        return CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);
    }

    let origins: Vec<HeaderValue> = raw
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();

    if origins.is_empty() {
        return CorsLayer::new();
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(Any)
        .allow_headers(Any)
}

/// Enforce the configured bearer token on every request except [`AUTH_EXEMPT_PATH`].
///
/// `token: None` means authentication is disabled (no `CRAWLBERG_API_TOKEN`
/// configured). `startup::serve` refuses to bind a non-loopback address in that
/// configuration unless the operator explicitly opts out, so reaching this
/// function with `token: None` implies a deliberately open local deployment.
async fn enforce_bearer_token(token: Option<Arc<str>>, req: Request, next: Next) -> Response {
    if req.uri().path() == AUTH_EXEMPT_PATH {
        return next.run(req).await;
    }

    let Some(expected) = token else {
        return next.run(req).await;
    };

    let provided = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match provided {
        Some(candidate) if constant_time_eq(candidate.as_bytes(), expected.as_bytes()) => next.run(req).await,
        _ => ApiError::unauthorized("missing or invalid bearer token").into_response(),
    }
}

/// Compare two byte strings in constant time, so response timing cannot leak
/// information about the configured token.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Handler that returns the OpenAPI JSON schema.
async fn openapi_handler() -> impl IntoResponse {
    let schema = ApiDoc::openapi();
    Json(schema)
}

/// Middleware that enforces a global request timeout.
///
/// If the inner handler does not complete within [`REQUEST_TIMEOUT`], this
/// returns `408 Request Timeout`.
async fn request_timeout(req: Request, next: Next) -> impl IntoResponse {
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(req)).await {
        Ok(response) => response,
        Err(_elapsed) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::types::CrawlConfig;

    use super::*;

    fn test_engine() -> Arc<CrawlEngine> {
        Arc::new(
            CrawlEngine::builder()
                .rate_limiter(crate::defaults::NoopRateLimiter)
                .build()
                .expect("default engine"),
        )
    }

    fn test_engine_with_config(config: CrawlConfig) -> Arc<CrawlEngine> {
        Arc::new(
            CrawlEngine::builder()
                .config(config)
                .rate_limiter(crate::defaults::NoopRateLimiter)
                .build()
                .expect("engine with test config"),
        )
    }

    /// A `CrawlConfig` that allows fetching a local wiremock server without tripping SSRF
    /// private-network protections, and skips robots.txt (the mock mounts no `/robots.txt`).
    fn local_test_config() -> CrawlConfig {
        CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }
    }

    fn urlset(locs: &[&str]) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for loc in locs {
            body.push_str(&format!("<url><loc>{loc}</loc></url>"));
        }
        body.push_str("</urlset>");
        body
    }

    async fn mount_sitemap(mock: &MockServer, locs: &[&str]) {
        let response = ResponseTemplate::new(200)
            .set_body_string(urlset(locs))
            .append_header("content-type", "application/xml");
        Mock::given(method("GET"))
            .and(path("/sitemap.xml"))
            .respond_with(response)
            .mount(mock)
            .await;
    }

    /// POST `/v1/map` and return the discovered URLs in response order.
    async fn map_urls(router: Router, url: &str, search: &str) -> Vec<String> {
        let response = call(
            router,
            json_post("/v1/map", serde_json::json!({ "url": url, "search": search })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "map request must succeed");
        let body = body_json(response).await;
        body["data"]["urls"]
            .as_array()
            .expect("urls array")
            .iter()
            .map(|u| u["url"].as_str().expect("url string").to_owned())
            .collect()
    }

    #[tokio::test]
    async fn test_create_router() {
        let _router = create_router_with_security(test_engine(), ApiSecurityConfig::default());
    }

    async fn call(router: Router, req: HttpRequest<Body>) -> Response {
        router.oneshot(req).await.expect("router handles request")
    }

    fn json_request(method: &str, uri: &str, auth: Option<&str>) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder().method(method).uri(uri);
        if let Some(token) = auth {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).expect("valid request")
    }

    fn json_post(uri: &str, body: serde_json::Value) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method("POST")
            .uri(uri)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("valid request")
    }

    fn crawl_router_and_state() -> (Router, Arc<ApiState>) {
        let state = Arc::new(ApiState::with_security(test_engine(), ApiSecurityConfig::default()));
        let router = Router::new()
            .route("/v1/crawl", post(handlers::crawl_handler))
            .with_state(state.clone());
        (router, state)
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body collects");
        serde_json::from_slice(&bytes).expect("body is valid JSON")
    }

    #[tokio::test]
    async fn request_without_token_is_rejected_when_auth_enabled() {
        let security = ApiSecurityConfig {
            auth_token: Some(Arc::from("s3cr3t")),
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(router, json_request("GET", "/version", None)).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a request with no Authorization header must be rejected when a token is configured"
        );
    }

    #[tokio::test]
    async fn request_with_wrong_token_is_rejected_when_auth_enabled() {
        let security = ApiSecurityConfig {
            auth_token: Some(Arc::from("s3cr3t")),
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(router, json_request("GET", "/version", Some("wrong-token"))).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an incorrect bearer token must be rejected"
        );
    }

    #[tokio::test]
    async fn request_with_correct_token_is_accepted() {
        let security = ApiSecurityConfig {
            auth_token: Some(Arc::from("s3cr3t")),
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(router, json_request("GET", "/version", Some("s3cr3t"))).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the correctly-configured bearer token must be accepted"
        );
    }

    #[tokio::test]
    async fn health_is_exempt_from_auth() {
        let security = ApiSecurityConfig {
            auth_token: Some(Arc::from("s3cr3t")),
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(router, json_request("GET", "/health", None)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/health must stay reachable without a token"
        );
    }

    #[tokio::test]
    async fn requests_pass_through_when_auth_is_disabled() {
        let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());

        let response = call(router, json_request("GET", "/version", None)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "no token configured means auth is disabled"
        );
    }

    #[tokio::test]
    async fn cors_header_absent_by_default() {
        let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());

        let mut req = json_request("GET", "/health", None);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://evil.example.com"));
        let response = call(router, req).await;

        assert!(
            response.headers().get("access-control-allow-origin").is_none(),
            "the default (no CRAWLBERG_API_CORS_ORIGINS) must not send an Access-Control-Allow-Origin header"
        );
    }

    #[tokio::test]
    async fn crawl_over_max_pages_ceiling_is_rejected() {
        let security = ApiSecurityConfig {
            max_pages_ceiling: 10,
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(
            router,
            json_post(
                "/v1/crawl",
                serde_json::json!({ "url": "https://example.com", "maxPages": 1_000_000 }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "max_pages exceeding the server ceiling must be rejected, not silently clamped or accepted"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "BAD_REQUEST", "body: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("max_pages"),
            "error message must mention max_pages: {body}"
        );
    }

    #[tokio::test]
    async fn crawl_zero_max_pages_is_rejected() {
        let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());

        let response = call(
            router,
            json_post(
                "/v1/crawl",
                serde_json::json!({ "url": "https://example.com", "maxPages": 0 }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "max_pages: 0 must be rejected"
        );
    }

    #[tokio::test]
    async fn batch_scrape_over_url_ceiling_is_rejected() {
        let security = ApiSecurityConfig {
            max_batch_urls: 1,
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(
            router,
            json_post(
                "/v1/batch/scrape",
                serde_json::json!({ "urls": ["https://a.example.com", "https://b.example.com"] }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a batch exceeding the server's URL ceiling must be rejected"
        );
    }

    #[tokio::test]
    async fn crawl_with_a_look_around_path_pattern_is_rejected() {
        for field in ["includePaths", "excludePaths"] {
            let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());
            let response = call(
                router,
                json_post(
                    "/v1/crawl",
                    serde_json::json!({ "url": "http://127.0.0.1:9/", field: ["^/docs", "^/(?!private/)"] }),
                ),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "a look-around pattern in {field} must be refused from a REST caller"
            );
            let body = body_json(response).await;
            let message = body["error"]["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(field) && message.contains("^/(?!private/)") && message.contains("look-around"),
                "the error must name the field the caller sent, the pattern and why: {body}"
            );
        }
    }

    #[tokio::test]
    async fn crawl_with_a_malformed_path_pattern_returns_400_without_creating_a_job() {
        for field in ["includePaths", "excludePaths"] {
            let (router, state) = crawl_router_and_state();
            let response = call(
                router,
                json_post(
                    "/v1/crawl",
                    serde_json::json!({ "url": "http://127.0.0.1:9/", field: ["a{2,1}"] }),
                ),
            )
            .await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = body_json(response).await;
            assert_eq!(body["success"], false, "body: {body}");
            assert_eq!(body["error"]["code"], "BAD_REQUEST", "body: {body}");
            let message = body["error"]["message"].as_str().unwrap_or_default();
            assert_eq!(
                message,
                format!(r#"{field} pattern "a{{2,1}}" does not compile"#),
                "the error must name the field and safe rejected pattern"
            );
            assert!(state.jobs.is_empty(), "an invalid request must not create a job");
        }
    }

    #[tokio::test]
    async fn malformed_path_pattern_error_redacts_url_credentials() {
        let (router, state) = crawl_router_and_state();
        let response = call(
            router,
            json_post(
                "/v1/crawl",
                serde_json::json!({
                    "url": "http://127.0.0.1:9/",
                    "includePaths": ["^https://user:REST-PW-284@example.com/["]
                }),
            ),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "BAD_REQUEST", "body: {body}");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert_eq!(
            message, r#"includePaths pattern "[address hidden: it may carry credentials]" does not compile"#,
            "the error must identify the field without exposing credentials"
        );
        assert!(!message.contains("REST-PW-284"), "credential leaked: {body}");
        assert!(state.jobs.is_empty(), "an invalid request must not create a job");
    }

    #[tokio::test]
    async fn crawl_with_plain_path_patterns_is_accepted() {
        let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());
        let response = call(
            router,
            json_post(
                "/v1/crawl",
                serde_json::json!({
                    "url": "http://127.0.0.1:9/",
                    "includePaths": ["^/docs", r"(?-u)\w"],
                    "excludePaths": [r"\?p=\d+"],
                }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "a pattern the regex crate accepts must still be accepted from a REST caller"
        );
    }

    #[tokio::test]
    async fn crawl_is_rejected_at_the_concurrent_job_ceiling() {
        let security = ApiSecurityConfig {
            max_concurrent_jobs: 0,
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(
            router,
            json_post("/v1/crawl", serde_json::json!({ "url": "https://example.com" })),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "a server at its concurrent job ceiling (0 here) must reject new crawl jobs"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "TOO_MANY_REQUESTS", "body: {body}");
    }

    #[tokio::test]
    async fn batch_scrape_is_rejected_at_the_concurrent_job_ceiling() {
        let security = ApiSecurityConfig {
            max_concurrent_jobs: 0,
            ..ApiSecurityConfig::default()
        };
        let router = create_router_with_security(test_engine(), security);

        let response = call(
            router,
            json_post(
                "/v1/batch/scrape",
                serde_json::json!({ "urls": ["https://example.com"] }),
            ),
        )
        .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn scrape_with_unsupported_format_is_rejected() {
        let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());

        let response = call(
            router,
            json_post(
                "/v1/scrape",
                serde_json::json!({ "url": "https://example.com", "formats": ["screenshot"] }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "an unrecognized `formats` value must be rejected at the boundary rather than silently ignored"
        );
        let body = body_json(response).await;
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("screenshot"),
            "error must name the rejected value: {body}"
        );
    }

    #[tokio::test]
    async fn scrape_with_include_tags_is_rejected_as_unsupported() {
        let router = create_router_with_security(test_engine(), ApiSecurityConfig::default());

        let response = call(
            router,
            json_post(
                "/v1/scrape",
                serde_json::json!({ "url": "https://example.com", "includeTags": ["h1"] }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "include_tags has no engine-side knob today and must be rejected, not silently ignored"
        );
    }

    #[test]
    fn constant_time_eq_matches_equal_and_rejects_different() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"tokeN"));
        assert!(!constant_time_eq(b"short", b"longer-value"));
    }

    #[tokio::test]
    async fn map_endpoint_search_matches_a_non_ascii_term_against_a_percent_encoded_path() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_sitemap(&mock, &["https://example.com/café", "https://example.com/other"]).await;

        let router = create_router_with_security(
            test_engine_with_config(local_test_config()),
            ApiSecurityConfig::default(),
        );

        let urls = map_urls(router, &base, "café").await;

        assert_eq!(
            urls,
            vec!["https://example.com/caf%C3%A9".to_owned()],
            "search=\"café\" must match the address map() stores percent-encoded, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn map_endpoint_search_matches_a_non_ascii_term_against_a_punycode_host() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_sitemap(&mock, &["https://bücher.example/x", "https://example.com/other"]).await;

        let router = create_router_with_security(
            test_engine_with_config(local_test_config()),
            ApiSecurityConfig::default(),
        );

        let urls = map_urls(router, &base, "bücher").await;

        assert_eq!(
            urls,
            vec!["https://xn--bcher-kva.example/x".to_owned()],
            "search=\"bücher\" must match the address map() stores as punycode, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn map_endpoint_search_matches_an_ascii_term() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_sitemap(&mock, &["https://example.com/keep-1", "https://example.com/drop-1"]).await;

        let router = create_router_with_security(
            test_engine_with_config(local_test_config()),
            ApiSecurityConfig::default(),
        );

        let urls = map_urls(router, &base, "KEEP").await;

        assert_eq!(
            urls,
            vec!["https://example.com/keep-1".to_owned()],
            "an ASCII search term must still match case-insensitively, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn map_endpoint_answers_a_seed_robots_txt_disallows_as_forbidden() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("User-agent: *\nDisallow: /private\n")
                    .append_header("content-type", "text/plain"),
            )
            .mount(&mock)
            .await;
        let config = CrawlConfig {
            respect_robots_txt: true,
            ..local_test_config()
        };
        let router = create_router_with_security(test_engine_with_config(config), ApiSecurityConfig::default());

        let response = call(
            router,
            json_post(
                "/v1/map",
                serde_json::json!({ "url": format!("{}/private", mock.uri()) }),
            ),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a refusal is not a server fault"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "FORBIDDEN", "{body}");
        assert_eq!(
            body["error"]["message"], "forbidden: robots.txt disallows /private",
            "{body}"
        );
    }
}
