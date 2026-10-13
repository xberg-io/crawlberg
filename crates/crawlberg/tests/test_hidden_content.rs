use crawlberg::{BrowserMode, ContentConfig, CrawlConfig, CrawlEngine, crawl, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGE: &str = r#"<!doctype html><html><head><title>t</title></head><body>
<p>Visible sentence one.</p>
<p hidden>Hidden attribute sentence.</p>
<div style="display:none">Display none sentence.</div>
<div aria-hidden="true">Aria hidden sentence.</div>
<details><summary>Summary words</summary><p>Collapsed details sentence.</p></details>
<template><p>Template sentence.</p></template>
<p>Visible sentence two.</p>
</body></html>"#;

const KEPT: &str = "Visible sentence one.\n\nHidden attribute sentence.\n\nDisplay none sentence.\n\nAria hidden sentence.\n\n**Summary words**\n\nCollapsed details sentence.\n\nTemplate sentence.\n\nVisible sentence two.";
const DROPPED: &str = "Visible sentence one.\n\nAria hidden sentence.\n\n**Summary words**\n\nCollapsed details sentence.\n\nVisible sentence two.";

fn config(keep: bool, mode: BrowserMode) -> CrawlConfig {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.respect_robots_txt = false;
    config.browser.mode = mode;
    config.browser.backend = crawlberg::BrowserBackend::Chromiumoxide;
    config.content = serde_json::from_value(serde_json::json!({
        "keep_hidden": keep, "extract_metadata": false, "remove_navigation": false,
        "remove_forms": false, "exclude_selectors": [], "preprocessing_preset": "minimal"
    }))
    .expect("valid content config");
    config
}

async fn site() -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(PAGE, "text/html")
                .append_header("cache-control", "max-age=600"),
        )
        .mount(&site)
        .await;
    site
}

#[tokio::test]
async fn should_keep_all_hidden_text_when_enabled_for_http() {
    let site = site().await;
    let engine = create_engine(Some(config(true, BrowserMode::Never))).expect("engine builds");
    let result = scrape(&engine, &site.uri()).await.expect("scrape succeeds");
    assert_eq!(result.markdown.expect("markdown").content.trim(), KEPT);
}

#[tokio::test]
async fn should_preserve_existing_output_when_hidden_text_is_disabled() {
    let site = site().await;
    let engine = create_engine(Some(config(false, BrowserMode::Never))).expect("engine builds");
    let result = scrape(&engine, &site.uri()).await.expect("scrape succeeds");
    assert_eq!(result.markdown.expect("markdown").content.trim(), DROPPED);
    let default: ContentConfig = serde_json::from_str("{}").expect("default content");
    assert_eq!(serde_json::to_value(default).expect("serialize")["keep_hidden"], false);
    // The option decides the output of this page: the same page gives the hidden text when it is on.
    let engine = create_engine(Some(config(true, BrowserMode::Never))).expect("engine builds");
    let result = scrape(&engine, &site.uri()).await.expect("scrape succeeds");
    assert_eq!(result.markdown.expect("markdown").content.trim(), KEPT);
}

#[tokio::test]
async fn should_keep_hidden_text_when_crawling_http_pages() {
    let site = site().await;
    let engine = create_engine(Some(config(true, BrowserMode::Never))).expect("engine builds");
    let result = crawl(&engine, &site.uri()).await.expect("crawl succeeds");
    assert_eq!(result.pages.len(), 1);
    assert_eq!(
        result.pages[0].markdown.as_ref().expect("markdown").content.trim(),
        KEPT
    );
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_convert_hidden_elements_in_the_actual_browser_document() {
    let site = site().await;
    let engine = create_engine(Some(config(true, BrowserMode::Always))).expect("engine builds");
    let result = scrape(&engine, &site.uri()).await.expect("browser scrape succeeds");
    assert!(result.browser_used, "the browser must execute this fetch");
    for phrase in [
        "Hidden attribute sentence.",
        "Display none sentence.",
        "Template sentence.",
    ] {
        assert!(result.html.contains(phrase), "Chrome's returned document lost {phrase}");
    }
    assert_eq!(result.markdown.expect("markdown").content.trim(), KEPT);
}

#[derive(Clone, Default)]
struct MemoryCache(std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, crawlberg::CachedPage>>>);

#[async_trait::async_trait]
impl crawlberg::traits::CrawlCache for MemoryCache {
    async fn get(&self, key: &str) -> Result<Option<crawlberg::CachedPage>, crawlberg::CrawlError> {
        Ok(self.0.lock().expect("cache lock").get(key).cloned())
    }
    async fn set(&self, key: &str, page: &crawlberg::CachedPage) -> Result<(), crawlberg::CrawlError> {
        self.0.lock().expect("cache lock").insert(key.to_owned(), page.clone());
        Ok(())
    }
    async fn has(&self, key: &str) -> Result<bool, crawlberg::CrawlError> {
        Ok(self.0.lock().expect("cache lock").contains_key(key))
    }
}

#[tokio::test]
async fn should_apply_each_engines_hidden_content_choice_to_a_shared_cached_response() {
    let site = site().await;
    let cache = MemoryCache::default();
    let drop_engine = CrawlEngine::builder()
        .config(config(false, BrowserMode::Never))
        .cache(cache.clone())
        .build()
        .expect("drop engine");
    let keep_engine = CrawlEngine::builder()
        .config(config(true, BrowserMode::Never))
        .cache(cache.clone())
        .build()
        .expect("keep engine");
    let first = drop_engine.scrape(&site.uri()).await.expect("initial response");
    assert_eq!(first.markdown.expect("markdown").content.trim(), DROPPED);
    assert_eq!(
        cache.0.lock().expect("cache lock").len(),
        1,
        "the response must be cached"
    );
    let second = keep_engine.scrape(&site.uri()).await.expect("cached response");
    assert_eq!(second.markdown.expect("markdown").content.trim(), KEPT);
    let third = drop_engine.scrape(&site.uri()).await.expect("cached response");
    assert_eq!(third.markdown.expect("markdown").content.trim(), DROPPED);
    assert_eq!(
        site.received_requests().await.expect("recorded requests").len(),
        1,
        "both later engines must use the same cached HTML, not refetch it"
    );
}

const EXCLUSION_PAGE: &str = r#"<!doctype html><html><body>
<p>Visible sentence.</p>
<noscript><p>Excluded fallback sentence.</p></noscript>
<template class="secret"><p>Private template sentence.</p></template>
<template><p>Public template sentence.</p></template>
</body></html>"#;

async fn check_hidden_content_exclusions(mode: BrowserMode) {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(EXCLUSION_PAGE, "text/html"))
        .mount(&site)
        .await;
    let defaults = ContentConfig::default().exclude_selectors;
    assert_eq!(defaults, ["noscript"]);
    let cases = [
        (
            defaults,
            "Visible sentence.\n\nPrivate template sentence.\n\nPublic template sentence.",
        ),
        (
            vec!["noscript".to_owned(), ".secret".to_owned()],
            "Visible sentence.\n\nPublic template sentence.",
        ),
        (vec!["noscript".to_owned(), "template".to_owned()], "Visible sentence."),
    ];
    for (selectors, expected) in cases {
        let mut configuration = config(true, mode.clone());
        configuration.content.exclude_selectors = selectors.clone();
        let engine = create_engine(Some(configuration)).expect("engine builds");
        let result = scrape(&engine, &site.uri()).await.expect("scrape succeeds");
        assert_eq!(result.browser_used, mode == BrowserMode::Always);
        assert_eq!(
            result.markdown.expect("markdown").content.trim(),
            expected,
            "{mode:?}, {selectors:?}"
        );
    }
}

#[tokio::test]
async fn should_apply_default_and_custom_exclusions_when_keeping_hidden_http_content() {
    check_hidden_content_exclusions(BrowserMode::Never).await;
}

const TABLE_EXCLUSION_PAGE: &str = r#"<!doctype html><html><body>
<p>Visible sentence.</p>
<p hidden class="hx">Excluded hidden paragraph.</p>
<table><tr><th>Head</th><th>Other</th></tr>
<tr><td>Shown cell</td><td hidden class="hx">ExcludedHiddenCell</td></tr></table>
</body></html>"#;

/// Pins a DEFECT of the converter on purpose (issue 730): a selector does not remove a table
/// cell, a row or a row group, so a hidden `td` that the caller excluded stays when
/// `keep_hidden` is on. The fix of issue 730 must change the last assertion to `!contains`.
#[tokio::test]
async fn should_still_keep_an_excluded_hidden_table_cell_until_issue_730_is_fixed() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(TABLE_EXCLUSION_PAGE, "text/html"))
        .mount(&site)
        .await;
    let mut configuration = config(true, BrowserMode::Never);
    configuration.content.exclude_selectors = vec!["noscript".to_owned(), ".hx".to_owned()];
    let engine = create_engine(Some(configuration)).expect("engine builds");
    let result = scrape(&engine, &site.uri()).await.expect("scrape succeeds");
    let markdown = result.markdown.expect("markdown").content;
    assert!(markdown.contains("Shown cell"), "{markdown}");
    assert!(
        !markdown.contains("Excluded hidden paragraph."),
        "the selector removes an element that is not a table part: {markdown}"
    );
    assert!(
        markdown.contains("ExcludedHiddenCell"),
        "issue 730 is fixed: assert that the cell is absent: {markdown}"
    );
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_apply_default_and_custom_exclusions_when_keeping_hidden_browser_content() {
    check_hidden_content_exclusions(BrowserMode::Always).await;
}
