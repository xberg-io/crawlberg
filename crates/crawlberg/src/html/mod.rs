//! HTML parsing helpers for metadata extraction, link discovery, and content processing.

mod charset;
mod content;
mod detection;
mod extract;
mod feeds;
mod images;
mod json_ld;
mod link_targets;
mod links;
mod metadata;
mod raw_text;
pub(crate) mod selectors;
mod start_tags;

use tl::{HTMLTag, Parser, VDom};
use url::Url;

/// Whether an address attribute is blank, meaning it carries no reference at all.
pub(crate) fn is_blank_address(value: &str) -> bool {
    // ~keep Only ASCII whitespace counts. HTML strips nothing else from a URL attribute, so U+00A0
    // and U+2000-200A belong to the value and are percent-encoded: an NBSP-only reference is real,
    // if useless, and must not be treated as blank (#191). A blank reference must be skipped rather
    // than resolved, because joining one to a base yields the base itself (#187, #220).
    value.bytes().all(|byte| byte.is_ascii_whitespace())
}

pub(crate) fn resolve_url(src: &str, base_url: &Url) -> String {
    base_url
        .join(src)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| src.to_owned())
}

/// Get a string attribute value from an HTMLTag.
///
/// Returns `None` if the attribute does not exist or has no value.
pub(crate) fn get_attr<'a>(tag: &'a HTMLTag<'_>, attr: &'a str) -> Option<&'a str> {
    tag.attributes().get(attr).flatten().and_then(|b| b.try_as_utf8_str())
}

/// Iterate over nodes matching a CSS selector, calling the closure for each tag.
pub(crate) fn query_tags<'a, F>(dom: &'a VDom<'a>, selector: &str, mut f: F)
where
    F: FnMut(&HTMLTag<'a>, &Parser<'a>),
{
    let parser = dom.parser();
    if let Some(iter) = dom.query_selector(selector) {
        for handle in iter {
            if let Some(node) = handle.get(parser)
                && let Some(tag) = node.as_tag()
            {
                f(tag, parser);
            }
        }
    }
}

pub(crate) use charset::detect_charset;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use detection::is_pdf_url;
pub(crate) use detection::{is_binary_content_type, is_binary_url, is_html_content, is_pdf_content};
pub(crate) use extract::HtmlExtraction;
pub(crate) use extract::extract_page_data;
pub(crate) use link_targets::resolve_link_targets;
pub(crate) use links::{effective_base_url, extract_links};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use metadata::detect_meta_refresh;
pub(crate) use metadata::{detect_nofollow, detect_noindex};
pub(crate) use raw_text::mask_raw_text_markup;
