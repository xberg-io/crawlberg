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

use std::borrow::Cow;
use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
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

/// Decode the character references in a raw attribute value (`&amp;`, `&#x2F;`), as an HTML
/// parser does before it uses the value.
///
/// ~keep html5ever's tokenizer does the decoding, because it follows the WHATWG rules a browser
/// ~keep does, including the Windows-1252 table for `&#128;`-`&#159;`. The value is wrapped in a
/// ~keep double-quoted attribute; each `"` in it becomes `&quot;`, which decodes back to `"`.
pub(crate) fn decode_attr_value(raw: &str) -> Cow<'_, str> {
    if !raw.contains('&') {
        return Cow::Borrowed(raw);
    }
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(format!("<a v=\"{}\">", raw.replace('"', "&quot;"))));
    let tokenizer = Tokenizer::new(FirstAttrValue::default(), TokenizerOpts::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer
        .sink
        .0
        .into_inner()
        .map_or(Cow::Borrowed(raw), |value| Cow::Owned(value.to_string()))
}

/// A token sink that keeps the value of the first attribute of the tag it sees.
#[derive(Default)]
struct FirstAttrValue(RefCell<Option<StrTendril>>);

impl TokenSink for FirstAttrValue {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        if let Token::TagToken(tag) = token
            && let Some(attr) = tag.attrs.into_iter().next()
        {
            *self.0.borrow_mut() = Some(attr.value);
        }
        TokenSinkResult::Continue
    }
}

/// Every element named `name`, in document order, with the name compared regardless of ASCII
/// case as HTML compares it.
///
/// ~keep tl lowercases attribute names when it parses, but its selectors compare tag names
/// ~keep byte for byte, so a `base[href]` query misses `<BASE HREF>`.
pub(crate) fn elements_named<'d, 'a>(dom: &'d VDom<'a>, name: &'d str) -> impl Iterator<Item = &'d HTMLTag<'a>> {
    dom.nodes()
        .iter()
        .filter_map(|node| node.as_tag())
        .filter(move |tag| tag.name().as_bytes().eq_ignore_ascii_case(name.as_bytes()))
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
pub(crate) use links::extract_links;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use metadata::detect_meta_refresh;
pub(crate) use metadata::{detect_nofollow, detect_noindex};
pub(crate) use raw_text::mask_raw_text_markup;
