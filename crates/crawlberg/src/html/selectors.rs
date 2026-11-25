//! Element selectors and regex patterns used across html submodules.

use std::sync::LazyLock;

use regex::Regex;

// ~keep `(?i)`: HTML tag and attribute names are case-insensitive, so `<META NAME=...>` counts.
pub(super) static META_RE_NAME_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s+[^>]*name\s*=\s*["']([^"']+)["'][^>]*content\s*=\s*["']([^"']+)["'][^>]*>"#)
        .expect("valid regex: META_RE_NAME_CONTENT")
});
pub(super) static META_RE_CONTENT_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s+[^>]*content\s*=\s*["']([^"']+)["'][^>]*name\s*=\s*["']([^"']+)["'][^>]*>"#)
        .expect("valid regex: META_RE_CONTENT_NAME")
});

/// The elements named one of `names` (lower case) that carry the attribute `attr`, when given.
#[derive(Clone, Copy)]
pub(crate) struct Selector {
    pub(crate) names: &'static [&'static str],
    pub(crate) attr: Option<&'static str>,
}

const fn select(names: &'static [&'static str], attr: Option<&'static str>) -> Selector {
    Selector { names, attr }
}

// ~keep No selector here matches an attribute value: HTML compares `rel`, `name`, `http-equiv`
// ~keep and `type` without case. Each caller selects the tag and checks the value with `attr_eq`
// ~keep or `has_rel`.
pub(super) const SEL_META: Selector = select(&["meta"], None);
pub(super) const SEL_TITLE: Selector = select(&["title"], None);
/// The `name` of a robots meta tag addressed to every crawler rather than to a named one.
pub(super) const ROBOTS_META_NAME: &str = "robots";
pub(super) const SEL_A_HREF: Selector = select(&["a"], Some("href"));
pub(crate) const SEL_IMG_SRC: Selector = select(&["img"], Some("src"));
pub(super) const SEL_SOURCE_SRCSET: Selector = select(&["source"], Some("srcset"));
pub(crate) const SEL_LINK_REL: Selector = select(&["link"], Some("rel"));
pub(super) const SEL_HREFLANG: Selector = select(&["link"], Some("hreflang"));
pub(super) const SEL_SCRIPT_TYPE: Selector = select(&["script"], Some("type"));
pub(super) const SEL_HTML: Selector = select(&["html"], None);
pub(super) const SEL_HEADINGS: Selector = select(&["h1", "h2", "h3", "h4", "h5", "h6"], None);
pub(crate) const SEL_SCRIPT_SRC: Selector = select(&["script"], Some("src"));
pub(crate) const SEL_BODY: Selector = select(&["body"], None);
pub(crate) const SEL_NOSCRIPT: Selector = select(&["noscript"], None);
