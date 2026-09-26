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
pub(crate) mod selectors;

use std::borrow::Cow;
use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use tl::{HTMLTag, Parser, VDom};
use url::Url;

/// Resolve `src` against `base_url`, keeping it as written when it does not parse. An empty
/// `src` stays empty.
pub(crate) fn resolve_url(src: &str, base_url: &Url) -> String {
    if src.is_empty() {
        return String::new();
    }
    base_url
        .join(src)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| src.to_owned())
}

/// Parse an HTML document with every tag name in lowercase.
///
/// ~keep HTML tag names are case-insensitive, but tl's selectors compare them byte for byte,
/// ~keep so `a[href]` would miss `<A HREF>`. tl already lowercases attribute names. Parse
/// ~keep every document crawlberg queries through here, so no selector needs its own fix.
pub(crate) fn parse_html(html: &str) -> Result<VDom<'_>, tl::ParseError> {
    let mut dom = tl::parse(html, tl::ParserOptions::default())?;
    for tag in dom.nodes_mut().iter_mut().filter_map(|node| node.as_tag_mut()) {
        let name = tag.name().as_bytes();
        if name.iter().any(u8::is_ascii_uppercase) {
            let lowercase = name.to_ascii_lowercase();
            tag.name_mut()
                .set(lowercase)
                .expect("lowercasing a tag name keeps its length");
        }
    }
    Ok(dom)
}

/// Get an attribute value from an HTMLTag, with its character references decoded.
///
/// Returns `None` if the attribute does not exist, has no value, or is not UTF-8.
pub(crate) fn get_attr<'a>(tag: &'a HTMLTag<'_>, attr: &'a str) -> Option<Cow<'a, str>> {
    tag.attributes()
        .get(attr)
        .flatten()
        .and_then(|b| b.try_as_utf8_str())
        .map(decode_attr_value)
}

/// Whether the tag's `attr` value, without surrounding ASCII whitespace, equals `expected` in any
/// ASCII case.
///
/// ~keep HTML compares values such as `name`, `http-equiv` and `type` without case, but tl's
/// ~keep attribute selectors compare them byte for byte and cannot parse the CSS `i` flag. Select
/// ~keep the tag and compare the value here instead.
pub(crate) fn attr_eq(tag: &HTMLTag<'_>, attr: &str, expected: &str) -> bool {
    get_attr(tag, attr).is_some_and(|value| value.trim_ascii().eq_ignore_ascii_case(expected))
}

/// The essence of the tag's `type` value, a MIME type, in lowercase: the part before any `;`
/// parameters, without the tab, LF, CR and space around it.
///
/// ~keep The WHATWG MIME type parser strips HTTP whitespace, which has no form feed, so this
/// ~keep does not use `trim_ascii`.
pub(crate) fn mime_essence(tag: &HTMLTag<'_>) -> Option<String> {
    get_attr(tag, "type").map(|value| {
        let essence = value.split(';').next().unwrap_or_default();
        essence
            .trim_matches(|c| matches!(c, '\t' | '\n' | '\r' | ' '))
            .to_ascii_lowercase()
    })
}

/// Whether the tag's `rel` value, a space-separated list of tokens, holds `token` in any ASCII case.
pub(crate) fn has_rel(tag: &HTMLTag<'_>, token: &str) -> bool {
    get_attr(tag, "rel").is_some_and(|rel| rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(token)))
}

/// Decode the character references in a raw attribute value (`&amp;`, `&#x2F;`) and turn each
/// CR or CRLF into LF and each NUL into U+FFFD, as an HTML parser does before it uses the value.
///
/// ~keep html5ever's tokenizer decodes a value that has a `&`, because it follows the WHATWG rules
/// ~keep a browser does, including the Windows-1252 table for `&#128;`-`&#159;`. For a value
/// ~keep without one, the tokenizer only rewrites CR and NUL, so that is done here directly and
/// ~keep the tokenizer, the slow part of reading a value, is not started.
pub(crate) fn decode_attr_value(raw: &str) -> Cow<'_, str> {
    let Some(first) = raw.bytes().position(|b| matches!(b, b'&' | b'\r' | b'\0')) else {
        return Cow::Borrowed(raw);
    };
    if raw.as_bytes()[first..].contains(&b'&') {
        return decode_with_tokenizer(raw);
    }
    let mut out = String::with_capacity(raw.len() + 2);
    out.push_str(&raw[..first]);
    let mut chars = raw[first..].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                out.push('\n');
                chars.next_if_eq(&'\n');
            }
            '\0' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Decode `raw` with html5ever's tokenizer. The value is wrapped in a double-quoted attribute;
/// each `"` in it becomes `&quot;`, which decodes back to `"`.
fn decode_with_tokenizer(raw: &str) -> Cow<'_, str> {
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
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use extract::HtmlExtraction;
pub(crate) use extract::extract_page_data;
pub(crate) use link_targets::resolve_link_targets;
pub(crate) use links::{effective_base_url, extract_links};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use metadata::detect_meta_refresh;
pub(crate) use metadata::{detect_nofollow, detect_noindex};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_normalizes_newlines_and_nul_as_an_html_parser_does() {
        assert_eq!(decode_attr_value("a\r\nb\rc\nd\0e"), "a\nb\nc\nd\u{FFFD}e");
        assert_eq!(decode_attr_value("a&amp;\r\nb\0"), "a&\nb\u{FFFD}");
        assert_eq!(decode_attr_value("a\0b"), "a\u{FFFD}b");
        assert_eq!(decode_attr_value("a\rb"), "a\nb");
        assert!(matches!(decode_attr_value("plain value"), Cow::Borrowed("plain value")));
    }

    #[test]
    fn values_without_an_ampersand_decode_as_the_tokenizer_decodes_them() {
        // ~keep Every value of up to five characters over the characters the tokenizer treats
        // ~keep specially inside a double-quoted value, and a few it does not.
        const ALPHABET: [char; 8] = ['a', '\r', '\n', '\0', '"', '<', '\'', '\u{e9}'];
        let mut values = vec![String::new()];
        let mut checked = 0;
        for _ in 0..5 {
            values = values
                .iter()
                .flat_map(|v| ALPHABET.iter().map(move |c| format!("{v}{c}")))
                .collect();
            for value in &values {
                assert_eq!(decode_attr_value(value), decode_with_tokenizer(value), "for {value:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, (1..=5).map(|n| 8_usize.pow(n)).sum::<usize>());
    }
}
