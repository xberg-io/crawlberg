//! HTTP response cache layer for the Tower service stack.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use tower::{Layer, Service};

use super::types::{BodyText, CrawlRequest, CrawlResponse};
use crate::error::CrawlError;
use crate::traits::CrawlCache;
use crate::types::{CachedPage, CrawlConfig};

/// The subset of `Cache-Control` response directives this cache acts on.
///
/// ~keep A crawl cache is a *shared* cache: one entry is replayed to whoever asks for the
/// URL next, which may be a different tenant or a different run. That is why `private` is
/// treated as "do not store" here rather than ignored as a browser cache could, and why
/// `s-maxage` outranks `max-age`.
#[derive(Debug, Default, PartialEq, Eq)]
struct CacheDirectives {
    /// `no-store`: the response must not be written to the cache at all.
    no_store: bool,
    /// `private`: intended for a single user, so a shared cache must not store it.
    private: bool,
    /// `no-cache`: storing is allowed, serving without revalidation is not.
    no_cache: bool,
    /// `s-maxage` if present, else `max-age`, in seconds.
    max_age_secs: Option<u64>,
}

impl CacheDirectives {
    /// Whether a response carrying these directives may be written to a shared cache.
    fn may_store(&self) -> bool {
        !self.no_store && !self.private
    }

    /// Parse every `Cache-Control` header value on a response.
    ///
    /// A header may legitimately appear more than once, and each occurrence may carry
    /// several comma-separated directives, so both levels are flattened. Directive names
    /// are case-insensitive per RFC 9111; an argument may be quoted (`no-cache="set-cookie"`)
    /// and a malformed `max-age` is ignored rather than treated as zero, since guessing
    /// "stale" from a syntax error would silently disable the cache.
    fn parse<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut directives = Self::default();
        let mut max_age = None;
        let mut shared_max_age = None;

        for value in values {
            for token in value.split(',') {
                let token = token.trim();
                let (name, argument) = match token.split_once('=') {
                    Some((name, argument)) => (name.trim(), Some(argument.trim().trim_matches('"'))),
                    None => (token, None),
                };
                match name.to_ascii_lowercase().as_str() {
                    "no-store" => directives.no_store = true,
                    "private" => directives.private = true,
                    "no-cache" => directives.no_cache = true,
                    "max-age" => max_age = argument.and_then(|a| a.parse::<u64>().ok()).or(max_age),
                    "s-maxage" => shared_max_age = argument.and_then(|a| a.parse::<u64>().ok()).or(shared_max_age),
                    _ => {}
                }
            }
        }

        directives.max_age_secs = shared_max_age.or(max_age);
        directives
    }
}

/// Whether `cached` may be served without asking the origin first.
///
/// ~keep `max_age_secs` narrows the backend's TTL, never widens it: the backend has
/// already refused to return anything past its own TTL by the time this runs, so a large
/// origin `max-age` cannot resurrect an entry the operator configured away.
fn is_fresh(cached: &CachedPage, now_secs: u64) -> bool {
    if cached.must_revalidate {
        return false;
    }
    match cached.max_age_secs {
        Some(max_age) => now_secs.saturating_sub(cached.cached_at) < max_age,
        None => true,
    }
}

/// Seconds since the Unix epoch, saturating to 0 if the clock is before it.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// HTTP status for "not modified", answering a conditional request.
const STATUS_NOT_MODIFIED: u16 = 304;

/// Build a [`CrawlResponse`] that replays `cached`.
///
/// `sent_user_agent` is the agent the request pinned before this layer answered from cache
/// (a caller earlier in the chain -- `RedirectPolicy::admits` -- may have chosen one for
/// robots.txt group selection and forced it onto the request); a cache hit never reaches the
/// UA rotation layer, but the caller's robots decision was still made for that agent, so the
/// replayed response must report the same one rather than none.
fn response_from_cache(cached: CachedPage, sent_user_agent: Option<String>) -> CrawlResponse {
    let mut headers = HashMap::new();
    if let Some(ref etag) = cached.etag {
        headers.insert("etag".to_owned(), vec![etag.clone()]);
    }
    if let Some(ref last_modified) = cached.last_modified {
        headers.insert("last-modified".to_owned(), vec![last_modified.clone()]);
    }
    let body_bytes = cached.body.as_bytes().to_vec();
    CrawlResponse {
        status: cached.status_code,
        content_type: cached.content_type,
        body: cached.body,
        body_bytes,
        headers,
        landed: None,
        sent_user_agent,
        soft_error: false,
        // ~keep The entry holds the text of the page. Its `Content-Type` and its `<meta>` tag still
        // ~keep name the character set of the bytes it came from, so it must not be decoded again.
        text: BodyText::Decoded {
            charset: cached.charset,
        },
    }
}

/// Attach the conditional-request headers that let the origin answer 304.
///
/// Returns false when the entry carries no validator, in which case there is nothing to
/// revalidate with and the caller must issue an ordinary request.
fn apply_validators(req: &mut CrawlRequest, cached: &CachedPage) -> bool {
    let mut has_validator = false;
    if let Some(ref etag) = cached.etag {
        req.headers.insert("if-none-match".to_owned(), etag.clone());
        has_validator = true;
    }
    if let Some(ref last_modified) = cached.last_modified {
        req.headers
            .insert("if-modified-since".to_owned(), last_modified.clone());
        has_validator = true;
    }
    has_validator
}

/// Tower layer that caches HTTP responses using a [`CrawlCache`].
pub struct CrawlCacheLayer {
    cache: Arc<dyn CrawlCache>,
    config: Option<Arc<CrawlConfig>>,
}

impl CrawlCacheLayer {
    pub fn new(cache: Arc<dyn CrawlCache>) -> Self {
        Self { cache, config: None }
    }

    /// Pass requests that carry `config`'s credentials straight through, uncached.
    ///
    /// ~keep A shared cache must not reuse a response to an authorized request (RFC 9111
    /// ~keep section 3.5), and the cache key is the URL alone, so one caller's
    /// ~keep authenticated page would be served to the next caller of the same URL.
    pub fn bypassing_credentials(mut self, config: Arc<CrawlConfig>) -> Self {
        self.config = Some(config);
        self
    }
}

impl<S: Clone> Layer<S> for CrawlCacheLayer {
    type Service = CrawlCacheService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CrawlCacheService {
            inner,
            cache: self.cache.clone(),
            config: self.config.clone(),
        }
    }
}

/// Tower service that checks the cache before forwarding requests and stores responses.
#[derive(Clone)]
pub struct CrawlCacheService<S> {
    inner: S,
    cache: Arc<dyn CrawlCache>,
    config: Option<Arc<CrawlConfig>>,
}

impl<S> Service<CrawlRequest> for CrawlCacheService<S>
where
    S: Service<CrawlRequest, Response = CrawlResponse, Error = CrawlError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = CrawlResponse;
    type Error = CrawlError;
    type Future = Pin<Box<dyn Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: CrawlRequest) -> Self::Future {
        let cache = self.cache.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let url = req.url.clone();
        let credentialed = self.config.as_deref().is_some_and(|config| {
            url::Url::parse(&url).is_ok_and(|parsed| crate::net::credentials::is_credentialed(config, &parsed))
        });

        Box::pin(async move {
            if credentialed {
                return inner.call(req).await;
            }
            let mut req = req;
            let sent_user_agent = req.headers.get("user-agent").cloned();

            // ~keep A fresh entry short-circuits; a stored-but-unusable one (expired, or
            // `no-cache`) is still worth a conditional request, so it is carried forward
            // to be validated rather than discarded.
            let revalidating = match cache.get(&url).await {
                Ok(Some(cached)) if is_fresh(&cached, now_secs()) => {
                    return Ok(response_from_cache(cached, sent_user_agent));
                }
                Ok(Some(stale)) => Some(stale),
                _ => cache.get_stale(&url).await.ok().flatten(),
            };

            let revalidating = revalidating.filter(|cached| apply_validators(&mut req, cached));

            let resp = inner.call(req).await?;

            if let Some(cached) = revalidating
                && resp.status == STATUS_NOT_MODIFIED
            {
                tracing::debug!(url = %url, "origin confirmed the cached entry is unchanged");
                let refreshed = CachedPage {
                    cached_at: now_secs(),
                    ..cached
                };
                let _ = cache.set(&url, &refreshed).await;
                return Ok(response_from_cache(refreshed, resp.sent_user_agent.clone()));
            }

            if resp.status >= 200 && resp.status < 300 {
                let directives = CacheDirectives::parse(
                    resp.headers
                        .get("cache-control")
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );

                if directives.may_store() {
                    // ~keep The cache holds text, not bytes, so the entry gets the text of the page:
                    // ~keep the bytes decoded with the character set decided for them. A hit replays
                    // ~keep that text, where a lossy UTF-8 read of the bytes lost every letter of a
                    // ~keep page in another character set.
                    let (text, charset) =
                        crate::html::decode_page(&resp.text, &resp.content_type, &url, &resp.body_bytes);
                    let _ = cache
                        .set(
                            &url,
                            &CachedPage {
                                url: url.clone(),
                                status_code: resp.status,
                                content_type: resp.content_type.clone(),
                                body: text.unwrap_or_else(|| resp.body.clone()),
                                etag: resp.headers.get("etag").and_then(|v| v.first().cloned()),
                                last_modified: resp.headers.get("last-modified").and_then(|v| v.first().cloned()),
                                cached_at: now_secs(),
                                max_age_secs: directives.max_age_secs,
                                must_revalidate: directives.no_cache,
                                charset,
                            },
                        )
                        .await;
                } else {
                    tracing::debug!(
                        url = %url,
                        no_store = directives.no_store,
                        private = directives.private,
                        "not caching: the origin's Cache-Control forbids storing this response"
                    );
                }
            }

            Ok(resp)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::NoopCache;
    use tower::Service;

    /// An entry stored before `charset` existed still loads. Its text is replayed as it is: the
    /// header and the `<meta>` tag it still carries do not decode it again.
    #[test]
    fn an_entry_stored_without_a_charset_still_loads_and_is_not_decoded_again() {
        let stored = r#"{"url":"http://example.com/","status_code":200,
            "content_type":"text/html; charset=iso-8859-1",
            "body":"<meta charset=\"iso-8859-1\"><p>café</p>",
            "etag":null,"last_modified":null,"cached_at":1}"#;
        let entry: CachedPage = serde_json::from_str(stored).expect("an entry without `charset` must load");
        assert_eq!(entry.charset, None);

        let response = response_from_cache(entry, None);
        assert_eq!(response.body, "<meta charset=\"iso-8859-1\"><p>café</p>");
        assert_eq!(response.text, BodyText::Decoded { charset: None });
        assert_eq!(
            crate::html::decode_page(
                &response.text,
                &response.content_type,
                "http://example.com/",
                &response.body_bytes
            ),
            (None, None),
            "the text of the entry must stay as it is"
        );
    }

    #[test]
    fn a_hit_reports_the_character_set_of_its_entry() {
        let entry = CachedPage {
            body: "<p>café</p>".to_owned(),
            charset: Some("shift_jis".to_owned()),
            ..CachedPage::default()
        };
        let stored = serde_json::to_string(&entry).expect("an entry must serialize");
        let loaded: CachedPage = serde_json::from_str(&stored).expect("an entry must load");
        assert_eq!(
            response_from_cache(loaded, None).text,
            BodyText::Decoded {
                charset: Some("shift_jis".to_owned()),
            }
        );
    }

    #[derive(Clone)]
    struct CountingService(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Service<CrawlRequest> for CountingService {
        type Response = CrawlResponse;
        type Error = CrawlError;
        type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;
        fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, _req: CrawlRequest) -> Self::Future {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async {
                Ok(CrawlResponse {
                    status: 200,
                    content_type: "text/html".into(),
                    body: "ok".into(),
                    body_bytes: vec![],
                    headers: HashMap::new(),
                    landed: None,
                    sent_user_agent: None,
                    soft_error: false,
                    text: crate::tower::BodyText::Undecoded,
                })
            })
        }
    }

    /// In-memory cache that records what it was asked to store, so the tests can assert on
    /// the decision the layer made rather than on a side effect two layers away.
    #[derive(Clone, Default)]
    struct RecordingCache {
        entries: std::sync::Arc<std::sync::Mutex<HashMap<String, CachedPage>>>,
        /// Entries returned only by `get_stale`, standing in for TTL-expired ones.
        stale: std::sync::Arc<std::sync::Mutex<HashMap<String, CachedPage>>>,
    }

    #[async_trait::async_trait]
    impl CrawlCache for RecordingCache {
        async fn get(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
            Ok(self.entries.lock().expect("cache mutex").get(key).cloned())
        }

        async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError> {
            self.entries
                .lock()
                .expect("cache mutex")
                .insert(key.to_owned(), page.clone());
            Ok(())
        }

        async fn has(&self, key: &str) -> Result<bool, CrawlError> {
            Ok(self.entries.lock().expect("cache mutex").contains_key(key))
        }

        async fn get_stale(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
            Ok(self.stale.lock().expect("stale mutex").get(key).cloned())
        }
    }

    /// Service returning a fixed response and recording the request headers it saw.
    #[derive(Clone)]
    struct ScriptedService {
        status: u16,
        headers: HashMap<String, Vec<String>>,
        seen_request_headers: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
        /// The agent the origin reports back as actually sent, as a real fetch would.
        sent_user_agent: Option<String>,
    }

    impl ScriptedService {
        fn new(status: u16, response_headers: &[(&str, &str)]) -> Self {
            let mut headers = HashMap::new();
            for (name, value) in response_headers {
                headers.insert((*name).to_owned(), vec![(*value).to_owned()]);
            }
            Self {
                status,
                headers,
                seen_request_headers: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
                sent_user_agent: None,
            }
        }

        /// Report `agent` back on the response, as the agent this hop's request pinned.
        fn with_sent_user_agent(mut self, agent: &str) -> Self {
            self.sent_user_agent = Some(agent.to_owned());
            self
        }
    }

    impl Service<CrawlRequest> for ScriptedService {
        type Response = CrawlResponse;
        type Error = CrawlError;
        type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;
        fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: CrawlRequest) -> Self::Future {
            *self.seen_request_headers.lock().expect("headers mutex") = req.headers.clone();
            let status = self.status;
            let headers = self.headers.clone();
            let sent_user_agent = self.sent_user_agent.clone();
            Box::pin(async move {
                Ok(CrawlResponse {
                    status,
                    content_type: "text/html".into(),
                    body: "fresh from origin".into(),
                    body_bytes: b"fresh from origin".to_vec(),
                    headers,
                    landed: None,
                    sent_user_agent,
                    soft_error: false,
                    text: crate::tower::BodyText::Undecoded,
                })
            })
        }
    }

    #[test]
    fn parses_the_directives_a_shared_cache_must_obey() {
        let directives = CacheDirectives::parse(["no-store, max-age=60"]);
        assert_eq!(
            directives,
            CacheDirectives {
                no_store: true,
                private: false,
                no_cache: false,
                max_age_secs: Some(60),
            },
            "no-store alongside max-age must still forbid storing"
        );
    }

    #[test]
    fn directive_names_are_case_insensitive_and_arguments_may_be_quoted() {
        let directives = CacheDirectives::parse(["Private, No-Cache=\"set-cookie\""]);
        assert!(directives.private, "Private must be recognised regardless of case");
        assert!(
            directives.no_cache,
            "a quoted no-cache argument must still mean no-cache"
        );
    }

    #[test]
    fn s_maxage_outranks_max_age_for_a_shared_cache() {
        let directives = CacheDirectives::parse(["max-age=60, s-maxage=5"]);
        assert_eq!(
            directives.max_age_secs,
            Some(5),
            "s-maxage is the shared-cache lifetime and must win over max-age"
        );
    }

    #[test]
    fn a_malformed_max_age_is_ignored_rather_than_read_as_zero() {
        let directives = CacheDirectives::parse(["max-age=not-a-number"]);
        assert_eq!(
            directives.max_age_secs, None,
            "a syntax error must not be read as 'already stale', which would disable the cache"
        );
    }

    #[test]
    fn directives_are_collected_across_repeated_headers() {
        let directives = CacheDirectives::parse(["private", "max-age=30"]);
        assert!(directives.private, "a directive in an earlier header must be kept");
        assert_eq!(directives.max_age_secs, Some(30), "and merged with later ones");
    }

    #[tokio::test]
    async fn a_no_store_response_is_never_written_to_the_cache() {
        let cache = RecordingCache::default();
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let mut svc = layer.layer(ScriptedService::new(200, &[("cache-control", "no-store")]));

        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert!(
            cache.entries.lock().expect("cache mutex").is_empty(),
            "a no-store response must not be cached"
        );
    }

    #[tokio::test]
    async fn a_private_response_is_never_written_to_a_shared_cache() {
        let cache = RecordingCache::default();
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let mut svc = layer.layer(ScriptedService::new(200, &[("cache-control", "private, max-age=600")]));

        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert!(
            cache.entries.lock().expect("cache mutex").is_empty(),
            "a crawl cache is shared, so a private response must not be stored even with max-age"
        );
    }

    /// crawlberg#423: a cache hit never reaches the UA rotation layer, but a caller earlier
    /// in the chain (`RedirectPolicy::admits`) may have pinned an agent onto the request for
    /// its robots.txt decision. The replayed response must report that same agent back, not
    /// none, or a downstream directive check would fall back to the configured default.
    #[tokio::test]
    async fn a_fresh_cache_hit_reports_the_agent_the_request_pinned() {
        let cache = RecordingCache::default();
        cache.entries.lock().expect("cache mutex").insert(
            "http://a.com".to_owned(),
            CachedPage {
                url: "http://a.com".to_owned(),
                status_code: 200,
                content_type: "text/html".to_owned(),
                body: "cached body".to_owned(),
                etag: None,
                last_modified: None,
                cached_at: now_secs(),
                max_age_secs: Some(600),
                must_revalidate: false,
                charset: None,
            },
        );
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache));
        let mut svc = layer.layer(ScriptedService::new(200, &[]));

        let mut req = CrawlRequest::new("http://a.com");
        req.headers.insert("user-agent".to_owned(), "AgentB".to_owned());
        let resp = svc.call(req).await.unwrap();

        assert_eq!(
            resp.sent_user_agent.as_deref(),
            Some("AgentB"),
            "a fresh cache hit must report the agent pinned on the request that looked it up"
        );
    }

    /// crawlberg#423: the revalidated-hit call site must report the *origin's* answer to the
    /// conditional request, not the request's own pin, since a 304 still comes from a live
    /// fetch through the UA rotation layer.
    #[tokio::test]
    async fn a_revalidated_cache_hit_reports_the_agent_the_304_request_sent() {
        let cache = RecordingCache::default();
        cache.entries.lock().expect("cache mutex").insert(
            "http://a.com".to_owned(),
            CachedPage {
                url: "http://a.com".to_owned(),
                status_code: 200,
                content_type: "text/html".to_owned(),
                body: "stored body".to_owned(),
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
                cached_at: now_secs(),
                max_age_secs: None,
                must_revalidate: true,
                charset: None,
            },
        );
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache));
        let mut svc = layer.layer(ScriptedService::new(STATUS_NOT_MODIFIED, &[]).with_sent_user_agent("AgentB"));

        let resp = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert_eq!(
            resp.sent_user_agent.as_deref(),
            Some("AgentB"),
            "a revalidated cache hit must report the agent the 304 request actually sent"
        );
    }

    #[tokio::test]
    async fn a_cacheable_response_records_its_freshness_lifetime() {
        let cache = RecordingCache::default();
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let mut svc = layer.layer(ScriptedService::new(200, &[("cache-control", "max-age=42")]));

        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        let entries = cache.entries.lock().expect("cache mutex");
        let stored = entries.get("http://a.com").expect("response must have been cached");
        assert_eq!(stored.max_age_secs, Some(42), "the origin's max-age must be recorded");
        assert!(!stored.must_revalidate, "max-age alone does not force revalidation");
    }

    #[tokio::test]
    async fn a_no_cache_entry_is_stored_but_not_served_without_revalidation() {
        let cache = RecordingCache::default();
        cache.entries.lock().expect("cache mutex").insert(
            "http://a.com".to_owned(),
            CachedPage {
                url: "http://a.com".to_owned(),
                status_code: 200,
                content_type: "text/html".to_owned(),
                body: "stored body".to_owned(),
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
                cached_at: now_secs(),
                max_age_secs: None,
                must_revalidate: true,
                charset: None,
            },
        );
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let origin = ScriptedService::new(200, &[]);
        let seen = origin.seen_request_headers.clone();
        let mut svc = layer.layer(origin);

        let resp = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert_eq!(
            resp.body, "fresh from origin",
            "a must-revalidate entry must not be served from the cache without asking the origin"
        );
        assert_eq!(
            seen.lock()
                .expect("headers mutex")
                .get("if-none-match")
                .map(String::as_str),
            Some("\"v1\""),
            "the stored ETag must be sent as If-None-Match"
        );
    }

    #[tokio::test]
    async fn a_304_serves_the_stored_body_and_refreshes_the_entry() {
        let cache = RecordingCache::default();
        let stale_entry = CachedPage {
            url: "http://a.com".to_owned(),
            status_code: 200,
            content_type: "text/html".to_owned(),
            body: "stored body".to_owned(),
            etag: Some("\"v1\"".to_owned()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_owned()),
            cached_at: 1,
            max_age_secs: None,
            must_revalidate: false,
            charset: None,
        };
        cache
            .stale
            .lock()
            .expect("stale mutex")
            .insert("http://a.com".to_owned(), stale_entry);

        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let origin = ScriptedService::new(STATUS_NOT_MODIFIED, &[]);
        let seen = origin.seen_request_headers.clone();
        let mut svc = layer.layer(origin);

        let resp = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert_eq!(
            resp.body, "stored body",
            "a 304 carries no body, so the stored one must be served in its place"
        );
        assert_eq!(
            resp.status, 200,
            "the replayed response must carry the originally cached status"
        );
        {
            let headers = seen.lock().expect("headers mutex");
            assert_eq!(
                headers.get("if-modified-since").map(String::as_str),
                Some("Wed, 21 Oct 2015 07:28:00 GMT"),
                "Last-Modified must be sent as If-Modified-Since"
            );
        }
        let entries = cache.entries.lock().expect("cache mutex");
        let refreshed = entries.get("http://a.com").expect("the entry must be rewritten");
        assert!(
            refreshed.cached_at > 1,
            "a confirmed entry must have its age reset, or it revalidates on every single request"
        );
    }

    #[tokio::test]
    async fn an_entry_past_its_origin_max_age_is_revalidated_rather_than_served() {
        let cache = RecordingCache::default();
        cache.entries.lock().expect("cache mutex").insert(
            "http://a.com".to_owned(),
            CachedPage {
                url: "http://a.com".to_owned(),
                status_code: 200,
                content_type: "text/html".to_owned(),
                body: "stored body".to_owned(),
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
                cached_at: now_secs().saturating_sub(100),
                max_age_secs: Some(10),
                must_revalidate: false,
                charset: None,
            },
        );
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let mut svc = layer.layer(ScriptedService::new(200, &[]));

        let resp = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert_eq!(
            resp.body, "fresh from origin",
            "an entry 100s old with max-age=10 is stale and must not be served"
        );
    }

    #[tokio::test]
    async fn an_entry_within_its_origin_max_age_is_served_without_a_request() {
        let cache = RecordingCache::default();
        cache.entries.lock().expect("cache mutex").insert(
            "http://a.com".to_owned(),
            CachedPage {
                url: "http://a.com".to_owned(),
                status_code: 200,
                content_type: "text/html".to_owned(),
                body: "stored body".to_owned(),
                etag: None,
                last_modified: None,
                cached_at: now_secs(),
                max_age_secs: Some(600),
                must_revalidate: false,
                charset: None,
            },
        );
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(cache.clone()));
        let mut svc = layer.layer(CountingService(counter.clone()));

        let resp = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();

        assert_eq!(resp.body, "stored body", "a fresh entry must be served from the cache");
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a fresh entry must not reach the origin at all"
        );
    }

    #[tokio::test]
    async fn test_noop_cache_always_forwards() {
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(NoopCache));
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut svc = layer.layer(CountingService(counter.clone()));

        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();
        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "noop cache should forward all requests"
        );
    }
}
