//! Content verification for reachability benchmarks.

use crate::adapter::ScrapeOutput;
use crate::types::{ReachabilityResult, ScrapeFixture};

/// Verify that scraped content is real (not blocked/captcha/fake).
///
/// Checks:
/// 1. CSS selectors: searches the raw HTML for matching elements using substring
///    matching (no DOM parser in the harness, so simple string search is used).
/// 2. Text verification: case-insensitive search in the content output.
/// 3. False positive: HTTP 200 but verification failed.
///
/// When no verification rules are defined the result is trivially `verified = true`.
pub fn verify_content(fixture: &ScrapeFixture, output: &ScrapeOutput) -> ReachabilityResult {
    let selectors_total = fixture.verify_selectors.len();
    let text_total = fixture.verify_text.len();

    if selectors_total == 0 && text_total == 0 {
        return ReachabilityResult {
            verified: true,
            selectors_found: 0,
            selectors_total: 0,
            text_found: 0,
            text_total: 0,
            is_false_positive: false,
        };
    }

    let html_lower = output.html.to_lowercase();
    let content_lower = output.content.as_deref().unwrap_or("").to_lowercase();

    let selectors_found = fixture
        .verify_selectors
        .iter()
        .filter(|sel| selector_matches(&html_lower, sel))
        .count();

    let text_found = fixture
        .verify_text
        .iter()
        .filter(|text| {
            let needle = text.to_lowercase();
            content_lower.contains(&needle) || html_lower.contains(&needle)
        })
        .count();

    let selector_ok = selectors_total == 0 || selectors_found >= selectors_total.saturating_sub(1);
    let text_ok = text_total == 0 || text_found > 0;
    let verified = selector_ok && text_ok;

    let is_false_positive = output.status_code == 200 && !verified;

    ReachabilityResult {
        verified,
        selectors_found,
        selectors_total,
        text_found,
        text_total,
        is_false_positive,
    }
}

/// Simple selector matching against raw (lowercased) HTML.
///
/// Handles four selector forms:
/// - `.class` — looks for the class name inside a `class` attribute value.
/// - `#id` — looks for `id="…"` or `id='…'`.
/// - `[attr]` / `[attr='value']` — looks for the inner attribute string.
/// - bare tag (e.g. `div`) — looks for `<tag` or `<tag ` in the HTML.
fn selector_matches(html: &str, selector: &str) -> bool {
    let sel = selector.trim().to_lowercase();

    if let Some(class) = sel.strip_prefix('.') {
        html.contains(&format!("class=\"{class}"))
            || html.contains(&format!("class='{class}"))
            || html.contains(&format!(" {class}\""))
            || html.contains(&format!(" {class} "))
    } else if let Some(id) = sel.strip_prefix('#') {
        html.contains(&format!("id=\"{id}\"")) || html.contains(&format!("id='{id}'"))
    } else if sel.starts_with('[') && sel.ends_with(']') {
        let inner = &sel[1..sel.len() - 1];
        html.contains(inner)
    } else {
        html.contains(&format!("<{sel}")) || html.contains(&format!("<{sel} "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_output(html: &str, content: Option<&str>, status_code: u16) -> ScrapeOutput {
        ScrapeOutput {
            status_code,
            content: content.map(str::to_owned),
            html: html.to_owned(),
            content_size: content.map(|c| c.len()).unwrap_or(0),
            browser_used: false,
            js_render_hint: false,
            error: None,
        }
    }

    fn make_fixture(selectors: Vec<&str>, texts: Vec<&str>) -> ScrapeFixture {
        ScrapeFixture {
            id: "test".to_owned(),
            url: "https://example.com".to_owned(),
            truth_text: None,
            lie_text: None,
            error: None,
            split: None,
            tags: vec![],
            expected_status: None,
            verify_selectors: selectors.into_iter().map(str::to_owned).collect(),
            verify_text: texts.into_iter().map(str::to_owned).collect(),
            category: None,
        }
    }

    #[test]
    fn test_no_rules_is_trivially_verified() {
        let fixture = make_fixture(vec![], vec![]);
        let output = make_output("<html></html>", None, 200);
        let result = verify_content(&fixture, &output);
        assert!(result.verified);
        assert!(!result.is_false_positive);
        assert_eq!(result.selectors_total, 0);
        assert_eq!(result.text_total, 0);
    }

    #[test]
    fn test_text_found_in_content() {
        let fixture = make_fixture(vec![], vec!["Add to Cart"]);
        let output = make_output("<html></html>", Some("Add to Cart now"), 200);
        let result = verify_content(&fixture, &output);
        assert!(result.verified);
        assert_eq!(result.text_found, 1);
        assert!(!result.is_false_positive);
    }

    #[test]
    fn test_text_case_insensitive() {
        let fixture = make_fixture(vec![], vec!["add to cart"]);
        let output = make_output("<html></html>", Some("ADD TO CART"), 200);
        let result = verify_content(&fixture, &output);
        assert!(result.verified);
    }

    #[test]
    fn test_text_not_found_is_false_positive_on_200() {
        let fixture = make_fixture(vec![], vec!["expected content"]);
        let output = make_output("<html>captcha page</html>", Some("captcha"), 200);
        let result = verify_content(&fixture, &output);
        assert!(!result.verified);
        assert!(result.is_false_positive);
    }

    #[test]
    fn test_text_not_found_non_200_not_false_positive() {
        let fixture = make_fixture(vec![], vec!["expected content"]);
        let output = make_output("<html>blocked</html>", Some("blocked"), 403);
        let result = verify_content(&fixture, &output);
        assert!(!result.verified);
        assert!(!result.is_false_positive);
    }

    #[test]
    fn test_class_selector_match() {
        let fixture = make_fixture(vec![".product-title"], vec![]);
        let output = make_output(r#"<h1 class="product-title">Laptop</h1>"#, Some("Laptop"), 200);
        let result = verify_content(&fixture, &output);
        assert_eq!(result.selectors_found, 1);
        assert!(result.verified);
    }

    #[test]
    fn test_id_selector_match() {
        let fixture = make_fixture(vec!["#productTitle"], vec![]);
        let output = make_output(r#"<span id="productTitle">Widget</span>"#, Some("Widget"), 200);
        let result = verify_content(&fixture, &output);
        assert_eq!(result.selectors_found, 1);
        assert!(result.verified);
    }

    #[test]
    fn test_tag_selector_match() {
        let fixture = make_fixture(vec!["article"], vec![]);
        let output = make_output("<article>content</article>", Some("content"), 200);
        let result = verify_content(&fixture, &output);
        assert_eq!(result.selectors_found, 1);
        assert!(result.verified);
    }

    #[test]
    fn test_attribute_selector_match() {
        let fixture = make_fixture(vec!["[data-testid='cart-button']"], vec![]);
        let output = make_output(r#"<button data-testid='cart-button'>Add</button>"#, Some("Add"), 200);
        let result = verify_content(&fixture, &output);
        assert_eq!(result.selectors_found, 1);
        assert!(result.verified);
    }

    #[test]
    fn test_one_missing_selector_still_verified() {
        let fixture = make_fixture(vec![".exists", ".missing"], vec![]);
        let output = make_output(r#"<div class="exists">ok</div>"#, Some("ok"), 200);
        let result = verify_content(&fixture, &output);
        assert_eq!(result.selectors_found, 1);
        assert_eq!(result.selectors_total, 2);
        assert!(result.verified);
    }

    #[test]
    fn test_text_found_in_html_when_no_content() {
        let fixture = make_fixture(vec![], vec!["rust-lang"]);
        let output = make_output("<html>rust-lang</html>", None, 200);
        let result = verify_content(&fixture, &output);
        assert!(result.verified);
    }
}
