//! Detection heuristics for identifying pages that need JavaScript rendering.
//!
//! These are pure functions operating on HTML strings — no browser or network
//! access required. Always compiled regardless of feature flags.

use std::sync::LazyLock;

use regex::Regex;
use tl::ParserOptions;

/// Minimum word count to consider a page as having substantial content.
const MIN_CONTENT_WORD_COUNT: usize = 50;

/// Word count below which script tags suggest client-side rendering.
const SPARSE_CONTENT_WORD_COUNT: usize = 20;

/// Well-known SPA mount-point IDs with typically empty content.
static SPA_MOUNT_IDS: &[&str] = &["root", "app", "__next", "__nuxt"];

/// Pattern matching common noscript warning text.
static NOSCRIPT_WARNING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(enable|need|require)\s+(javascript|js)").expect("noscript warning regex should compile")
});

/// Detect whether a page's HTML content suggests it needs JavaScript rendering
/// to produce meaningful content.
///
/// Returns `true` when the page appears to be a client-side rendered SPA shell
/// with no substantial server-rendered content.
pub(crate) fn detect_js_render_needed(body: &str, word_count: usize) -> bool {
    if word_count >= MIN_CONTENT_WORD_COUNT {
        return false;
    }

    let parsed_html = crate::html::mask_raw_text_markup(body);
    let Ok(dom) = tl::parse(&parsed_html, ParserOptions::default()) else {
        return false;
    };
    let parser = dom.parser();

    if has_empty_spa_mount(&dom) {
        return true;
    }

    if has_noscript_js_warning(&dom, parser) {
        return true;
    }

    if word_count < SPARSE_CONTENT_WORD_COUNT && has_script_tags(&dom) {
        return true;
    }

    false
}

/// Check if the document has a well-known SPA mount-point div that is empty
/// or contains only whitespace.
fn has_empty_spa_mount(dom: &tl::VDom<'_>) -> bool {
    let parser = dom.parser();
    for id in SPA_MOUNT_IDS {
        if let Some(handle) = dom.get_element_by_id(*id)
            && let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
        {
            let inner_text = tag.inner_text(parser);
            if inner_text.trim().is_empty() {
                return true;
            }
        }
    }
    false
}

/// Check if the document has a `<noscript>` element with text suggesting
/// JavaScript is required to use the page.
fn has_noscript_js_warning(dom: &tl::VDom<'_>, parser: &tl::Parser<'_>) -> bool {
    if let Some(iter) = dom.query_selector("noscript") {
        for handle in iter {
            if let Some(node) = handle.get(parser) {
                let text = node.inner_text(parser).to_string();
                if NOSCRIPT_WARNING.is_match(&text) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if the document has external script tags.
fn has_script_tags(dom: &tl::VDom<'_>) -> bool {
    dom.query_selector("script[src]")
        .is_some_and(|mut iter| iter.next().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn react_shell_detected() {
        let html = r#"<html><body><div id="root"></div>
            <noscript>You need to enable JavaScript to run this app.</noscript>
            <script src="/main.js"></script></body></html>"#;
        assert!(detect_js_render_needed(html, 0));
    }

    #[test]
    fn vue_shell_detected() {
        let html = r#"<html><body><div id="app"></div>
            <script src="/app.js"></script></body></html>"#;
        assert!(detect_js_render_needed(html, 0));
    }

    #[test]
    fn next_empty_detected() {
        let html = r#"<html><body><div id="__next"></div>
            <script id="__NEXT_DATA__" type="application/json">{}</script>
            <script src="/_next/static/main.js"></script></body></html>"#;
        assert!(detect_js_render_needed(html, 0));
    }

    #[test]
    fn nuxt_shell_detected() {
        let html = r#"<html><body><div id="__nuxt"></div>
            <script src="/_nuxt/entry.js"></script></body></html>"#;
        assert!(detect_js_render_needed(html, 0));
    }

    #[test]
    fn next_ssr_not_detected() {
        let html = r#"<html><body><div id="__next"><main><h1>Hello World</h1>
            <p>This is a server-rendered page with plenty of content that should not
            trigger the JS render hint because it has substantial text content already
            present in the HTML response from the server.</p></main></div>
            <script src="/_next/static/main.js"></script></body></html>"#;
        assert!(!detect_js_render_needed(html, 60));
    }

    #[test]
    fn normal_page_not_detected() {
        let html = r#"<html><body><h1>Example</h1>
            <p>This is a normal page with content.</p></body></html>"#;
        assert!(!detect_js_render_needed(html, 10));
    }

    #[test]
    fn minimal_page_no_scripts_not_detected() {
        let html = r#"<html><body><h1>Contact</h1>
            <p>Email us at hello@example.com</p></body></html>"#;
        assert!(!detect_js_render_needed(html, 8));
    }

    #[test]
    fn noscript_warning_triggers() {
        let html = r#"<html><body><div id="app"></div>
            <noscript><strong>Please enable JavaScript to continue.</strong></noscript>
            <script src="/app.js"></script></body></html>"#;
        assert!(detect_js_render_needed(html, 0));
    }

    // ~keep The word count sits between SPARSE_CONTENT_WORD_COUNT and MIN_CONTENT_WORD_COUNT in
    // ~keep the next two tests, so neither the early return nor the sparse-content-with-scripts
    // ~keep rule decides them: only the SPA-mount and noscript-warning checks can, and both read
    // ~keep the parsed document. Without the raw-text masking `tl` reads the markup written inside
    // ~keep the script and title text and both pages are reported as needing a browser.
    #[test]
    fn should_not_report_js_render_needed_when_the_spa_mount_only_appears_in_script_text() {
        let html = r#"<html><body>
            <p>This server-rendered article has real prose in it, enough words that the
            sparse-content rule cannot decide the page on its own.</p>
            <script>var shell = '<div id="root"></div>';</script>
            </body></html>"#;
        assert!(
            !detect_js_render_needed(html, 30),
            "an SPA mount div written inside script text is not an element a browser sees"
        );
    }

    #[test]
    fn should_not_report_js_render_needed_when_the_noscript_warning_only_appears_in_title_text() {
        let html = r#"<html><head><title>Docs <noscript>You need JavaScript</noscript></title></head>
            <body><p>This server-rendered article has real prose in it, enough words that the
            sparse-content rule cannot decide the page on its own.</p></body></html>"#;
        assert!(
            !detect_js_render_needed(html, 30),
            "a noscript warning written inside title text is not an element a browser sees"
        );
    }
}
