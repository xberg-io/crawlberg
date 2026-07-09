//! CSS selector strings and regex patterns used across html submodules.
//!
//! With `tl`, CSS selectors are parsed on each `query_selector` call,
//! so we store them as string constants rather than pre-compiled objects.

use std::sync::LazyLock;

use regex::Regex;

pub(super) static META_RE_NAME_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<meta\s+[^>]*name\s*=\s*["']([^"']+)["'][^>]*content\s*=\s*["']([^"']+)["'][^>]*>"#)
        .expect("valid regex: META_RE_NAME_CONTENT")
});
pub(super) static META_RE_CONTENT_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<meta\s+[^>]*content\s*=\s*["']([^"']+)["'][^>]*name\s*=\s*["']([^"']+)["'][^>]*>"#)
        .expect("valid regex: META_RE_CONTENT_NAME")
});

pub(super) const SEL_META: &str = "meta";
pub(super) const SEL_TITLE: &str = "title";
pub(super) const SEL_CANONICAL: &str = "link[rel='canonical']";
pub(super) const SEL_ROBOTS_META: &str = "meta[name='robots']";
#[cfg(not(target_arch = "wasm32"))]
pub(super) const SEL_META_REFRESH: &str = "meta[http-equiv='refresh']";
pub(super) const SEL_A_HREF: &str = "a[href]";
pub(super) const SEL_BASE_HREF: &str = "base[href]";
pub(crate) const SEL_IMG_SRC: &str = "img[src]";
pub(super) const SEL_SOURCE_SRCSET: &str = "source[srcset]";
pub(super) const SEL_OG_IMAGE: &str = "meta[property='og:image']";
pub(super) const SEL_TWITTER_IMAGE: &str = "meta[name='twitter:image']";
pub(super) const SEL_FEED_ALTERNATE: &str = "link[rel='alternate']";
pub(super) const SEL_JSON_LD: &str = "script[type='application/ld+json']";
pub(super) const SEL_HTML: &str = "html";
pub(super) const SEL_HREFLANG: &str = "link[rel='alternate'][hreflang]";
pub(super) const SEL_FAVICON: &str = "link[rel='icon'], link[rel='shortcut icon'], link[rel='apple-touch-icon']";
pub(super) const SEL_HEADINGS: &str = "h1, h2, h3, h4, h5, h6";
pub(crate) const SEL_LINK_CSS: &str = "link[rel='stylesheet'][href]";
pub(crate) const SEL_SCRIPT_SRC: &str = "script[src]";
