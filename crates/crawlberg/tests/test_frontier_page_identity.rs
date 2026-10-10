//! Which two addresses the crawl frontier counts as one page (#607, #615, #616).
//!
//! Two addresses a server can answer differently are two pages. Two addresses are one page only
//! where the URL standard or RFC 3986 section 6.2.2 says they are equivalent.

use std::collections::HashMap;

use crawlberg::{CrawlConfig, CrawlResult, LinkType, crawl, create_engine, map_urls, scrape};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// What the fixture server answers at one exact path.
enum Answer {
    /// The body of a page; the server writes the document around it.
    Html(&'static str),
    /// A whole document, sent as written.
    Document(&'static str),
    /// A 302 with this `Location`, sent as written.
    RedirectTo(&'static str),
    /// A 302 to this path on the server's own port, with the host written as given.
    RedirectToOrigin(&'static str, &'static str),
}

/// The path of `request` as the client wrote it, with `?query` when it has one.
fn path_and_query(request: &Request) -> String {
    match request.url.query() {
        Some(query) => format!("{}?{query}", request.url.path()),
        None => request.url.path().to_owned(),
    }
}

/// A server that answers each listed path, compared byte for byte with the request path, and
/// answers 404 for every other path.
///
/// ~keep One responder that reads the raw request path, instead of one path matcher per page: a
/// ~keep matcher that normalises the path would hide the difference these tests are about.
async fn site(pages: &[(&'static str, Answer)]) -> MockServer {
    let mock = MockServer::start().await;
    let pages: HashMap<&'static str, ResponseTemplate> = pages
        .iter()
        .map(|(at, answer)| {
            let response = match answer {
                Answer::Html(body) => ResponseTemplate::new(200)
                    .set_body_string(format!(
                        "<!doctype html><html><head><title>t</title></head><body>{body}</body></html>"
                    ))
                    .append_header("content-type", "text/html; charset=utf-8"),
                Answer::Document(document) => ResponseTemplate::new(200)
                    .set_body_string(*document)
                    .append_header("content-type", "text/html; charset=utf-8"),
                Answer::RedirectTo(target) => ResponseTemplate::new(302).append_header("location", *target),
                Answer::RedirectToOrigin(host, path) => ResponseTemplate::new(302)
                    .append_header("location", format!("http://{host}:{}{path}", mock.address().port())),
            };
            (*at, response)
        })
        .collect();
    Mock::given(method("GET"))
        .respond_with(move |request: &Request| {
            pages
                .get(path_and_query(request).as_str())
                .cloned()
                .unwrap_or_else(|| ResponseTemplate::new(404))
        })
        .mount(&mock)
        .await;
    mock
}

/// A server that answers `document` at `at`, written as given (the test supplies the `head`).
async fn site_with_document(
    at: &'static str,
    document: &'static str,
    others: &[(&'static str, &'static str)],
) -> MockServer {
    let mut pages = vec![(at, Answer::Document(document))];
    pages.extend(others.iter().map(|(at, body)| (*at, Answer::Document(*body))));
    site(&pages).await
}

/// Every page path the server was asked for, as the request wrote it, in arrival order.
async fn requested_paths(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .map(path_and_query)
        .filter(|path| path != "/robots.txt")
        .collect()
}

/// The reported address of each page, without the origin `origin`, sorted.
fn page_paths(origin: &str, result: &CrawlResult) -> Vec<String> {
    sorted(result.pages.iter().map(|page| page.url.replace(origin, "")).collect())
}

/// Crawl `seed` to `depth` with the settings of [`config`].
async fn crawl_to_depth(seed: &str, depth: usize) -> CrawlResult {
    let config = CrawlConfig {
        max_depth: Some(depth),
        ..config()
    };
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, seed).await.expect("crawl runs")
}

fn sorted(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths
}

fn config() -> CrawlConfig {
    CrawlConfig::builder()
        .allow_private_networks(true)
        .respect_robots_txt(false)
        .stay_on_domain(true)
        .max_depth(1)
        .max_pages(20)
        .build()
}

async fn crawl_from(mock: &MockServer, seed_path: &str) -> CrawlResult {
    let engine = create_engine(Some(config())).expect("engine builds");
    crawl(&engine, &format!("{}{seed_path}", mock.uri()))
        .await
        .expect("crawl runs")
}

/// The `normalized_url` of each page, without the server's origin, sorted.
fn normalized_paths(mock: &MockServer, result: &CrawlResult) -> Vec<String> {
    sorted(
        result
            .pages
            .iter()
            .map(|page| page.normalized_url.replace(&mock.uri(), ""))
            .collect(),
    )
}

/// The page whose reported address ends with `path`.
fn html_of<'a>(result: &'a CrawlResult, mock: &MockServer, path: &str) -> &'a str {
    let address = format!("{}{path}", mock.uri());
    result
        .pages
        .iter()
        .find(|page| page.url == address)
        .map(|page| page.html.as_str())
        .unwrap_or_else(|| {
            panic!(
                "no page was reported at {path}; pages: {:?}",
                result.pages.iter().map(|page| &page.url).collect::<Vec<_>>()
            )
        })
}

// ---------------------------------------------------------------------------------------
// #607: a trailing slash names another resource
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn two_addresses_that_differ_by_a_trailing_slash_are_two_pages() {
    let mock = site(&[
        (
            "/docs/",
            Answer::Html(r#"<h1>Index</h1><p><a href="guide">guide</a></p><p><a href="guide/">guide folder</a></p>"#),
        ),
        ("/docs/guide", Answer::Html("<p>the page WITHOUT the slash</p>")),
        ("/docs/guide/", Answer::Html("<p>the page WITH the slash</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/docs/").await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/docs/", "/docs/guide", "/docs/guide/"],
        "each of the two spellings must be requested once"
    );
    assert!(
        html_of(&result, &mock, "/docs/guide").contains("the page WITHOUT the slash"),
        "the page without the slash must carry its own text"
    );
    assert!(
        html_of(&result, &mock, "/docs/guide/").contains("the page WITH the slash"),
        "the page with the slash must carry its own text"
    );
    assert_eq!(
        normalized_paths(&mock, &result),
        ["/docs/", "/docs/guide", "/docs/guide/"],
        "normalized_url must keep the trailing slash that the address has"
    );
}

/// The seed's own key keeps its slash too: a link from `/docs/` to `/docs` is another page.
#[tokio::test]
async fn a_link_to_the_seed_without_its_trailing_slash_is_another_page() {
    let mock = site(&[
        (
            "/docs/",
            Answer::Html(r#"<a href="/docs">file</a><a href="/docs/">self</a>"#),
        ),
        ("/docs", Answer::Html("<p>the file named docs</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/docs/").await;

    assert_eq!(
        requested_paths(&mock).await,
        ["/docs/", "/docs"],
        "the seed is requested once and its twin without the slash once"
    );
    assert!(html_of(&result, &mock, "/docs").contains("the file named docs"));
}

/// A redirect hop is claimed with the same key: a hop to `/guide/` is not the page `/guide`.
#[tokio::test]
async fn a_redirect_to_the_trailing_slash_twin_of_a_seen_page_is_followed() {
    let mock = site(&[
        (
            "/",
            Answer::Html(r#"<a href="/guide">guide</a><a href="/moved">moved</a>"#),
        ),
        ("/guide", Answer::Html("<p>the file named guide</p>")),
        ("/moved", Answer::RedirectTo("/guide/")),
        ("/guide/", Answer::Html("<p>the folder named guide</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/").await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/", "/guide", "/guide/", "/moved"],
        "the redirect target with the slash must be requested"
    );
    assert!(
        result
            .pages
            .iter()
            .any(|page| page.html.contains("the folder named guide")),
        "the page behind the redirect must be reported, got: {:?}",
        result.pages.iter().map(|page| &page.url).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------------------
// #615: RFC 3986 section 6.2.2, the two normalisations that are always safe
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn three_percent_encoded_spellings_of_one_address_are_one_page() {
    let mock = site(&[
        (
            "/",
            Answer::Html(
                r#"<h1>Index</h1><p><a href="/a-b">plain</a></p><p><a href="/a%2db">encoded hyphen</a></p><p><a href="/a%2Db">encoded hyphen, upper case</a></p>"#,
            ),
        ),
        ("/a-b", Answer::Html("<p>the same page</p>")),
        ("/a%2db", Answer::Html("<p>the same page</p>")),
        ("/a%2Db", Answer::Html("<p>the same page</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/").await;

    assert_eq!(
        requested_paths(&mock).await,
        ["/", "/a-b"],
        "one request for the page, with the spelling of the first link"
    );
    assert_eq!(
        normalized_paths(&mock, &result),
        ["/", "/a-b"],
        "the page is reported once"
    );
}

/// The request keeps the spelling the first link wrote; `normalized_url` is the one RFC form.
#[tokio::test]
async fn an_encoded_first_spelling_is_requested_as_written_and_reported_in_one_form() {
    let mock = site(&[
        (
            "/",
            Answer::Html(r#"<a href="/a%2db/%7euser">first</a><a href="/a-b/~user">second</a><a href="/a%2Db/%7Euser">third</a>"#),
        ),
        ("/a%2db/%7euser", Answer::Html("<p>the same page</p>")),
        ("/a-b/~user", Answer::Html("<p>the same page</p>")),
        ("/a%2Db/%7Euser", Answer::Html("<p>the same page</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/").await;

    assert_eq!(requested_paths(&mock).await, ["/", "/a%2db/%7euser"]);
    assert_eq!(normalized_paths(&mock, &result), ["/", "/a-b/~user"]);
}

/// The seed's key has the same form: a link to another spelling of the seed is the seed.
#[tokio::test]
async fn a_link_to_another_spelling_of_the_seed_is_not_requested() {
    let mock = site(&[
        (
            "/s%2Dt",
            Answer::Html(r#"<a href="/s-t">plain</a><a href="/next">next</a>"#),
        ),
        ("/s-t", Answer::Html("<p>the seed again</p>")),
        ("/next", Answer::Html("<p>next</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/s%2Dt").await;

    assert_eq!(requested_paths(&mock).await, ["/s%2Dt", "/next"]);
    assert_eq!(normalized_paths(&mock, &result), ["/next", "/s-t"]);
}

/// A redirect hop to another spelling of a seen page is the seen page.
#[tokio::test]
async fn a_redirect_to_another_spelling_of_a_seen_page_is_not_requested_again() {
    let mock = site(&[
        (
            "/",
            Answer::Html(r#"<a href="/a-b">page</a><a href="/moved">moved</a><a href="/last">last</a>"#),
        ),
        ("/a-b", Answer::Html("<p>the page</p>")),
        ("/moved", Answer::RedirectTo("/a%2Db")),
        ("/a%2Db", Answer::Html("<p>the page</p>")),
        ("/last", Answer::Html("<p>the last page</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/").await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/", "/a-b", "/last", "/moved"],
        "the redirect target is one spelling of a page the crawl already has"
    );
    assert!(
        result.pages.iter().any(|page| page.html.contains("the last page")),
        "the crawl must go on after the refused hop"
    );
}

/// Escapes that RFC 3986 does not call equivalent, the case of the path, and an escaped
/// backslash each name another resource. Only the case of the two hex digits is free.
#[tokio::test]
async fn addresses_that_no_standard_calls_equivalent_stay_separate_pages() {
    let mock = site(&[
        (
            "/",
            Answer::Html(concat!(
                r#"<a href="/a%2Fb">escaped slash</a><a href="/a/b">slash</a>"#,
                r#"<a href="/Page">upper</a><a href="/page">lower</a>"#,
                r#"<a href="/q%5Cr">escaped backslash</a><a href="/q/r">slash</a>"#,
                r#"<a href="/s%3fx">escaped question mark, lower hex</a><a href="/s%3Fx">upper hex</a>"#,
            )),
        ),
        ("/a%2Fb", Answer::Html("<p>one segment</p>")),
        ("/a/b", Answer::Html("<p>two segments</p>")),
        ("/Page", Answer::Html("<p>upper</p>")),
        ("/page", Answer::Html("<p>lower</p>")),
        ("/q%5Cr", Answer::Html("<p>escaped backslash</p>")),
        ("/q/r", Answer::Html("<p>slash</p>")),
        ("/s%3fx", Answer::Html("<p>escaped question mark</p>")),
        ("/s%3Fx", Answer::Html("<p>escaped question mark</p>")),
    ])
    .await;

    crawl_from(&mock, "/").await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/", "/Page", "/a%2Fb", "/a/b", "/page", "/q%5Cr", "/q/r", "/s%3fx"],
        "only the two spellings that differ in the case of the hex digits are one page"
    );
}

// ---------------------------------------------------------------------------------------
// #616: "same page" is decided on the resolved address
// ---------------------------------------------------------------------------------------

const START_WITH_BASE: &str = concat!(
    r#"<!doctype html><html><head><title>t</title><base href="/other/"></head><body><h1>Start</h1>"#,
    r##"<p><a href="#part">fragment link</a></p><p><a href="leaf">leaf link</a></p></body></html>"##,
);
const OTHER: &str = "<!doctype html><html><head><title>t</title></head><body><p>the page at /other/</p></body></html>";
const LEAF: &str =
    "<!doctype html><html><head><title>t</title></head><body><p>the page at /other/leaf</p></body></html>";

#[tokio::test]
async fn a_fragment_link_on_a_page_with_a_base_address_is_followed() {
    let mock = site_with_document("/start/", START_WITH_BASE, &[("/other/", OTHER), ("/other/leaf", LEAF)]).await;

    let result = crawl_from(&mock, "/start/").await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/other/", "/other/leaf", "/start/"],
        "the fragment link points at the base address, which is another page"
    );
    assert!(html_of(&result, &mock, "/other/").contains("the page at /other/"));
}

/// A link that resolves to the page's own address with a fragment is a link inside the page,
/// however the link wrote it.
#[tokio::test]
async fn a_link_to_the_page_itself_with_a_fragment_is_not_another_page() {
    let mock = site(&[
        (
            "/dir/page",
            Answer::Html(r##"<a href="#a">fragment</a><a href="page#b">relative</a><a href="/dir/page#c">path</a><a href="next">next</a>"##),
        ),
        ("/dir/next", Answer::Html("<p>next</p>")),
    ])
    .await;

    let result = crawl_from(&mock, "/dir/page").await;

    assert_eq!(requested_paths(&mock).await, ["/dir/page", "/dir/next"]);
    let seed = &result.pages[0];
    let types: Vec<&LinkType> = seed.links.iter().map(|link| &link.link_type).collect();
    assert_eq!(
        types,
        [
            &LinkType::Anchor,
            &LinkType::Anchor,
            &LinkType::Anchor,
            &LinkType::Internal
        ],
        "each of the three links to the page itself is an anchor, got: {:?}",
        seed.links
    );
}

/// `link_type` of a scraped page is decided against the page address, not the base address.
#[tokio::test]
async fn link_types_of_a_page_with_a_base_address_are_decided_against_the_page_address() {
    let mock = site_with_document("/start/", START_WITH_BASE, &[]).await;
    let engine = create_engine(Some(config())).expect("engine builds");

    let page = scrape(&engine, &format!("{}/start/", mock.uri()))
        .await
        .expect("scrape runs");

    let links: Vec<(String, &LinkType)> = page
        .links
        .iter()
        .map(|link| (link.url.replace(&mock.uri(), ""), &link.link_type))
        .collect();
    assert_eq!(
        links,
        [
            ("/other/#part".to_owned(), &LinkType::Internal),
            ("/other/leaf".to_owned(), &LinkType::Internal),
        ]
    );
}

/// A `<base>` on another host does not make that host the page's own: `external` compares the
/// link's host with the page's host.
#[tokio::test]
async fn links_under_a_base_on_another_host_are_external_and_a_link_to_the_page_host_is_internal() {
    const DOCUMENT: &str = concat!(
        r#"<!doctype html><html><head><title>t</title><base href="http://cdn.example/assets/"></head><body>"#,
        r#"<a href="x.html">relative</a><a href="http://127.0.0.1/home">page host</a></body></html>"#,
    );
    let mock = site_with_document("/start/", DOCUMENT, &[]).await;
    let engine = create_engine(Some(config())).expect("engine builds");

    let page = scrape(&engine, &format!("{}/start/", mock.uri()))
        .await
        .expect("scrape runs");

    let links: Vec<(&str, &LinkType)> = page
        .links
        .iter()
        .map(|link| (link.url.as_str(), &link.link_type))
        .collect();
    assert_eq!(
        links,
        [
            ("http://cdn.example/assets/x.html", &LinkType::External),
            ("http://127.0.0.1/home", &LinkType::Internal),
        ]
    );
}

/// `map` reads the same links: the fragment link names the base address, another page.
#[tokio::test]
async fn map_lists_the_target_of_a_fragment_link_on_a_page_with_a_base_address() {
    let mock = site_with_document("/start/", START_WITH_BASE, &[]).await;
    let engine = create_engine(Some(config())).expect("engine builds");

    let mapped = map_urls(&engine, &format!("{}/start/", mock.uri()))
        .await
        .expect("map runs");

    let urls: Vec<String> = mapped
        .urls
        .iter()
        .map(|entry| entry.url.replace(&mock.uri(), ""))
        .collect();
    assert_eq!(urls, ["/other/", "/other/leaf"]);
}

/// `map` deduplicates with the same key: the two spellings with and without a slash are two
/// entries, and the three spellings of one escape are one entry.
#[tokio::test]
async fn map_keeps_trailing_slash_twins_and_merges_equivalent_escapes() {
    let mock = site(&[(
        "/",
        Answer::Html(concat!(
            r#"<a href="/guide">file</a><a href="/guide/">folder</a>"#,
            r#"<a href="/a-b">plain</a><a href="/a%2db">lower</a><a href="/a%2Db">upper</a>"#,
        )),
    )])
    .await;
    let engine = create_engine(Some(config())).expect("engine builds");

    let mapped = map_urls(&engine, &format!("{}/", mock.uri())).await.expect("map runs");

    let urls: Vec<String> = mapped
        .urls
        .iter()
        .map(|entry| entry.url.replace(&mock.uri(), ""))
        .collect();
    assert_eq!(urls, ["/guide", "/guide/", "/a-b"]);
}

// ---------------------------------------------------------------------------------------
// The kept query (`dedup_include_query`): sorted by parameter name, otherwise as written
// ---------------------------------------------------------------------------------------

/// With the query kept, two addresses are one page only by the documented sort of parameter
/// names. The order of the values of one name, a missing `=`, and `+` against `%20` each make
/// another address, and the crawl requests it.
#[tokio::test]
async fn a_kept_query_merges_by_parameter_name_order_and_by_nothing_else() {
    const PAGE: Answer = Answer::Html("<p>page</p>");
    let mock = site(&[
        (
            "/",
            Answer::Html(concat!(
                r#"<a href="/p?a=1&amp;a=2">1</a><a href="/p?a=2&amp;a=1">2</a>"#,
                r#"<a href="/p?x">3</a><a href="/p?x=">4</a>"#,
                r#"<a href="/p?q=a+b">5</a><a href="/p?q=a%20b">6</a>"#,
                r#"<a href="/p?m=1&amp;n=2">7</a><a href="/p?n=2&amp;m=1">the same as 7</a>"#,
                r#"<a href="/p?e=%7e">8</a><a href="/p?e=~">the same as 8</a>"#,
            )),
        ),
        ("/p?a=1&a=2", PAGE),
        ("/p?a=2&a=1", PAGE),
        ("/p?x", PAGE),
        ("/p?x=", PAGE),
        ("/p?q=a+b", PAGE),
        ("/p?q=a%20b", PAGE),
        ("/p?m=1&n=2", PAGE),
        ("/p?n=2&m=1", PAGE),
        ("/p?e=%7e", PAGE),
        ("/p?e=~", PAGE),
    ])
    .await;
    let config = CrawlConfig {
        dedup_include_query: true,
        ..config()
    };
    let engine = create_engine(Some(config)).expect("engine builds");

    let result = crawl(&engine, &format!("{}/", mock.uri())).await.expect("crawl runs");

    assert_eq!(
        sorted(requested_paths(&mock).await),
        [
            "/",
            "/p?a=1&a=2",
            "/p?a=2&a=1",
            "/p?e=%7e",
            "/p?m=1&n=2",
            "/p?q=a%20b",
            "/p?q=a+b",
            "/p?x",
            "/p?x="
        ],
        "eight pages behind the index: only the two documented merges are one request"
    );
    assert_eq!(
        normalized_paths(&mock, &result),
        [
            "/",
            "/p?a=1&a=2",
            "/p?a=2&a=1",
            "/p?e=~",
            "/p?m=1&n=2",
            "/p?q=a%20b",
            "/p?q=a+b",
            "/p?x",
            "/p?x="
        ],
        "normalized_url keeps each query as it is written, sorted by name, with RFC 3986 escapes"
    );
}

// ---------------------------------------------------------------------------------------
// #629: a redirect of a linked page to another address of the same page
// ---------------------------------------------------------------------------------------

/// The final address of each page, without the server's origin, sorted.
fn final_paths(mock: &MockServer, result: &CrawlResult) -> Vec<String> {
    sorted(
        result
            .pages
            .iter()
            .map(|page| page.final_url.replace(&mock.uri(), ""))
            .collect(),
    )
}

/// The site of #629: a linked folder without its slash answers a redirect to the slash form.
#[tokio::test]
async fn a_linked_folder_that_redirects_to_its_slash_form_is_fetched_with_the_page_behind_it() {
    let mock = site(&[
        (
            "/",
            Answer::Html(r#"<h1>Home</h1><p><a href="/docs">the docs</a></p><p><a href="/moved">a moved page</a></p>"#),
        ),
        ("/docs", Answer::RedirectTo("/docs/")),
        (
            "/docs/",
            Answer::Html(r#"<h1>Docs index</h1><p><a href="guide">the guide</a></p>"#),
        ),
        ("/docs/guide", Answer::Html("<h1>Guide</h1>")),
        ("/moved", Answer::RedirectTo("/new-place")),
        ("/new-place", Answer::Html("<h1>New place</h1>")),
    ])
    .await;

    let result = crawl_to_depth(&format!("{}/", mock.uri()), 2).await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/", "/docs", "/docs/", "/docs/guide", "/moved", "/new-place"],
        "the redirect to the slash form is followed, and the link on the folder page too"
    );
    assert_eq!(
        final_paths(&mock, &result),
        ["/", "/docs/", "/docs/guide", "/new-place"],
        "four pages: the folder page and the page behind it are reported"
    );
    assert_eq!(
        page_paths(&mock.uri(), &result),
        ["/", "/docs", "/docs/guide", "/moved"]
    );
    assert!(html_of(&result, &mock, "/docs").contains("Docs index"));
    assert!(html_of(&result, &mock, "/docs/guide").contains("Guide"));
}

/// A chain through both forms of the folder to its index file, with a link behind it.
#[tokio::test]
async fn a_redirect_chain_through_the_slash_form_to_an_index_file_is_followed() {
    let mock = site(&[
        ("/", Answer::Html(r#"<a href="/docs">the docs</a>"#)),
        ("/docs", Answer::RedirectTo("/docs/")),
        ("/docs/", Answer::RedirectTo("/docs/index.html")),
        (
            "/docs/index.html",
            Answer::Html(r#"<h1>Docs index</h1><a href="guide">the guide</a>"#),
        ),
        ("/docs/guide", Answer::Html("<h1>Guide</h1>")),
    ])
    .await;

    let result = crawl_to_depth(&format!("{}/", mock.uri()), 2).await;

    assert_eq!(
        requested_paths(&mock).await,
        ["/", "/docs", "/docs/", "/docs/index.html", "/docs/guide"]
    );
    assert_eq!(final_paths(&mock, &result), ["/", "/docs/guide", "/docs/index.html"]);
}

/// A page the crawl reaches only through a redirect is marked seen by the chain that claims it,
/// so a later link to it is not requested again.
#[tokio::test]
async fn a_page_reached_only_through_a_redirect_is_not_requested_again_from_a_later_link() {
    let mock = site(&[
        ("/", Answer::Html(r#"<a href="/go">go</a>"#)),
        ("/go", Answer::RedirectTo("/landed")),
        (
            "/landed",
            Answer::Html(r#"<h1>Landed</h1><a href="/behind">behind</a>"#),
        ),
        ("/behind", Answer::Html(r#"<h1>Behind</h1><a href="/landed">back</a>"#)),
    ])
    .await;

    crawl_to_depth(&format!("{}/", mock.uri()), 3).await;

    assert_eq!(requested_paths(&mock).await, ["/", "/go", "/landed", "/behind"]);
}

/// A redirect to another spelling of the requested address is the same page under RFC 3986,
/// so the frontier already has its key. The key is there because of this link, not because of
/// another page, and the redirect is followed.
#[tokio::test]
async fn a_redirect_to_another_spelling_of_the_requested_address_is_followed() {
    let mock = site(&[
        ("/", Answer::Html(r#"<a href="/a-b">page</a><a href="/n">twice</a>"#)),
        ("/a-b", Answer::RedirectTo("/a%2Db")),
        (
            "/a%2Db",
            Answer::Html(r#"<h1>The page</h1><a href="/behind">behind</a>"#),
        ),
        ("/behind", Answer::Html("<h1>Behind</h1>")),
        ("/n", Answer::RedirectTo("/m%2Dz")),
        ("/m%2Dz", Answer::RedirectTo("/m-z")),
        ("/m-z", Answer::Html("<h1>Canonical twice</h1>")),
    ])
    .await;

    let result = crawl_to_depth(&format!("{}/", mock.uri()), 2).await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/", "/a%2Db", "/a-b", "/behind", "/m%2Dz", "/m-z", "/n"],
        "each hop is requested once, and the link behind the first page too"
    );
    assert!(html_of(&result, &mock, "/a-b").contains("The page"));
    assert!(
        html_of(&result, &mock, "/n").contains("Canonical twice"),
        "a hop the chain claimed is the chain's own page when the next hop has the same key"
    );
}

/// The default key drops the query, so a redirect that adds a query has the key of the link.
#[tokio::test]
async fn a_redirect_that_adds_a_query_to_the_requested_address_is_followed() {
    let mock = site(&[
        ("/", Answer::Html(r#"<a href="/q">page</a>"#)),
        ("/q", Answer::RedirectTo("/q?step=2")),
        ("/q?step=2", Answer::Html("<h1>Step two</h1>")),
    ])
    .await;

    let result = crawl_to_depth(&format!("{}/", mock.uri()), 1).await;

    assert_eq!(requested_paths(&mock).await, ["/", "/q", "/q?step=2"]);
    assert!(html_of(&result, &mock, "/q").contains("Step two"));
}

/// `/p?id=1` and `/p?id=2` differ only in the query, and the first redirects to the second.
/// `/a` and `/b` redirect to two queries of `/q`.
async fn site_with_redirects_between_queries() -> MockServer {
    site(&[
        (
            "/",
            Answer::Html(concat!(
                r#"<a href="/p?id=1">one</a><a href="/p?id=2">two</a>"#,
                r#"<a href="/a">a</a><a href="/b">b</a>"#,
            )),
        ),
        ("/p?id=1", Answer::RedirectTo("/p?id=2")),
        ("/p?id=2", Answer::Html("<h1>Two</h1>")),
        ("/a", Answer::RedirectTo("/q?id=1")),
        ("/b", Answer::RedirectTo("/q?id=2")),
        ("/q?id=1", Answer::Html("<h1>First</h1>")),
        ("/q?id=2", Answer::Html("<h1>Second</h1>")),
    ])
    .await
}

async fn crawl_with_dedup_include_query(mock: &MockServer, dedup_include_query: bool) -> CrawlResult {
    let config = CrawlConfig {
        dedup_include_query,
        ..config()
    };
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, &format!("{}/", mock.uri())).await.expect("crawl runs")
}

/// Each path without its `?query`, sorted.
fn without_queries(paths: &[String]) -> Vec<String> {
    sorted(
        paths
            .iter()
            .map(|path| path.split('?').next().unwrap_or_default().to_owned())
            .collect(),
    )
}

/// With the query kept, a redirect target is claimed on the key that link discovery uses, the
/// one with the query. A redirect onto a page another link holds is refused, and two redirects
/// onto two queries of one path are two pages.
#[tokio::test]
async fn with_the_query_kept_a_redirect_is_claimed_on_the_key_with_the_query() {
    let mock = site_with_redirects_between_queries().await;

    let result = crawl_with_dedup_include_query(&mock, true).await;

    assert_eq!(
        sorted(requested_paths(&mock).await),
        ["/", "/a", "/b", "/p?id=1", "/p?id=2", "/q?id=1", "/q?id=2"],
        "the page of the second link is requested once, and both queries of /q are requested"
    );
    assert_eq!(
        final_paths(&mock, &result),
        ["/", "/p?id=2", "/q?id=1", "/q?id=2"],
        "the page another link holds is reported once, and no page behind a redirect is lost"
    );
}

/// With the default key the same two links are one page, and the redirect between them is the
/// page under another address. The two queries of `/q` are one page too.
#[tokio::test]
async fn with_the_query_dropped_a_redirect_is_claimed_on_the_key_without_the_query() {
    let mock = site_with_redirects_between_queries().await;

    let result = crawl_with_dedup_include_query(&mock, false).await;

    assert_eq!(
        without_queries(&requested_paths(&mock).await),
        ["/", "/a", "/b", "/p", "/p", "/q"],
        "one of the two links to /p is requested and its redirect is followed; /q is requested once"
    );
    let reported = final_paths(&mock, &result);
    assert!(
        reported.contains(&"/p?id=2".to_owned()),
        "the redirect to another query of the link is followed, got: {reported:?}"
    );
    assert_eq!(without_queries(&reported), ["/", "/p", "/q"]);
}

/// The host of a redirect target is compared as the URL standard writes it, in lower case.
#[tokio::test]
async fn a_redirect_to_the_slash_form_with_the_host_in_upper_case_is_followed() {
    let mock = site(&[
        ("/", Answer::Html(r#"<a href="/docs">the docs</a>"#)),
        ("/docs", Answer::RedirectToOrigin("LOCALHOST", "/docs/")),
        (
            "/docs/",
            Answer::Html(r#"<h1>Docs index</h1><a href="guide">the guide</a>"#),
        ),
        ("/docs/guide", Answer::Html("<h1>Guide</h1>")),
    ])
    .await;
    let origin = format!("http://localhost:{}", mock.address().port());

    let result = crawl_to_depth(&format!("{origin}/"), 2).await;

    assert_eq!(requested_paths(&mock).await, ["/", "/docs", "/docs/", "/docs/guide"]);
    assert_eq!(page_paths(&origin, &result), ["/", "/docs", "/docs/guide"]);
}

/// A server that redirects each form of an address to the other makes a loop. The crawl takes
/// each address once, stops, and goes on with the next link.
#[tokio::test]
async fn a_redirect_loop_between_the_two_forms_of_an_address_ends() {
    let mock = site(&[
        (
            "/",
            Answer::Html(r#"<a href="/loop">loop</a><a href="/frag">fragment</a><a href="/last">last</a>"#),
        ),
        ("/loop", Answer::RedirectTo("/loop/")),
        ("/loop/", Answer::RedirectTo("/loop")),
        ("/frag", Answer::RedirectTo("/frag#top")),
        ("/last", Answer::Html("<h1>Last</h1>")),
    ])
    .await;

    let result = crawl_to_depth(&format!("{}/", mock.uri()), 1).await;

    let requested = requested_paths(&mock).await;
    let count = |path: &str| requested.iter().filter(|requested| *requested == path).count();
    assert_eq!(
        (count("/loop"), count("/loop/")),
        (1, 1),
        "each form of the address is requested once, got: {requested:?}"
    );
    assert_eq!(
        count("/frag"),
        2,
        "a redirect to the same address with a fragment is taken once and then ends, got: {requested:?}"
    );
    assert!(html_of(&result, &mock, "/last").contains("Last"), "the crawl goes on");
}
