//! Integration test for a scrape where only some discovered assets download (#443).
//!
//! ~keep `assets::download_single_asset` returns `None` on any fetch failure, and
//! `download_assets` silently drops that `None` -- a failed asset is absent from the
//! result, not reported as an error. That is the existing, intentional behaviour; nothing
//! pinned it, so a change that dropped the whole list on the first failure, or that
//! panicked, would pass every existing test (the only prior asset-download integration
//! test has every mock succeed). This drives a real `scrape()` against a mix of a 404 and
//! two 200s.

use crawlberg::{AssetCategory, CrawlConfig, CrawlEngine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGE_WITH_ASSETS: &str = r#"<html>
<head><link rel="stylesheet" href="/style.css"></head>
<body><img src="/photo.png"><script src="/missing.js"></script></body>
</html>"#;

fn build_engine(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder().config(config).build().unwrap()
}

/// `scrape()` with `download_assets` enabled must still return the assets that did
/// download when a sibling asset fails, and must not surface the failure as a scrape
/// error or drop the successes it already fetched.
#[tokio::test]
async fn should_keep_successful_assets_when_a_sibling_asset_download_fails() {
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
        .and(path("/missing.js"))
        .respond_with(ResponseTemplate::new(404))
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
        .expect("scrape must succeed even though one asset 404s");

    assert_eq!(
        result.assets.len(),
        2,
        "expected the 2 assets that downloaded, got {:?}",
        result.assets
    );

    let stylesheet = result
        .assets
        .iter()
        .find(|a| a.asset_category == AssetCategory::Stylesheet)
        .expect("the stylesheet, which returned 200, must be present");
    assert_eq!(
        stylesheet.size,
        "body { color: red; }".len(),
        "stylesheet size must match served bytes"
    );

    let image = result
        .assets
        .iter()
        .find(|a| a.asset_category == AssetCategory::Image)
        .expect("the image, which returned 200, must be present");
    assert_eq!(image.size, 4, "image size must match served bytes");

    assert!(
        !result.assets.iter().any(|a| a.asset_category == AssetCategory::Script),
        "the 404ing script must not appear as a downloaded asset, got {:?}",
        result.assets
    );
}
