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
