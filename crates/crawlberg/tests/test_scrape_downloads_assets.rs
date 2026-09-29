//! Integration test for asset downloading through `scrape()`.
//!
//! ~keep The existing asset unit tests call `assets::discover_assets`/`download_assets`
//! directly, so a break in the call site inside `scrape_from_crawl_response` that stops
//! wiring discovered assets to the downloader would not turn any test red (#342). This
//! drives the download through a real `CrawlEngine::scrape()` call instead.

use crawlberg::{AssetCategory, CrawlConfig, CrawlEngine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGE_WITH_ASSETS: &str = r#"<html>
<head><link rel="stylesheet" href="/style.css"></head>
<body><img src="/photo.png"><script src="/app.js"></script></body>
</html>"#;

fn build_engine(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder().config(config).build().unwrap()
}

/// `scrape()` with `download_assets` enabled must fetch every discovered asset and report
/// its bytes, not merely return `Ok` for the page fetch.
#[tokio::test]
async fn should_download_and_report_every_discovered_asset_when_scraping() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(PAGE_WITH_ASSETS)
                .append_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/style.css"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("body { color: red; }")
                .append_header("content-type", "text/css"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/photo.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0x89, 0x50, 0x4e, 0x47])
                .append_header("content-type", "image/png"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/app.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("console.log('hi');")
                .append_header("content-type", "application/javascript"),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    config.download_assets = true;
    let engine = build_engine(config);

    let result = engine
        .scrape(&format!("{}/page", mock.uri()))
        .await
        .expect("scrape must succeed");

    assert_eq!(
        result.assets.len(),
        3,
        "expected 3 downloaded assets, got {:?}",
        result.assets
    );

    let stylesheet = result
        .assets
        .iter()
        .find(|a| a.asset_category == AssetCategory::Stylesheet)
        .expect("stylesheet asset must be present");
    assert_eq!(
        stylesheet.size,
        "body { color: red; }".len(),
        "stylesheet size must match served bytes"
    );

    let image = result
        .assets
        .iter()
        .find(|a| a.asset_category == AssetCategory::Image)
        .expect("image asset must be present");
    assert_eq!(image.size, 4, "image size must match served bytes");

    let script = result
        .assets
        .iter()
        .find(|a| a.asset_category == AssetCategory::Script)
        .expect("script asset must be present");
    assert_eq!(
        script.size,
        "console.log('hi');".len(),
        "script size must match served bytes"
    );
}

/// A discovered asset must resolve against the page's `<base href>`, not the page's own
/// directory: the discovery call site inside `scrape_from_crawl_response` must read
/// `page.base_href`, the way link and image discovery already do.
#[tokio::test]
async fn should_resolve_a_downloaded_asset_against_the_base_href() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/dir/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><head><base href="/assets/"></head><body><img src="photo.png"></body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/assets/photo.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0x89, 0x50, 0x4e, 0x47])
                .append_header("content-type", "image/png"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/dir/photo.png"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&mock)
        .await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    config.download_assets = true;
    let engine = build_engine(config);

    let result = engine
        .scrape(&format!("{}/dir/page", mock.uri()))
        .await
        .expect("scrape must succeed");

    assert_eq!(
        result.assets.len(),
        1,
        "expected 1 downloaded asset, got {:?}",
        result.assets
    );
    assert_eq!(
        result.assets[0].url,
        format!("{}/assets/photo.png", mock.uri()),
        "the asset must resolve against the base href, not the page's own directory"
    );
}

/// `scrape()` with `download_assets` left at its default (`false`) must not fetch any
/// asset at all, proving the flag actually gates the download call site.
#[tokio::test]
async fn should_not_download_assets_when_download_assets_is_disabled() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(PAGE_WITH_ASSETS)
                .append_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/style.css"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/photo.png"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/app.js"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&mock)
        .await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    let engine = build_engine(config);

    let result = engine
        .scrape(&format!("{}/page", mock.uri()))
        .await
        .expect("scrape must succeed");

    assert_eq!(
        result.assets.len(),
        0,
        "no assets must be downloaded when download_assets is false, got {:?}",
        result.assets
    );
}
