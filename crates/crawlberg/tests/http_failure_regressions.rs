use crawlberg::{CrawlConfig, CrawlError, SsrfPolicy, create_engine, scrape};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config() -> CrawlConfig {
    CrawlConfig {
        ssrf: SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        },
        max_redirects: 3,
        ..CrawlConfig::default()
    }
}

#[tokio::test]
async fn should_fail_self_redirect_two_page_loop_and_overlong_chain() {
    let server = MockServer::start().await;
    for (from, to) in [
        ("/loop", "/loop"),
        ("/a", "/b"),
        ("/b", "/a"),
        ("/0", "/1"),
        ("/1", "/2"),
        ("/2", "/3"),
        ("/3", "/4"),
    ] {
        Mock::given(path(from))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", to))
            .mount(&server)
            .await;
    }
    #[cfg(feature = "browser")]
    let modes = [crawlberg::BrowserMode::Never, crawlberg::BrowserMode::Always];
    #[cfg(not(feature = "browser"))]
    let modes = [crawlberg::BrowserMode::Never];
    for mode in modes {
        let mut config = config();
        config.browser.mode = mode;
        let engine = create_engine(Some(config)).expect("engine");
        for route in ["/loop", "/a", "/0"] {
            let url = format!("{}{route}", server.uri());
            let error = scrape(&engine, &url).await.expect_err("invalid redirect chain");
            assert!(matches!(error, CrawlError::SsrfPolicyViolation { .. }), "{error}");
            assert!(error.to_string().contains("redirect"), "{error}");
            assert!(error.to_string().contains(&server.uri()), "{error}");
        }
    }
}

#[tokio::test]
async fn should_keep_delayed_refresh_content_and_discover_its_link() {
    let server = MockServer::start().await;
    Mock::given(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>target page words</p>"))
        .mount(&server)
        .await;
    for delay in [0, 5, 600] {
        Mock::given(path(format!("/source-{delay}"))).respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "text/html")
            .set_body_string(format!("<html><head><meta http-equiv=\"refresh\" content=\"{delay}; url=/target\"></head><body><p>source page words</p></body></html>"))).mount(&server).await;
    }
    let engine = create_engine(Some(config())).expect("engine");
    for delay in [0, 5, 600] {
        let url = format!("{}/source-{delay}", server.uri());
        let result = scrape(&engine, &url).await.expect("refresh page");
        if delay == 0 {
            assert_eq!(result.final_url, format!("{}/target", server.uri()));
            assert!(
                result
                    .markdown
                    .as_ref()
                    .expect("markdown")
                    .content
                    .contains("target page words")
            );
        } else {
            assert_eq!(result.final_url, url);
            assert!(
                result
                    .markdown
                    .as_ref()
                    .expect("markdown")
                    .content
                    .contains("source page words")
            );
            assert_eq!(
                result.links.iter().map(|link| link.url.as_str()).collect::<Vec<_>>(),
                [format!("{}/target", server.uri())]
            );
        }
    }
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_keep_browser_page_when_delayed_meta_refresh_becomes_due() {
    let server = MockServer::start().await;
    Mock::given(path("/source"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<meta http-equiv=\"refresh\" content=\"5; url=/target#after\"><p>source page words</p><iframe src=\"/frame\"></iframe>",
            "text/html",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>target page words</p>"))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(path("/frame"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<meta http-equiv=\"refresh\" content=\"5; url=/frame-target\"><p>frame source</p>",
            "text/html",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/frame-target"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>frame target</p>", "text/html"))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = config();
    config.browser.mode = crawlberg::BrowserMode::Always;
    config.browser.extra_wait = Some(std::time::Duration::from_secs(6));
    let engine = create_engine(Some(config)).expect("engine");
    let result = scrape(&engine, &format!("{}/source", server.uri()))
        .await
        .expect("browser refresh page");
    assert_eq!(result.final_url, format!("{}/source", server.uri()));
    assert!(
        result
            .markdown
            .as_ref()
            .expect("markdown")
            .content
            .contains("source page words")
    );
    assert_eq!(
        result.links.iter().map(|link| link.url.as_str()).collect::<Vec<_>>(),
        [format!("{}/target#after", server.uri())],
        "source HTML: {}",
        result.html
    );
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_follow_immediate_browser_meta_refresh() {
    let server = MockServer::start().await;
    Mock::given(path("/source"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<meta http-equiv=\"refresh\" content=\"0; url=/target\"><p>source page words</p>",
            "text/html",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>target page words</p>", "text/html"))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = config();
    config.browser.mode = crawlberg::BrowserMode::Always;
    let engine = create_engine(Some(config)).expect("engine");
    let result = scrape(&engine, &format!("{}/source", server.uri()))
        .await
        .expect("immediate browser refresh");
    assert_eq!(result.final_url, format!("{}/target", server.uri()));
    assert!(
        result
            .markdown
            .as_ref()
            .expect("markdown")
            .content
            .contains("target page words")
    );
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_allow_script_navigation_to_a_delayed_refresh_target() {
    let server = MockServer::start().await;
    Mock::given(path("/source"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<meta http-equiv=\"refresh\" content=\"5; url=/target\"><script>setTimeout(() => location.href = '/target', 500)</script><p>source page words</p>", "text/html"))
        .expect(1).mount(&server).await;
    Mock::given(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>target page words</p>", "text/html"))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = config();
    config.browser.mode = crawlberg::BrowserMode::Always;
    config.browser.extra_wait = Some(std::time::Duration::from_secs(2));
    let engine = create_engine(Some(config)).expect("engine");
    let result = scrape(&engine, &format!("{}/source", server.uri()))
        .await
        .expect("script browser navigation");
    assert_eq!(result.final_url, format!("{}/target", server.uri()));
}
