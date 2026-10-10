//! HTML parsing helpers for metadata extraction, link discovery, and content processing.

mod charset;
mod content;
mod detection;
mod extract;
mod feeds;
mod images;
mod json_ld;
mod links;
mod metadata;
mod raw_text;
mod real_tags;
pub(crate) mod selectors;
mod srcset;
mod url_targets;

use std::borrow::Cow;
use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use tl::{HTMLTag, Parser, VDom};
use url::Url;

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
                .expect("a tag name tl parsed is at most u32::MAX bytes");
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

/// Get a URL attribute value such as `href` or `src`, decoded and cleaned by [`clean_url`].
pub(crate) fn get_url_attr<'a>(tag: &'a HTMLTag<'_>, attr: &'a str) -> Option<Cow<'a, str>> {
    get_attr(tag, attr).and_then(clean_url)
}

/// Remove what the WHATWG URL parser removes from a URL string: the C0 controls and spaces
/// (U+0000 to U+0020) at either end, and every tab, LF and CR inside.
///
/// Returns `None` when nothing is left: a blank URL points at the page itself. Other Unicode
/// spaces, such as U+00A0, stay, as they do in a browser.
pub(crate) fn clean_url(value: Cow<'_, str>) -> Option<Cow<'_, str>> {
    let is_tab_or_newline = |c: char| matches!(c, '\t' | '\n' | '\r');
    let trimmed = value.trim_matches(|c: char| c <= ' ');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains(is_tab_or_newline) {
        return Some(Cow::Owned(trimmed.replace(is_tab_or_newline, "")));
    }
    if trimmed.len() == value.len() {
        return Some(value);
    }
    Some(Cow::Owned(trimmed.to_owned()))
}

/// Whether the URL parser reads `address` as an absolute URL whose scheme is `scheme` (given in
/// lower case, without the colon). An address that does not parse has no scheme.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn has_scheme(address: &str, scheme: &str) -> bool {
    Url::parse(address).is_ok_and(|url| url.scheme() == scheme)
}

/// Whether `url` is a scheme the crawler can fetch. `http_fetch` builds on a `reqwest::Client`,
/// which speaks only `http` and `https`; every other scheme, including the inline `data:`,
/// `javascript:` and `vbscript:` schemes, `mailto:`, `tel:`, `file:` and `blob:`, names something
/// the client can never retrieve.
pub(crate) fn is_fetchable_scheme(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// `address` resolved against `base_url`, without userinfo, when the result is a URL the crawler
/// can fetch. `None` when the address does not resolve at all (it does not parse, or the base
/// cannot take a relative address) or resolves to any scheme [`is_fetchable_scheme`] rejects.
///
/// ~keep Every address crawlberg reports from a page goes through here: the links list, images,
/// ~keep assets, feeds, hreflang alternates, the canonical link, and the Open Graph and Twitter
/// ~keep address fields. Icons apply the same rule in their own check, which also keeps an
/// ~keep inline `data:` icon: it needs no fetch.
pub(crate) fn fetchable_address(address: &str, base_url: &Url) -> Option<Url> {
    crate::net::userinfo::resolve(base_url, address).filter(is_fetchable_scheme)
}

/// Whether the tag's `attr` value equals `expected` in any ASCII case, ignoring ASCII whitespace
/// around the value.
///
/// ~keep HTML compares values such as `name`, `http-equiv` and `type` without case, but tl's
/// ~keep attribute selectors compare them byte for byte and cannot parse the CSS `i` flag. Select
/// ~keep the tag and compare the value here instead. HTML does not trim these values, so a browser
/// ~keep ignores `http-equiv=" refresh "`; the trim is a leniency for pages that add the spaces.
pub(crate) fn attr_eq(tag: &HTMLTag<'_>, attr: &str, expected: &str) -> bool {
    get_attr(tag, attr).is_some_and(|value| value.trim_ascii().eq_ignore_ascii_case(expected))
}

/// The essence of the tag's `type` value, a MIME type, in lowercase: the part before any `;`
/// parameters, without the ASCII whitespace around it.
///
/// ~keep HTML strips ASCII whitespace, form feed included, from a `<script type>` before it reads
/// ~keep the type; the MIME parser alone would keep a form feed and reject the type. `<link type>`
/// ~keep has no such rule and gets the same trim as a leniency, so both `type` attributes read alike.
/// ~keep The trim runs on the essence after the `;` split, not on the whole value, so whitespace
/// ~keep just before `;` is stripped too, which neither HTML nor the MIME rule does. That is more
/// ~keep leniency, in the same spirit as the attribute trim above.
pub(crate) fn mime_essence(tag: &HTMLTag<'_>) -> Option<String> {
    get_attr(tag, "type").map(|value| media_type_essence(&value))
}

/// The essence of a media type, in lowercase: the part of `value` before any `;` parameters,
/// without the ASCII whitespace around it.
///
/// ~keep A media type has no case, and its parameters (`; charset=...`) say nothing about what the
/// ~keep content is. Every decision on a type reads this value, never the text of the header or
/// ~keep of the attribute.
pub(crate) fn media_type_essence(value: &str) -> String {
    let essence = value.split(';').next().unwrap_or_default();
    essence.trim_ascii().to_ascii_lowercase()
}

/// Whether the tag's `rel` value, a space-separated list of tokens, holds `token` in any ASCII case.
pub(crate) fn has_rel(tag: &HTMLTag<'_>, token: &str) -> bool {
    rel_holds(tag, token, |c| c.is_ascii_whitespace())
}

/// Whether the tag's `rel` value holds the link qualifier `nofollow`, `ugc` or `sponsored`, in
/// any ASCII case.
///
/// ~keep Google documents comma-separated qualifiers (`rel="ugc,nofollow"`), so a comma also
/// ~keep separates these three words. Every other `rel` word keeps the HTML whitespace rule.
pub(crate) fn has_link_qualifier(tag: &HTMLTag<'_>, qualifier: &str) -> bool {
    rel_holds(tag, qualifier, |c| c.is_ascii_whitespace() || c == ',')
}

fn rel_holds(tag: &HTMLTag<'_>, token: &str, is_separator: fn(char) -> bool) -> bool {
    get_attr(tag, "rel").is_some_and(|rel| rel.split(is_separator).any(|t| t.eq_ignore_ascii_case(token)))
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
    if raw.contains('&') {
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

#[cfg(feature = "browser-native")]
pub(crate) use charset::decode_document;
pub(crate) use charset::decode_page;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use detection::is_pdf_url;
pub(crate) use detection::{is_binary_content_type, is_binary_url, is_html_content, is_page_content, is_pdf_content};
pub(crate) use extract::HtmlExtraction;
pub(crate) use extract::extract_page_data;
pub(crate) use links::{effective_base_url, extract_links};
pub(crate) use metadata::robots_meta_contents;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use metadata::{detect_meta_refresh, refresh_target};
pub(crate) use raw_text::{MaskedHtml, PageScan, mask_raw_text_markup};
#[cfg(test)]
pub(crate) use real_tags::ATTRIBUTE_LIMIT;
#[cfg(test)]
pub(crate) use real_tags::reads;
pub(crate) use url_targets::sanitize_url_attributes;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_normalizes_newlines_and_nul_as_an_html_parser_does() {
        assert_eq!(decode_attr_value("a\r\nb\rc\nd\0e"), "a\nb\nc\nd\u{FFFD}e");
        assert_eq!(decode_attr_value("a&amp;\r\nb\0"), "a&\nb\u{FFFD}");
        assert_eq!(decode_attr_value("a\r&amp;"), "a\n&");
        assert_eq!(decode_attr_value("a\0&amp;"), "a\u{FFFD}&");
        assert_eq!(decode_attr_value("a\0b"), "a\u{FFFD}b");
        assert_eq!(decode_attr_value("a\rb"), "a\nb");
        assert!(matches!(decode_attr_value("plain value"), Cow::Borrowed("plain value")));
    }

    #[test]
    fn cleaning_an_address_with_nothing_to_remove_borrows_it() {
        assert!(matches!(
            clean_url(Cow::Borrowed("a.html")),
            Some(Cow::Borrowed("a.html"))
        ));
        assert!(matches!(clean_url(Cow::Borrowed(" a.html")), Some(Cow::Owned(ref s)) if s == "a.html"));
        assert_eq!(clean_url(Cow::Borrowed("a\t.html\n")).as_deref(), Some("a.html"));
        assert_eq!(clean_url(Cow::Borrowed("\u{1} \u{C}")), None);
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

    #[test]
    fn a_scheme_is_recognised_as_the_url_parser_reads_it() {
        let cases = [
            ("data", "data:", true),
            ("data", "DATA:image/png;base64,AA", true),
            ("data", "Data:text/plain,x", true),
            ("data", "dAtA:,", true),
            ("data", "data://host/x", true),
            ("data", "data://[x", false),
            ("data", "DATA://h:99999", false),
            ("data", "", false),
            ("data", "data", false),
            ("data", "data/x.png", false),
            ("data", "database.png", false),
            ("data", "data%3Ax", false),
            ("data", "data :x", false),
            ("data", "d\u{e4}ta:x", false),
            ("data", "x-data:y", false),
            ("data", "https://example.com/data:x", false),
            ("javascript", "JAVASCRIPT:alert(1)", true),
            ("mailto", "Mailto:x@example.com", true),
            ("tel", "TEL:+1", true),
            ("tel", "tel://a b", false),
            ("tel", "telx:1", false),
            ("vbscript", "VBScript:msgbox(1)", true),
            ("vbscript", "vbscriptx:1", false),
        ];
        for (scheme, address, expected) in cases {
            assert_eq!(has_scheme(address, scheme), expected, "for {scheme:?} in {address:?}");
        }
    }

    #[test]
    fn a_fetchable_address_loses_its_userinfo() {
        let base = Url::parse("https://example.com/").expect("test URL must parse");
        assert_eq!(
            fetchable_address("http://user:s3cret@example.com/i.png", &base).map(String::from),
            Some("http://example.com/i.png".to_owned())
        );
    }

    #[test]
    fn only_an_address_that_resolves_to_http_or_https_is_fetchable() {
        let base = Url::parse("https://example.com/dir/page.html").expect("test URL must parse");
        let kept = [
            ("i.png", "https://example.com/dir/i.png"),
            ("//cdn.example.com/i.png", "https://cdn.example.com/i.png"),
            ("HTTP://example.com/i.png", "http://example.com/i.png"),
        ];
        for (address, expected) in kept {
            assert_eq!(
                fetchable_address(address, &base).map(String::from).as_deref(),
                Some(expected),
                "for {address:?}"
            );
        }
        let dropped = [
            "file:///etc/passwd",
            "blob:https://example.com/x",
            "ftp://example.com/x",
            "mailto:a@example.com",
            "data:image/png;base64,AA",
            "file://[bad/x",
            "http://[bad/x",
            "data://[a",
        ];
        for address in dropped {
            assert_eq!(fetchable_address(address, &base), None, "for {address:?}");
        }
        let blob_base = Url::parse("blob:https://example.com/b").expect("test URL must parse");
        assert_eq!(
            fetchable_address("i.png", &blob_base),
            None,
            "a relative address under a base that cannot take one does not resolve"
        );
    }
}
