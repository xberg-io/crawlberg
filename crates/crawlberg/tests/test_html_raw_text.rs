//! Integration tests for raw-text element handling: the elements whose content an HTML parser
//! reads as text, such as `script`, `title`, `xmp` and `plaintext`, and the `<base href>` an HTML
//! parser takes from the finished document.
//!
//! These drive the real `scrape()` pipeline so the assertions cover the production
//! parse path, not a unit-test-only helper.

use crawlberg::{BrowserMode, CrawlConfig, crawl, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read in this binary's other tests.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

/// Serve `body` as `text/html` from a fresh mock server and scrape it.
async fn scrape_html(body: &str) -> (String, crawlberg::ScrapeResult) {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).expect("engine should build");
    let base = mock.uri();
    let result = scrape(&handle, &base).await.expect("scrape should succeed");
    (base, result)
}

#[tokio::test]
async fn should_find_links_after_a_comment_opener_inside_script_text() {
    let html = r#"<html><body>
        <script>var marker = "/* <!-- */";</script>
        <a href="/real">real</a>
        <p>tail</p>
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "a `<!--` inside script text must not hide the anchor that follows it"
    );
}

#[tokio::test]
async fn should_find_links_after_a_comment_opener_inside_style_text() {
    let html = r#"<html><head><style>body:after { content: "<!--"; }</style></head>
        <body><a href="/after-style">after</a></body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/after-style")],
        "a `<!--` inside style text must not hide the anchor that follows it"
    );
}

#[tokio::test]
async fn should_not_extract_links_that_only_appear_inside_raw_text() {
    let html = r#"<html><head><title>Real Title</title></head><body>
        <script>document.write('<a href="/from-script">x</a>');</script>
        <style>/* <a href="/from-style">y</a> */</style>
        <textarea><a href="/from-textarea">z</a></textarea>
        <a href="/real">real</a>
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "links inside script/style/textarea text must be ignored, and the real link kept"
    );
    assert_eq!(
        result.metadata.title.as_deref(),
        Some("Real Title"),
        "title text must still populate the title metadata"
    );
}

#[tokio::test]
async fn should_not_extract_images_that_only_appear_inside_raw_text() {
    let html = r#"<html><body>
        <script>var t = '<img src="/from-script.png">';</script>
        <textarea><img src="/from-textarea.png"></textarea>
        <img src="/real.png" alt="real">
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.images.iter().map(|i| i.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real.png")],
        "images inside script/textarea text must be ignored, and the real image kept"
    );
}

#[tokio::test]
async fn should_ignore_a_base_href_inside_title_text() {
    let html = r#"<html><head><title>Home <base href="https://hijacked.example/"></title></head>
        <body><a href="/page">page</a></body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/page")],
        "a `<base href>` inside title text must not change the document base"
    );
    assert!(
        result
            .metadata
            .title
            .as_deref()
            .is_some_and(|t| t.trim_start().starts_with("Home")),
        "the title metadata must still carry the title text, got {:?}",
        result.metadata.title
    );
}

#[tokio::test]
async fn should_still_extract_json_ld_from_script_contents() {
    let html = r#"<html><head>
        <script type="application/ld+json">{"@type":"Article","name":"Ada"}</script>
        </head><body><a href="/real">real</a></body></html>"#;
    let (base, result) = scrape_html(html).await;

    assert_eq!(
        result.json_ld.len(),
        1,
        "one JSON-LD entry expected, got {:?}",
        result.json_ld
    );
    assert_eq!(
        result.json_ld[0].schema_type, "Article",
        "JSON-LD @type should survive raw-text handling, got {:?}",
        result.json_ld[0]
    );
    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "the real link should still be found"
    );
}

#[tokio::test]
async fn should_keep_links_when_a_script_sits_inside_an_html_comment() {
    // ~keep The `>` before the `<script>` matters: it makes the fixture fail unless comments
    // ~keep are skipped to `-->`, instead of merely to the first `>`.
    let html = r#"<html><body>
        <!-- a > b <script> --><a href="/real">real</a>
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "a `<script>` inside a comment must not start a raw-text region"
    );
}

/// A crawl must not follow an address that only exists as raw text, because the crawl path
/// masks raw text with its own call before it parses the page. Scrape has a separate call, so
/// only a crawl covers this one.
#[tokio::test]
async fn should_not_follow_a_link_that_only_appears_inside_raw_text_when_crawling() {
    let mock = MockServer::start().await;
    for route in ["/real", "/from-script", "/from-title"] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("<html><body>leaf {route}</body></html>"))
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><head><title>Root <a href="/from-title">t</a></title></head><body>
                    <script>document.write('<a href="/from-script">s</a>');</script>
                    <a href="/real">real</a>
                    </body></html>"#
                        .to_owned(),
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let mut config = CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(10),
        // ~keep One fetch in flight so `pages` is filled in a deterministic order.
        max_concurrent: Some(1),
        ..allow_private_config()
    };
    config.browser.mode = BrowserMode::Never;
    let handle = create_engine(Some(config)).expect("engine should build");
    let base = mock.uri();
    let result = crawl(&handle, &base).await.expect("crawl should succeed");

    let paths: Vec<String> = result
        .pages
        .iter()
        .map(|page| match page.url.strip_prefix(&base).unwrap_or(&page.url) {
            "" => "/".to_owned(),
            rest => rest.to_owned(),
        })
        .collect();
    assert_eq!(
        paths,
        vec!["/".to_owned(), "/real".to_owned()],
        "the crawl frontier must take only the links a browser sees on the root page"
    );
}

/// A crawl runs page extraction through a different call site than a scrape does (the loop's
/// blocking extraction, not `scrape_from_crawl_response`), so raw-text safety must hold there
/// too, for data a browser never treats as markup: an image discovered inside script text.
#[tokio::test]
async fn should_not_extract_images_that_only_appear_inside_raw_text_when_crawling() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body>
                    <script>var t = '<img src="/from-script.png">';</script>
                    <img src="/real.png" alt="real">
                    </body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let mut config = CrawlConfig {
        max_depth: Some(0),
        max_pages: Some(1),
        ..allow_private_config()
    };
    config.browser.mode = BrowserMode::Never;
    let handle = create_engine(Some(config)).expect("engine should build");
    let base = mock.uri();
    let result = crawl(&handle, &base).await.expect("crawl should succeed");

    let urls: Vec<&str> = result.pages[0].images.iter().map(|i| i.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real.png")],
        "an image inside script text must be ignored during a crawl, not only a scrape"
    );
}

/// The URLs of the links `scrape()` extracts from `html`, and the address the page was served at.
async fn link_urls(html: &str) -> (String, Vec<String>) {
    let (base, result) = scrape_html(html).await;
    (base, result.links.into_iter().map(|l| l.url).collect())
}

#[tokio::test]
async fn should_find_links_after_a_comment_opener_inside_xmp_iframe_noembed_noframes_text() {
    for element in ["xmp", "iframe", "noembed", "noframes"] {
        let html = format!(r#"<html><body><{element}><!--</{element}><a href="/real">real</a></body></html>"#);
        let (base, urls) = link_urls(&html).await;
        assert_eq!(
            urls,
            vec![format!("{base}/real")],
            "a `<!--` inside {element} text must not hide the anchor that follows it"
        );
    }
}

#[tokio::test]
async fn should_not_extract_links_from_xmp_iframe_noembed_noframes_plaintext_text() {
    let html = r#"<html><body>
        <a href="/real">real</a>
        <xmp><a href="/from-xmp">x</a></xmp>
        <iframe><a href="/from-iframe">x</a></iframe>
        <noembed><a href="/from-noembed">x</a></noembed>
        <noframes><a href="/from-noframes">x</a></noframes>
        <plaintext><a href="/from-plaintext">x</a>
    </body></html>"#;
    let (base, urls) = link_urls(html).await;
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "links inside xmp/iframe/noembed/noframes/plaintext text must be ignored, and the real link kept"
    );
}

#[tokio::test]
async fn should_ignore_a_base_href_inside_xmp_iframe_self_closed_script_and_foreign_object_script() {
    let base_tag = r#"<base href="https://hijacked.example/">"#;
    for wrapped in [
        format!("<xmp>{base_tag}</xmp>"),
        format!("<iframe>{base_tag}</iframe>"),
        format!("<script/>{base_tag}</script>"),
        format!("<svg><foreignObject><script>{base_tag}</script></foreignObject></svg>"),
    ] {
        let html = format!(r#"<html><head>{wrapped}</head><body><a href="/page">page</a></body></html>"#);
        let (base, urls) = link_urls(&html).await;
        assert_eq!(
            urls,
            vec![format!("{base}/page")],
            "a `<base href>` in raw text must not change the document base: {wrapped}"
        );
    }
}

#[tokio::test]
async fn should_read_the_whole_base_href_value_after_a_quote_in_an_unquoted_value() {
    let html = r#"<html><head><base b=x'y href="/d'><title>t<i</title>/"></head>
        <body><a href="leaf">leaf</a></body></html>"#;
    let (base, urls) = link_urls(html).await;
    let expected = url::Url::parse(&base)
        .and_then(|page| page.join("/d'><title>t<i</title>/"))
        .and_then(|document_base| document_base.join("leaf"))
        .expect("valid URLs");
    assert_eq!(
        urls,
        vec![expected.to_string()],
        "the base is the whole `href` value an HTML parser reads"
    );
}

#[tokio::test]
async fn should_ignore_a_base_href_inside_template_contents() {
    let html = r#"<html><head><template><base href="https://hijacked.example/"></template></head>
        <body><a href="/page">page</a></body></html>"#;
    let (base, urls) = link_urls(html).await;
    assert_eq!(
        urls,
        vec![format!("{base}/page")],
        "a `<base href>` inside template contents is not part of the document"
    );
}

#[tokio::test]
async fn should_honour_a_base_href_inside_noscript() {
    // ~keep A crawler runs no script, and a browser without scripting reads `<noscript>` content
    // ~keep as markup, so its `<base href>` counts.
    let html = r#"<html><head><noscript><base href="/ns/"></noscript></head>
        <body><a href="page">page</a></body></html>"#;
    let (base, urls) = link_urls(html).await;
    assert_eq!(
        urls,
        vec![format!("{base}/ns/page")],
        "link extraction reads `<noscript>` as markup"
    );
}

#[tokio::test]
async fn should_take_the_base_href_in_front_of_a_table_before_one_inside_it() {
    // ~keep A browser moves the second `<base>` in front of the table, so it comes first in the
    // ~keep document even though the parser reads it second.
    let html = r#"<html><body><table><tr><td><base href="/1/"></td><base href="/2/"></tr></table>
        <a href="leaf">leaf</a><img src="pic.png"></body></html>"#;
    let (base, result) = scrape_html(html).await;
    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/2/leaf")],
        "link extraction takes the first base in tree order"
    );
    let images: Vec<&str> = result.images.iter().map(|i| i.url.as_str()).collect();
    assert_eq!(
        images,
        vec![format!("{base}/2/pic.png")],
        "image extraction resolves against the same base"
    );
}

#[tokio::test]
async fn should_ignore_a_base_href_in_a_body_that_a_frameset_replaces() {
    let html = r#"<div><base href="/1/"></div><frameset></frameset><a href="leaf">leaf</a>"#;
    let (base, urls) = link_urls(html).await;
    assert_eq!(
        urls,
        vec![format!("{base}/leaf")],
        "the frameset removes the body and its base"
    );
}

#[tokio::test]
async fn should_not_extract_links_inside_a_bogus_comment() {
    // ~keep An HTML parser reads each of these as a comment that runs to the first `>`, so the
    // ~keep `<a` inside it is comment text.
    let mut wrong = Vec::new();
    for opener in ["<?", "<!x", "</3", "<![CDATA["] {
        let html = format!(r#"<html><body>{opener} <a href="/x">]]> <a href="/real">real</a></body></html>"#);
        let (base, urls) = link_urls(&html).await;
        if urls != [format!("{base}/real")] {
            wrong.push((opener, urls));
        }
    }
    assert!(
        wrong.is_empty(),
        "a link inside a bogus comment must be ignored, and the real link kept: {wrong:?}"
    );
}

#[tokio::test]
async fn should_not_extract_links_inside_an_svg_cdata_section() {
    let html = r#"<html><body><svg><![CDATA[<a href="/x">x</a>]]></svg><a href="/real">real</a></body></html>"#;
    let (base, urls) = link_urls(html).await;
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "a CDATA section in SVG is text, so the link written in it must be ignored"
    );
}

#[tokio::test]
async fn should_not_extract_links_from_script_text_after_a_nul() {
    // ~keep The NUL is a parse error inside script text, which must not end the text there.
    let html = "<html><body><script>a\0<a href=\"/x\">x</a></script><a href=\"/real\">real</a></body></html>";
    let (base, urls) = link_urls(html).await;
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "a link in script text after a NUL must be ignored, and the real link kept"
    );
}
