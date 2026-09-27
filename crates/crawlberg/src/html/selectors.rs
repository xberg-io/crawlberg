//! CSS selector strings and regex patterns used across html submodules.
//!
//! With `tl`, CSS selectors are parsed on each `query_selector` call,
//! so we store them as string constants rather than pre-compiled objects.

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

// ~keep No selector here matches an attribute value: tl compares values byte for byte, and HTML
// ~keep compares `rel`, `name`, `http-equiv` and `type` without case. Each caller selects the tag
// ~keep and checks the value with `attr_eq` or `has_rel`.
pub(super) const SEL_META: &str = "meta";
pub(super) const SEL_TITLE: &str = "title";
pub(super) const SEL_A_HREF: &str = "a[href]";
pub(super) const SEL_BASE_HREF: &str = "base[href]";
pub(crate) const SEL_IMG_SRC: &str = "img[src]";
pub(super) const SEL_SOURCE_SRCSET: &str = "source[srcset]";
pub(crate) const SEL_LINK_REL: &str = "link[rel]";
pub(super) const SEL_HREFLANG: &str = "link[hreflang]";
pub(super) const SEL_SCRIPT_TYPE: &str = "script[type]";
pub(super) const SEL_HTML: &str = "html";
pub(super) const SEL_HEADINGS: &str = "h1, h2, h3, h4, h5, h6";
pub(crate) const SEL_SCRIPT_SRC: &str = "script[src]";
