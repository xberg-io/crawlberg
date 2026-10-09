//! A page is read with the character set a browser uses for it, in HTTP mode and in browser mode.
//!
//! Each case is one page: its `Content-Type` header, its bytes, and the text of its paragraph as
//! Chrome shows it. Every mode reads every case directly and through a redirect.
//!
//! The browser tests need a real Chrome binary; they are skipped (not failed) when Chrome is
//! unavailable, matching the other browser tests.

#![allow(clippy::print_stderr)]

use crawlberg::{BrowserMode, CrawlConfig, CrawlEngineHandle, crawl, create_engine, scrape};
use encoding_rs::{EUC_KR, Encoding, GB18030, SHIFT_JIS, WINDOWS_1252};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LATIN: &str = "café déjà vu naïve über straße, crème brûlée and smörgåsbord for señor Ångström";
const WINDOWS: &str = "“café” costs €5 – déjà vu naïve über straße, crème brûlée…";
const JAPANESE: &str = "日本語のテキストです。これは文字コードの判定を確かめるための文章です。";
const CHINESE: &str = "这是一个中文网页，用来检查字符编码的判定是否正确。";
/// Text for the pages that are UTF-8 and declare nothing. Read as windows-1252 it has no
/// character that an HTML serializer escapes.
const UNDECLARED: &str = "café über straße, crème brûlée and smörgåsbord";
const KOREAN: &str = "한국어로 쓴 웹 페이지입니다. 문자 인코딩 판정을 확인합니다.";

/// One page and what a reader of it must get.
struct Case {
    name: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    /// The text of the paragraph, as Chrome shows it.
    text: String,
    /// `detected_charset` in HTTP mode.
    http_charset: Option<&'static str>,
    /// `detected_charset` in browser mode: the character set Chrome used. `None` when the page
    /// declares none, so the browser's own detection names it.
    browser_charset: Option<&'static str>,
    /// A second text a browser may show: Chrome reads an undeclared UTF-8 page as windows-1252.
    browser_text: Option<String>,
    /// Whether the markdown holds `text` as it is. False for a text with replacement characters
    /// or wrong letters, which the markdown may escape.
    in_markdown: bool,
}

fn encode(encoding: &'static Encoding, text: &str) -> Vec<u8> {
    let (bytes, _, had_errors) = encoding.encode(text);
    assert!(!had_errors, "{text:?} must be representable in {}", encoding.name());
    bytes.into_owned()
}

fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).expect("the text must be Latin-1"))
        .collect()
}

fn utf16(text: &str, little_endian: bool, mark: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut push = |unit: u16| {
        bytes.extend_from_slice(&if little_endian {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        });
    };
    if mark {
        push(0xFEFF);
    }
    text.encode_utf16().for_each(&mut push);
    bytes
}

/// A page with `head` in its head and `text` in its one paragraph, after `prefix`.
fn page(prefix: &str, head: &str, text: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(prefix.as_bytes());
    bytes.extend_from_slice(b"<!doctype html><html><head><title>t</title>");
    bytes.extend_from_slice(head.as_bytes());
    bytes.extend_from_slice(b"</head><body><p>");
    bytes.extend_from_slice(text);
    bytes.extend_from_slice(b"</p></body></html>");
    bytes
}

/// `body`, which must not be UTF-8: a fixture that is UTF-8 by accident reads correctly with no
/// character set decision at all.
fn not_utf8(body: Vec<u8>) -> Vec<u8> {
    assert!(
        std::str::from_utf8(&body).is_err(),
        "the fixture must not be valid UTF-8"
    );
    body
}

fn decoded_as(encoding: &'static Encoding, bytes: &[u8]) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

fn cases() -> Vec<Case> {
    let padding = format!("<!-- {} -->", "x".repeat(1500));
    let latin_page = |text: &str| format!("<!doctype html><html><head><title>t</title></head><body><p>{text}</p>");
    let truncated = {
        let mut body = b"<!doctype html><html><head><title>t</title></head><body><p>".to_vec();
        body.extend_from_slice(&encode(SHIFT_JIS, JAPANESE));
        body.push(0x93);
        body
    };
    let case = |name, content_type, body, text: &str, http_charset, browser_charset| Case {
        name,
        content_type,
        body,
        text: text.to_owned(),
        http_charset,
        browser_charset: Some(browser_charset),
        browser_text: None,
        in_markdown: true,
    };
    let odd = |name, content_type, body, text: String, http_charset, browser_charset| Case {
        name,
        content_type,
        body,
        text,
        http_charset,
        browser_charset: Some(browser_charset),
        browser_text: None,
        in_markdown: false,
    };
    let late_in_body = {
        let mut body = format!(
            "<!doctype html><html><head><title>t</title></head><body><div>{}</div><meta charset=\"utf-8\"><p>",
            "x".repeat(1500)
        )
        .into_bytes();
        body.extend_from_slice(&latin1(LATIN));
        body.extend_from_slice(b"</p></body></html>");
        body
    };
    let mut cases = vec![
        case(
            "no-declaration-latin1",
            "text/html",
            not_utf8(page("", "", &latin1(LATIN))),
            LATIN,
            Some("windows-1252"),
            "windows-1252",
        ),
        case(
            "no-declaration-windows-1252",
            "text/html",
            not_utf8(page("", "", &encode(WINDOWS_1252, WINDOWS))),
            WINDOWS,
            Some("windows-1252"),
            "windows-1252",
        ),
        case(
            "no-declaration-shift-jis",
            "text/html",
            not_utf8(page("", "", &encode(SHIFT_JIS, JAPANESE))),
            JAPANESE,
            Some("shift_jis"),
            "shift_jis",
        ),
        case(
            "no-declaration-gb18030",
            "text/html",
            not_utf8(page("", "", &encode(GB18030, CHINESE))),
            CHINESE,
            Some("gbk"),
            "gbk",
        ),
        case(
            "no-declaration-euc-kr",
            "text/html",
            not_utf8(page("", "", &encode(EUC_KR, KOREAN))),
            KOREAN,
            Some("euc-kr"),
            "euc-kr",
        ),
        case(
            "no-declaration-utf-8",
            "text/html",
            page("", "", UNDECLARED.as_bytes()),
            UNDECLARED,
            None,
            "utf-8",
        ),
        odd(
            "no-declaration-utf-8-with-a-replacement-character",
            "text/html",
            page("", "", "a real \u{FFFD} in the page, café".as_bytes()),
            "a real \u{FFFD} in the page, café".to_owned(),
            None,
            "utf-8",
        ),
        case(
            "utf-16le-with-a-mark",
            "text/html",
            not_utf8(utf16(&latin_page(LATIN), true, true)),
            LATIN,
            Some("utf-16le"),
            "utf-16le",
        ),
        case(
            "utf-16be-with-a-mark",
            "text/html",
            not_utf8(utf16(&latin_page(LATIN), false, true)),
            LATIN,
            Some("utf-16be"),
            "utf-16be",
        ),
        case(
            "mark-against-header",
            "text/html; charset=iso-8859-1",
            page("\u{FEFF}", "", LATIN.as_bytes()),
            LATIN,
            Some("utf-8"),
            "utf-8",
        ),
        case(
            "header-utf-8-against-meta-latin1",
            "text/html; charset=utf-8",
            page("", r#"<meta charset="iso-8859-1">"#, LATIN.as_bytes()),
            LATIN,
            Some("utf-8"),
            "utf-8",
        ),
        case(
            "header-latin1-against-meta-utf-8",
            "text/html; charset=iso-8859-1",
            not_utf8(page("", r#"<meta charset="utf-8">"#, &latin1(LATIN))),
            LATIN,
            Some("iso-8859-1"),
            "windows-1252",
        ),
        case(
            "meta-latin1",
            "text/html",
            not_utf8(page("", r#"<meta charset="iso-8859-1">"#, &latin1(LATIN))),
            LATIN,
            Some("iso-8859-1"),
            "windows-1252",
        ),
        case(
            "meta-http-equiv",
            "text/html",
            not_utf8(page(
                "",
                r#"<meta http-equiv="Content-Type" content="text/html; charset=euc-kr">"#,
                &encode(EUC_KR, KOREAN),
            )),
            KOREAN,
            Some("euc-kr"),
            "euc-kr",
        ),
        case(
            "meta-alias-label",
            "text/html",
            not_utf8(page("", r#"<meta charset="x-sjis">"#, &encode(SHIFT_JIS, JAPANESE))),
            JAPANESE,
            Some("x-sjis"),
            "shift_jis",
        ),
        case(
            "header-alias-label",
            "text/html; charset=latin1",
            not_utf8(page("", "", &latin1(LATIN))),
            LATIN,
            Some("latin1"),
            "windows-1252",
        ),
        case(
            "header-unknown-label",
            "text/html; charset=not-a-charset",
            not_utf8(page("", "", &latin1(LATIN))),
            LATIN,
            Some("windows-1252"),
            "windows-1252",
        ),
        case(
            "header-unknown-label-then-meta",
            "text/html; charset=not-a-charset",
            not_utf8(page("", r#"<meta charset="shift_jis">"#, &encode(SHIFT_JIS, JAPANESE))),
            JAPANESE,
            Some("shift_jis"),
            "shift_jis",
        ),
        case(
            "xml-declaration",
            "text/html",
            not_utf8(page(
                r#"<?xml version="1.0" encoding="shift_jis"?>"#,
                "",
                &encode(SHIFT_JIS, JAPANESE),
            )),
            JAPANESE,
            Some("shift_jis"),
            "shift_jis",
        ),
        case(
            "charset-word-in-a-comment",
            "text/html",
            page("", "<!-- charset=shift_jis -->", UNDECLARED.as_bytes()),
            UNDECLARED,
            None,
            "utf-8",
        ),
        odd(
            "wrong-header-utf-8-on-latin1",
            "text/html; charset=utf-8",
            not_utf8(page("", "", &latin1("café déjà vu"))),
            String::from_utf8_lossy(&latin1("café déjà vu")).into_owned(),
            Some("utf-8"),
            "utf-8",
        ),
        odd(
            "wrong-meta-shift-jis-on-utf-8",
            "text/html",
            page("", r#"<meta charset="shift_jis">"#, "café déjà vu".as_bytes()),
            decoded_as(SHIFT_JIS, "café déjà vu".as_bytes()),
            Some("shift_jis"),
            "shift_jis",
        ),
        odd(
            "meta-after-1024-bytes-in-the-head",
            "text/html",
            not_utf8(page("", &format!(r#"{padding}<meta charset="utf-8">"#), &latin1(LATIN))),
            String::from_utf8_lossy(&latin1(LATIN)).into_owned(),
            Some("utf-8"),
            "utf-8",
        ),
        case(
            "meta-after-1024-bytes-in-the-body",
            "text/html",
            not_utf8(late_in_body),
            LATIN,
            Some("windows-1252"),
            "windows-1252",
        ),
        odd(
            "truncated-sequence-at-the-end",
            "text/html; charset=shift_jis",
            not_utf8(truncated),
            format!("{JAPANESE}\u{FFFD}"),
            Some("shift_jis"),
            "shift_jis",
        ),
    ];
    for case in &mut cases {
        let undeclared = case.name.starts_with("no-declaration")
            || matches!(
                case.name,
                "header-unknown-label" | "charset-word-in-a-comment" | "meta-after-1024-bytes-in-the-body"
            );
        if undeclared {
            case.browser_charset = None;
        }
        if std::str::from_utf8(&case.body).is_ok() && undeclared {
            case.browser_text = Some(decoded_as(WINDOWS_1252, case.text.as_bytes()));
        }
    }
    cases
}

/// Serve every case at `/p/<name>`, and a redirect to it at `/r/<name>`.
async fn site(cases: &[Case]) -> MockServer {
    let site = MockServer::start().await;
    for case in cases {
        Mock::given(method("GET"))
            .and(path(format!("/p/{}", case.name)))
            .respond_with(ResponseTemplate::new(200).set_body_raw(case.body.clone(), case.content_type))
            .mount(&site)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/r/{}", case.name)))
            .respond_with(ResponseTemplate::new(302).append_header("location", format!("/p/{}", case.name)))
            .mount(&site)
            .await;
    }
    site
}

/// What one read of a page reported.
struct Seen {
    html: String,
    markdown: String,
    charset: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Read {
    Scrape,
    Crawl,
}

async fn read(engine: &CrawlEngineHandle, how: Read, url: &str) -> Result<Seen, crawlberg::CrawlError> {
    match how {
        Read::Scrape => {
            let result = scrape(engine, url).await?;
            Ok(Seen {
                html: result.html,
                markdown: result.markdown.map(|markdown| markdown.content).unwrap_or_default(),
                charset: result.detected_charset,
            })
        }
        Read::Crawl => {
            let mut result = crawl(engine, url).await?;
            assert_eq!(result.pages.len(), 1, "the crawl of {url} must return one page");
            let page = result.pages.remove(0);
            Ok(Seen {
                html: page.html,
                markdown: page.markdown.map(|markdown| markdown.content).unwrap_or_default(),
                charset: page.detected_charset,
            })
        }
    }
}

/// The text of the first paragraph of `html`.
fn paragraph(html: &str) -> &str {
    let rest = html.split_once("<p>").map_or("", |(_, rest)| rest);
    rest.split_once("</p>").map_or(rest, |(text, _)| text)
}

/// What `detected_charset` must be.
enum Reports<'a> {
    Exactly(Option<&'a str>),
    /// A name, whichever one the browser's detection chose.
    #[cfg(feature = "browser")]
    SomeName,
}

/// Compare `seen` with `texts`, the texts `case` may read as, print the row, and add each
/// difference to `failures`.
fn judge(label: &str, case: &Case, texts: &[&str], reports: &Reports<'_>, seen: &Seen, failures: &mut Vec<String>) {
    let text = paragraph(&seen.html);
    let shown: String = text.chars().take(48).collect();
    let in_markdown = texts.iter().any(|text| seen.markdown.contains(text));
    eprintln!(
        "ROW {label} case={} charset={:?} text_ok={} markdown_ok={in_markdown} text={shown:?}",
        case.name,
        seen.charset,
        texts.contains(&text),
    );
    if !texts.contains(&text) {
        failures.push(format!(
            "{label} {}: the text must be one of {texts:?}, got {shown:?}",
            case.name
        ));
    }
    if case.in_markdown && !in_markdown {
        failures.push(format!(
            "{label} {}: the markdown must hold one of {texts:?}, got {:?}",
            case.name, seen.markdown
        ));
    }
    let charset_ok = match reports {
        Reports::Exactly(charset) => seen.charset.as_deref() == *charset,
        #[cfg(feature = "browser")]
        Reports::SomeName => seen.charset.as_deref().is_some_and(|name| !name.is_empty()),
    };
    if !charset_ok {
        failures.push(format!("{label} {}: detected_charset is {:?}", case.name, seen.charset));
    }
}

fn http_engine() -> CrawlEngineHandle {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.browser.mode = BrowserMode::Never;
    config.respect_robots_txt = false;
    create_engine(Some(config)).expect("engine must build")
}

/// Read every case in HTTP mode, directly and through a redirect, and require Chrome's text.
async fn assert_http_mode(how: Read) {
    let cases = cases();
    let site = site(&cases).await;
    let engine = http_engine();
    let mut failures = Vec::new();
    let mut rows = 0;
    for route in ["p", "r"] {
        for case in &cases {
            let url = format!("{}/{route}/{}", site.uri(), case.name);
            let label = format!("http {how:?} /{route}");
            match read(&engine, how, &url).await {
                Ok(seen) => judge(
                    &label,
                    case,
                    &[&case.text],
                    &Reports::Exactly(case.http_charset),
                    &seen,
                    &mut failures,
                ),
                Err(error) => failures.push(format!("{label} {}: {error}", case.name)),
            }
            rows += 1;
        }
    }
    assert_eq!(rows, cases.len() * 2, "every case must run on both routes");
    assert!(
        failures.is_empty(),
        "{} wrong:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[tokio::test]
async fn a_scrape_in_http_mode_reads_a_page_with_the_character_set_a_browser_uses() {
    assert_http_mode(Read::Scrape).await;
}

#[tokio::test]
async fn a_crawl_in_http_mode_reads_a_page_with_the_character_set_a_browser_uses() {
    assert_http_mode(Read::Crawl).await;
}

mod cache {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use crawlberg::traits::CrawlCache;
    use crawlberg::{BrowserMode, CachedPage, CrawlConfig, CrawlEngine, CrawlError};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{JAPANESE, LATIN, SHIFT_JIS, encode, latin1, not_utf8, page, paragraph};

    #[derive(Clone, Default)]
    struct MemoryCache(Arc<Mutex<HashMap<String, CachedPage>>>);

    #[async_trait]
    impl CrawlCache for MemoryCache {
        async fn get(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
            Ok(self.0.lock().expect("lock").get(key).cloned())
        }
        async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError> {
            self.0.lock().expect("lock").insert(key.to_owned(), page.clone());
            Ok(())
        }
        async fn has(&self, key: &str) -> Result<bool, CrawlError> {
            Ok(self.0.lock().expect("lock").contains_key(key))
        }
    }

    /// A page the cache answers reads as the page the server sent.
    #[tokio::test]
    async fn a_page_from_the_cache_reads_as_the_page_from_the_server() {
        let site = MockServer::start().await;
        let pages = [
            (
                "header",
                "text/html; charset=iso-8859-1",
                not_utf8(page("", "", &latin1(LATIN))),
                LATIN,
            ),
            (
                "meta",
                "text/html",
                not_utf8(page("", r#"<meta charset="shift_jis">"#, &encode(SHIFT_JIS, JAPANESE))),
                JAPANESE,
            ),
            ("none", "text/html", not_utf8(page("", "", &latin1(LATIN))), LATIN),
        ];
        for (name, content_type, body, _) in &pages {
            Mock::given(method("GET"))
                .and(path(format!("/{name}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(body.clone(), content_type)
                        .append_header("cache-control", "max-age=600"),
                )
                .mount(&site)
                .await;
        }
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.browser.mode = BrowserMode::Never;
        config.respect_robots_txt = false;
        let engine = CrawlEngine::builder()
            .config(config)
            .cache(MemoryCache::default())
            .build()
            .expect("engine must build");

        let mut failures = Vec::new();
        for (name, _, _, text) in &pages {
            let url = format!("{}/{name}", site.uri());
            let first = engine.scrape(&url).await.expect("the first scrape must succeed");
            let second = engine.scrape(&url).await.expect("the second scrape must succeed");
            let requests = site
                .received_requests()
                .await
                .expect("the server records its requests")
                .iter()
                .filter(|request| request.url.path() == format!("/{name}"))
                .count();
            eprintln!(
                "ROW cache case={name} requests={requests} first={:?}/{:?} second={:?}/{:?}",
                first.detected_charset,
                paragraph(&first.html).chars().take(24).collect::<String>(),
                second.detected_charset,
                paragraph(&second.html).chars().take(24).collect::<String>(),
            );
            if requests != 1 {
                failures.push(format!(
                    "{name}: the cache must answer the second scrape, the server got {requests} requests"
                ));
            }
            if paragraph(&first.html) != *text {
                failures.push(format!(
                    "{name}: the first scrape must read {text:?}, got {:?}",
                    paragraph(&first.html)
                ));
            }
            if paragraph(&second.html) != *text {
                failures.push(format!(
                    "{name}: the scrape from the cache must read {text:?}, got {:?}",
                    paragraph(&second.html)
                ));
            }
            if second.detected_charset != first.detected_charset {
                failures.push(format!(
                    "{name}: the scrape from the cache must report {:?}, got {:?}",
                    first.detected_charset, second.detected_charset
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} wrong:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

mod bypass {
    use std::sync::Arc;

    use async_trait::async_trait;
    use crawlberg::{
        BypassProvider, BypassResponse, CrawlConfig, CrawlEngine, CrawlError, DispatchProfile, EscalationStrategy,
    };

    use super::{JAPANESE, SHIFT_JIS, encode, not_utf8, page, paragraph};

    /// A provider that answers with the bytes of one page and a lossy UTF-8 read of them.
    #[derive(Debug)]
    struct OnePage(Vec<u8>);

    #[async_trait]
    impl BypassProvider for OnePage {
        async fn fetch(&self, _url: &str) -> Result<BypassResponse, CrawlError> {
            Ok(BypassResponse {
                status: 200,
                content_type: "text/html".to_owned(),
                body: String::from_utf8_lossy(&self.0).into_owned(),
                body_bytes: self.0.clone(),
                headers: std::collections::HashMap::new(),
                final_url: String::new(),
                cost_usd: None,
                vendor_request_id: None,
            })
        }

        fn vendor_name(&self) -> &'static str {
            "one-page"
        }
    }

    /// The bytes a bypass provider returns are read with their character set, as the bytes of
    /// the HTTP tier are.
    #[tokio::test]
    async fn a_page_from_a_bypass_provider_is_read_with_its_character_set() {
        let body = not_utf8(page("", "", &encode(SHIFT_JIS, JAPANESE)));
        let config = CrawlConfig {
            dispatch: Some(DispatchProfile {
                strategy: EscalationStrategy::BypassFirst,
                bypass: Some(Arc::new(OnePage(body)) as _),
                ..DispatchProfile::default()
            }),
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        };
        let engine = CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build");
        let result = engine
            .scrape("http://127.0.0.1:9/page")
            .await
            .expect("the scrape must succeed");
        assert_eq!(paragraph(&result.html), JAPANESE);
        assert_eq!(result.detected_charset.as_deref(), Some("shift_jis"));
    }
}

#[cfg(feature = "browser")]
mod browser {
    use std::time::Duration;

    use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine};

    use super::{Read, Reports, cases, judge, read, site};

    fn engine(backend: BrowserBackend) -> crawlberg::CrawlEngineHandle {
        let config = CrawlConfig {
            browser: BrowserConfig {
                backend,
                mode: BrowserMode::Always,
                timeout: Duration::from_secs(20),
                ..BrowserConfig::default()
            },
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        };
        create_engine(Some(config)).expect("engine must build")
    }

    fn chrome_missing(test_name: &str, error: &CrawlError) -> bool {
        match error {
            CrawlError::BrowserError { message, .. } if message.contains("auto detect a chrome executable") => {
                eprintln!("skipping {test_name} because no usable Chrome was found: {message}");
                true
            }
            _ => false,
        }
    }

    /// Read every case in browser mode, directly and through a redirect, and require the text
    /// and the character set of the browser's own document.
    async fn assert_browser_mode(test_name: &str, backend: BrowserBackend, how: Read) {
        let cases = cases();
        let site = site(&cases).await;
        let engine = engine(backend.clone());
        let mut failures = Vec::new();
        let mut rows = 0;
        for route in ["p", "r"] {
            for case in &cases {
                let url = format!("{}/{route}/{}", site.uri(), case.name);
                let label = format!("{backend:?} {how:?} /{route}");
                match read(&engine, how, &url).await {
                    Ok(seen) => {
                        let mut texts = vec![case.text.as_str()];
                        texts.extend(case.browser_text.as_deref());
                        let reports = case
                            .browser_charset
                            .map_or(Reports::SomeName, |name| Reports::Exactly(Some(name)));
                        judge(&label, case, &texts, &reports, &seen, &mut failures);
                    }
                    Err(error) if chrome_missing(test_name, &error) => return,
                    Err(error) => failures.push(format!("{label} {}: {error}", case.name)),
                }
                rows += 1;
            }
        }
        assert_eq!(rows, cases.len() * 2, "every case must run on both routes");
        assert!(
            failures.is_empty(),
            "{} wrong:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[tokio::test]
    async fn a_scrape_in_browser_mode_keeps_the_text_the_browser_decoded() {
        assert_browser_mode(
            "a_scrape_in_browser_mode_keeps_the_text_the_browser_decoded",
            BrowserBackend::Chromiumoxide,
            Read::Scrape,
        )
        .await;
    }

    #[tokio::test]
    async fn a_crawl_in_browser_mode_keeps_the_text_the_browser_decoded() {
        assert_browser_mode(
            "a_crawl_in_browser_mode_keeps_the_text_the_browser_decoded",
            BrowserBackend::Chromiumoxide,
            Read::Crawl,
        )
        .await;
    }

    /// The native backend decides the character set of a document as HTTP mode does, so it reads
    /// every page as HTTP mode reads it, and nothing decodes its text a second time.
    #[cfg(feature = "browser-native")]
    #[tokio::test]
    async fn the_native_backend_reads_a_page_as_http_mode_reads_it() {
        let cases = cases();
        let site = site(&cases).await;
        let engine = engine(BrowserBackend::Native);
        let mut failures = Vec::new();
        let mut rows = 0;
        for how in [Read::Scrape, Read::Crawl] {
            for route in ["p", "r"] {
                for case in &cases {
                    let url = format!("{}/{route}/{}", site.uri(), case.name);
                    let label = format!("Native {how:?} /{route}");
                    let reports = Reports::Exactly(case.http_charset);
                    match read(&engine, how, &url).await {
                        Ok(seen) => judge(&label, case, &[&case.text], &reports, &seen, &mut failures),
                        Err(error) => failures.push(format!("{label} {}: {error}", case.name)),
                    }
                    rows += 1;
                }
            }
        }
        assert_eq!(
            rows,
            cases.len() * 4,
            "every case must run on both routes, scraped and crawled"
        );
        assert!(
            failures.is_empty(),
            "{} wrong:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
